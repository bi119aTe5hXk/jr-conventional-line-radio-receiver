//! One acquisition stream, shared immutable IQ blocks, and independent channel workers.
//! Bounded queues cannot silently concatenate radio data across lost samples.
use crate::{
    monitor::{Activity, Monitor},
    pipeline::{validate_channels, Audio, ChannelConfig, Pipeline, Status},
    recording::{OutputOptions, Recorder, RecordingStatus},
    source::{RadioConfig, Read},
    wave::IqWave,
    Complex32,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, TrySendError},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Input {
    Sdr { radio: RadioConfig },
    RtlTcp { radio: crate::rtl_tcp::Config },
    Wave { path: String, center_hz: f64 },
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Config {
    pub input: Input,
    pub channels: Vec<ChannelConfig>,
}
#[derive(Clone, Serialize)]
pub struct ChannelStatus {
    pub config: ChannelConfig,
    pub status: Status,
    pub dropped_blocks: u64,
    pub audio_drops: u64,
    pub error: Option<String>,
}
struct Channel {
    status: ChannelStatus,
    audio: VecDeque<(u64, Audio)>,
    progress_us: u64,
}
#[derive(Clone, Serialize)]
pub struct Overview {
    pub running: bool,
    pub state: String,
    pub error: Option<String>,
    pub sample_rate: f64,
    pub center_hz: f64,
    pub source_overflows: u64,
    pub progress: f64,
    pub channels: Vec<ChannelStatus>,
    pub recording: RecordingStatus,
    pub selected_channel: Option<String>,
    pub active_calls: Vec<String>,
}
struct Shared {
    state: String,
    error: Option<String>,
    rate: f64,
    center: f64,
    overflows: u64,
    progress: f64,
}
pub struct Session {
    stop: Arc<AtomicBool>,
    shared: Arc<Mutex<Shared>>,
    channels: Vec<Arc<Mutex<Channel>>>,
    thread: Option<JoinHandle<()>>,
    recorder: Arc<Recorder>,
    monitor: Arc<Mutex<Monitor>>,
}
struct Block {
    sequence: u64,
    start_sample: u64,
    iq: Arc<[Complex32]>,
}
enum Source {
    Wave(IqWave),
    RtlTcp(crate::rtl_tcp::RtlTcp),
    #[cfg(feature = "soapy")]
    Sdr(crate::source::Sdr),
}
impl Source {
    fn open(input: &Input, stop: &AtomicBool) -> Result<(Self, f64, f64), String> {
        match input {
            Input::RtlTcp { radio } => {
                let s = crate::rtl_tcp::RtlTcp::open(radio, stop)?;
                Ok((
                    Self::RtlTcp(s),
                    radio.sample_rate as f64,
                    radio.center_hz as f64,
                ))
            }
            Input::Wave { path, center_hz } => {
                let w = IqWave::open(path).map_err(|e| e.to_string())?;
                let rate = w.sample_rate;
                Ok((Self::Wave(w), rate, *center_hz))
            }
            Input::Sdr { radio } => {
                #[cfg(feature = "soapy")]
                {
                    let s = crate::source::Sdr::open(radio)?;
                    let (rate, center) = (s.sample_rate, s.center_hz);
                    Ok((Self::Sdr(s), rate, center))
                }
                #[cfg(not(feature = "soapy"))]
                {
                    let _ = radio;
                    Err("SoapySDR was disabled at build time".into())
                }
            }
        }
    }
    fn read(&mut self, buf: &mut [Complex32]) -> Result<Option<Read>, String> {
        match self {
            Self::RtlTcp(s) => s.read(buf).map(Some),
            Self::Wave(w) => {
                let n = w.read(buf).map_err(|e| e.to_string())?;
                Ok(if n == 0 { None } else { Some(Read::Samples(n)) })
            }
            #[cfg(feature = "soapy")]
            Self::Sdr(s) => s.read(buf).map(Some),
        }
    }
}
impl Session {
    pub fn start(config: Config) -> Result<Self, String> {
        if config.channels.is_empty() || config.channels.len() > 64 {
            return Err("Select 1–64 channels".into());
        }
        let stop = Arc::new(AtomicBool::new(false));
        let shared = Arc::new(Mutex::new(Shared {
            state: "starting".into(),
            error: None,
            rate: 0.0,
            center: 0.0,
            overflows: 0,
            progress: 0.0,
        }));
        let channels: Vec<_> = config
            .channels
            .iter()
            .map(|c| {
                Arc::new(Mutex::new(Channel {
                    status: ChannelStatus {
                        config: c.clone(),
                        status: Status {
                            input_dbfs: -120.0,
                            ..Status::default()
                        },
                        dropped_blocks: 0,
                        audio_drops: 0,
                        error: None,
                    },
                    audio: VecDeque::new(),
                    progress_us: 0,
                }))
            })
            .collect();
        let monitor = Arc::new(Mutex::new(Monitor::new(
            config.channels.iter().map(|c| c.id.clone()),
        )));
        let call_monitor = monitor.clone();
        let recorder = Arc::new(Recorder::default());
        let recording = recorder.clone();
        let (cancel, state, workers) = (stop.clone(), shared.clone(), channels.clone());
        let handle = thread::Builder::new()
            .name("iq-acquisition".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run(config, &cancel, &state, &workers, &recording, &call_monitor)
                }));
                let mut s = state.lock().unwrap();
                // Serialize automatic finalization with starting a recording.
                // Workers have already flushed their last synthesized audio.
                call_monitor.lock().unwrap().finish();
                recording.stop();
                match result {
                    Ok(Ok(())) => {
                        s.state = if cancel.load(Ordering::Relaxed) {
                            "stopped"
                        } else {
                            "completed"
                        }
                        .into()
                    }
                    Ok(Err(_))
                        if cancel.load(Ordering::Relaxed)
                            && workers
                                .iter()
                                .all(|c| c.lock().unwrap().status.error.is_none()) =>
                    {
                        s.state = "stopped".into();
                    }
                    Ok(Err(e)) => {
                        s.state = "failed".into();
                        s.error = Some(e);
                    }
                    Err(_) => {
                        s.state = "failed".into();
                        s.error = Some("Receiver worker panicked; restart reception".into());
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Self {
            stop,
            shared,
            channels,
            thread: Some(handle),
            recorder,
            monitor,
        })
    }
    pub fn snapshot(&self, selected: Option<&str>) -> (Overview, Vec<Audio>) {
        self.snapshot_with_monitor(selected, false)
    }
    pub fn snapshot_with_monitor(
        &self,
        selected: Option<&str>,
        automatic: bool,
    ) -> (Overview, Vec<Audio>) {
        let s = self.shared.lock().unwrap();
        let (preferred, active_calls) = self.monitor.lock().unwrap().snapshot();
        let selected = if automatic {
            preferred.as_deref().or(selected)
        } else {
            selected
        };
        let mut audio = Vec::new();
        let channels = self
            .channels
            .iter()
            .map(|c| {
                let mut c = c.lock().unwrap();
                let mut status = c.status.clone();
                while c
                    .audio
                    .front()
                    .is_some_and(|(time, _)| time.saturating_add(600_000) < c.progress_us)
                {
                    c.audio.pop_front();
                }
                if selected == Some(status.config.id.as_str()) {
                    audio.extend(c.audio.drain(..).map(|(_, a)| a));
                } else {
                    status.status.messages.clear();
                    status.status.constellation.clear();
                }
                status
            })
            .collect();
        (
            Overview {
                running: matches!(s.state.as_str(), "starting" | "receiving"),
                state: s.state.clone(),
                error: s.error.clone(),
                sample_rate: s.rate,
                center_hz: s.center,
                source_overflows: s.overflows,
                progress: s.progress,
                channels,
                recording: self.recorder.snapshot(),
                selected_channel: selected.map(String::from),
                active_calls,
            },
            audio,
        )
    }
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.thread.take() {
            let _ = h.join();
        }
    }
    pub fn start_recording(&self, directory: &std::path::Path) -> Result<RecordingStatus, String> {
        self.start_recording_with_options(directory, OutputOptions::default())
    }
    pub fn start_recording_with_options(
        &self,
        directory: &std::path::Path,
        options: OutputOptions,
    ) -> Result<RecordingStatus, String> {
        let s = self.shared.lock().unwrap();
        if s.state != "receiving" || self.stop.load(Ordering::Relaxed) {
            return Err("受信を開始し、接続が完了してから録音してください。".into());
        }
        let configs: Vec<_> = self
            .channels
            .iter()
            .map(|c| c.lock().unwrap().status.config.clone())
            .collect();
        self.recorder
            .start_with_options(directory, &configs, options)
    }
    pub fn stop_recording(&self) -> RecordingStatus {
        self.recorder.stop()
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        self.stop();
    }
}
fn run(
    config: Config,
    stop: &Arc<AtomicBool>,
    shared: &Arc<Mutex<Shared>>,
    channels: &[Arc<Mutex<Channel>>],
    recorder: &Arc<Recorder>,
    monitor: &Arc<Mutex<Monitor>>,
) -> Result<(), String> {
    if let Input::RtlTcp { radio } = &config.input {
        radio.validate()?;
        validate_channels(
            &config.channels,
            radio.sample_rate as f64,
            radio.center_hz as f64,
        )?;
    }
    let (mut source, rate, center) = Source::open(&config.input, stop)?;
    validate_channels(&config.channels, rate, center)?;
    let total = match &source {
        Source::Wave(w) => Some(w.samples),
        _ => None,
    };
    // Construct every pipeline before starting threads: no partial receiver on invalid input.
    // TCP delivers bursts at network packet boundaries. At 240 kS/s this holds
    // at most 1.28 seconds, still bounded and with explicit loss/reset on overrun.
    let queue_capacity = if matches!(config.input, Input::RtlTcp { .. }) {
        32
    } else {
        8
    };
    let pipelines = config
        .channels
        .into_iter()
        .map(|c| Pipeline::new(c, rate, center))
        .collect::<Result<Vec<_>, _>>()?;
    let mut senders = Vec::new();
    let mut handles = Vec::new();
    for (channel_index, (mut pipe, ch)) in pipelines
        .into_iter()
        .zip(channels.iter().cloned())
        .enumerate()
    {
        let (tx, rx) = mpsc::sync_channel::<Block>(queue_capacity);
        let cancel = stop.clone();
        let recording = recorder.clone();
        let monitor = monitor.clone();
        let h = thread::spawn(move || {
            let result =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<(), String> {
                    let mut expected = 0;
                    let mut segment_start = 0;
                    let mut audio_origin_us = 0;
                    let channel_id = pipe.config.id.clone();
                    let mut last = Instant::now();
                    loop {
                        if cancel.load(Ordering::Relaxed) {
                            break;
                        }
                        let block = match rx.recv_timeout(Duration::from_millis(100)) {
                            Ok(b) => b,
                            Err(mpsc::RecvTimeoutError::Timeout) => continue,
                            Err(_) => break,
                        };
                        let to_us = |samples: u64| (samples as f64 / rate * 1_000_000.0) as u64;
                        let mut activity = Vec::new();
                        if block.sequence != expected {
                            pipe.discontinuity()?;
                            segment_start = block.start_sample;
                            activity.push(Activity::Reset(to_us(segment_start)));
                            ch.lock().unwrap().audio.clear();
                        }
                        expected = block.sequence + 1;
                        let mut epoch = pipe.status.epoch;
                        let audio = pipe.process_observed(&block.iq, |event| {
                            let time =
                                to_us(segment_start) + event.frame.bit_offset * 1_000_000 / 9600;
                            audio_origin_us = time.saturating_sub(event.index as u64 * 40_000);
                            if event.epoch != epoch {
                                activity.push(Activity::Reset(time));
                                epoch = event.epoch;
                            }
                            if event.corrected_bits.is_some() {
                                activity.push(Activity::Voice(time));
                            }
                            recording.push_frame(&channel_id, event);
                        })?;
                        monitor.lock().unwrap().update(
                            channel_index,
                            to_us(block.start_sample + block.iq.len() as u64),
                            activity,
                        );
                        for a in &audio {
                            recording.push(a);
                        }
                        let mut c = ch.lock().unwrap();
                        c.progress_us = to_us(block.start_sample + block.iq.len() as u64);
                        if c.status.status.epoch != pipe.status.epoch {
                            c.audio.clear();
                            c.status.status.epoch = pipe.status.epoch;
                        }
                        for a in audio {
                            if c.audio.len() == 125 {
                                c.audio.pop_front();
                                c.status.audio_drops += 1;
                            }
                            c.audio
                                .push_back((audio_origin_us + a.index as u64 * 40_000, a));
                        }
                        if last.elapsed() >= Duration::from_millis(150) {
                            c.status.status = pipe.snapshot();
                            last = Instant::now();
                        }
                    }
                    let tail = pipe.flush()?;
                    for a in &tail {
                        recording.push(a);
                    }
                    let mut c = ch.lock().unwrap();
                    for a in tail {
                        if c.audio.len() == 125 {
                            c.audio.pop_front();
                            c.status.audio_drops += 1;
                        }
                        c.audio
                            .push_back((audio_origin_us + a.index as u64 * 40_000, a));
                    }
                    c.status.status = pipe.snapshot();
                    Ok(())
                }));
            let error = match result {
                Ok(Ok(())) => None,
                Ok(Err(e)) => Some(e),
                Err(_) => Some("Channel worker panicked; restart reception".into()),
            };
            if let Some(e) = error {
                ch.lock().unwrap().status.error = Some(e);
                cancel.store(true, Ordering::Relaxed);
            }
        });
        senders.push(tx);
        handles.push(h);
    }
    {
        let mut s = shared.lock().unwrap();
        s.state = "receiving".into();
        s.rate = rate;
        s.center = center;
    }
    let mut sequence = 0;
    let mut samples = 0u64;
    let start = Instant::now();
    let mut buf = vec![Complex32::default(); (rate * 0.04).round().clamp(384.0, 65536.0) as usize];
    let result = (|| -> Result<(), String> {
        while !stop.load(Ordering::Relaxed) {
            match source.read(&mut buf)? {
                None => break,
                Some(Read::Timeout) => continue,
                Some(Read::Discontinuity) => {
                    sequence += 1;
                    shared.lock().unwrap().overflows += 1;
                    continue;
                }
                Some(Read::Samples(n)) => {
                    samples += n as u64;
                    let iq: Arc<[Complex32]> = Arc::from(&buf[..n]);
                    for (i, tx) in senders.iter().enumerate() {
                        match tx.try_send(Block {
                            sequence,
                            start_sample: samples - n as u64,
                            iq: iq.clone(),
                        }) {
                            Ok(()) => {}
                            Err(TrySendError::Full(_)) => {
                                channels[i].lock().unwrap().status.dropped_blocks += 1
                            }
                            Err(TrySendError::Disconnected(_)) => {
                                return Err("A channel worker stopped; restart reception".into())
                            }
                        }
                    }
                    sequence += 1;
                    if let Some(total) = total {
                        shared.lock().unwrap().progress = samples as f64 / total.max(1) as f64;
                        let target = Duration::from_secs_f64(samples as f64 / rate);
                        while !stop.load(Ordering::Relaxed) && start.elapsed() < target {
                            thread::sleep(
                                (target - start.elapsed().min(target))
                                    .min(Duration::from_millis(20)),
                            );
                        }
                    }
                }
            }
        }
        Ok(())
    })();
    drop(senders);
    for h in handles {
        if h.join().is_err() {
            return Err("Channel thread could not finish".into());
        }
    }
    if let Some(e) = channels
        .iter()
        .find_map(|c| c.lock().unwrap().status.error.clone())
    {
        return Err(e);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn automatic_snapshot_delivers_chosen_audio_and_manual_selection_remains_authoritative() {
        let configs = crate::pipeline::presets("east")[..2].to_vec();
        let monitor = Arc::new(Mutex::new(Monitor::new(
            configs.iter().map(|c| c.id.clone()),
        )));
        monitor
            .lock()
            .unwrap()
            .update(1, 400_000, vec![Activity::Voice(100_000)]);
        monitor
            .lock()
            .unwrap()
            .update(0, 400_000, vec![Activity::Voice(200_000)]);
        let session = Session {
            stop: Arc::new(AtomicBool::new(false)),
            shared: Arc::new(Mutex::new(Shared {
                state: "receiving".into(),
                error: None,
                rate: 240000.0,
                center: 352603125.0,
                overflows: 0,
                progress: 0.0,
            })),
            channels: configs
                .iter()
                .map(|c| {
                    Arc::new(Mutex::new(Channel {
                        status: ChannelStatus {
                            config: c.clone(),
                            status: Status::default(),
                            dropped_blocks: 0,
                            audio_drops: 0,
                            error: None,
                        },
                        progress_us: 400_000,
                        audio: VecDeque::from([(
                            200_000,
                            Audio {
                                channel: c.id.clone(),
                                epoch: 0,
                                index: 0,
                                pcm: vec![42; 320],
                            },
                        )]),
                    }))
                })
                .collect(),
            thread: None,
            recorder: Arc::new(Recorder::default()),
            monitor,
        };
        let (status, audio) = session.snapshot_with_monitor(Some(&configs[0].id), true);
        assert_eq!(
            status.selected_channel.as_deref(),
            Some(configs[1].id.as_str())
        );
        assert_eq!(audio.len(), 1);
        assert_eq!(audio[0].channel, configs[1].id);
        // Manual selection uses the exact requested channel even during calls.
        let (status, audio) = session.snapshot_with_monitor(Some(&configs[0].id), false);
        assert_eq!(
            status.selected_channel.as_deref(),
            Some(configs[0].id.as_str())
        );
        assert_eq!(audio[0].channel, configs[0].id);
        assert_eq!(
            session
                .snapshot_with_monitor(Some(&configs[0].id), true)
                .0
                .selected_channel
                .as_deref(),
            Some(configs[1].id.as_str())
        );
        // Switching to a silent channel cannot replay a long-completed call.
        let mut c = session.channels[0].lock().unwrap();
        c.audio.push_back((
            200_000,
            Audio {
                channel: configs[0].id.clone(),
                epoch: 0,
                index: 1,
                pcm: vec![42; 320],
            },
        ));
        c.progress_us = 2_000_000;
        drop(c);
        assert!(session
            .snapshot_with_monitor(Some(&configs[0].id), false)
            .1
            .is_empty());
    }
}
