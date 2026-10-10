//! Call priority follows the common IQ timeline, never worker scheduling order.
//! One second without an accepted voice frame ends an inferred call.
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

const CALL_GAP_US: u64 = 1_000_000;
// Synchronizer acquisition requires three 40 ms envelopes. Do not decide an
// onset until every live worker could have reported frames at that position.
const ACQUISITION_US: u64 = 120_000;

#[derive(Clone, Copy)]
pub enum Activity {
    Voice(u64),
    Reset(u64),
}
impl Activity {
    fn time(self) -> u64 {
        match self {
            Self::Voice(t) | Self::Reset(t) => t,
        }
    }
}
#[derive(Default)]
struct Call {
    start: u64,
    last: u64,
}
struct Track {
    id: String,
    progress: u64,
    updated: Instant,
    pending: VecDeque<Activity>,
    call: Option<Call>,
}
pub struct Monitor {
    tracks: Vec<Track>,
    finished: bool,
}
impl Monitor {
    pub fn new(ids: impl Iterator<Item = String>) -> Self {
        Self {
            finished: false,
            tracks: ids
                .map(|id| Track {
                    id,
                    progress: 0,
                    updated: Instant::now(),
                    pending: VecDeque::new(),
                    call: None,
                })
                .collect(),
        }
    }
    pub fn update(&mut self, channel: usize, progress: u64, activity: Vec<Activity>) {
        let t = &mut self.tracks[channel];
        t.progress = progress;
        t.updated = Instant::now();
        t.pending.extend(activity);
        self.advance();
    }
    pub fn finish(&mut self) {
        // All workers have drained: no unseen acquisition frames remain. This
        // lets a short call at the very end of an IQ file be selected as well.
        self.finished = true;
        self.advance();
    }
    fn advance(&mut self) {
        let now = Instant::now();
        // A disconnected/stalled worker must not hold another channel forever.
        let live = |t: &Track| now.duration_since(t.updated) < Duration::from_secs(1);
        let watermark = self
            .tracks
            .iter()
            .filter(|t| live(t))
            .map(|t| {
                t.progress
                    .saturating_sub(if self.finished { 0 } else { ACQUISITION_US })
            })
            .min();
        for t in &mut self.tracks {
            if !live(t) {
                t.call = None;
                t.pending.clear();
                continue;
            }
            let Some(watermark) = watermark else { continue };
            while t.pending.front().is_some_and(|e| e.time() <= watermark) {
                match t.pending.pop_front().unwrap() {
                    Activity::Reset(_) => t.call = None,
                    Activity::Voice(time) => {
                        if t.call
                            .as_ref()
                            .is_none_or(|c| time.saturating_sub(c.last) >= CALL_GAP_US)
                        {
                            t.call = Some(Call {
                                start: time,
                                last: time,
                            });
                        } else if let Some(c) = &mut t.call {
                            c.last = time;
                        }
                    }
                }
            }
            if t.call
                .as_ref()
                .is_some_and(|c| watermark.saturating_sub(c.last) >= CALL_GAP_US)
            {
                t.call = None;
            }
        }
    }
    pub fn snapshot(&mut self) -> (Option<String>, Vec<String>) {
        self.advance();
        let mut calls: Vec<_> = self
            .tracks
            .iter()
            .enumerate()
            .filter_map(|(i, t)| t.call.as_ref().map(|c| (c.start, i, t.id.clone())))
            .collect();
        calls.sort_by_key(|c| (c.0, c.1));
        (
            calls.first().map(|c| c.2.clone()),
            calls.into_iter().map(|c| c.2).collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn monitor() -> Monitor {
        Monitor::new(["a", "b", "c"].into_iter().map(String::from))
    }
    fn advance(m: &mut Monitor, time: u64) {
        for i in 0..3 {
            m.update(i, time, vec![]);
        }
    }
    #[test]
    fn end_of_input_releases_onsets_still_inside_acquisition_lookahead() {
        let mut m = monitor();
        m.update(1, 400_000, vec![Activity::Voice(350_000)]);
        advance(&mut m, 400_000);
        assert_eq!(m.snapshot().0, None);
        m.finish();
        assert_eq!(m.snapshot().0.as_deref(), Some("b"));
        for t in &mut m.tracks {
            t.updated = Instant::now() - Duration::from_secs(2);
        }
        assert_eq!(m.snapshot().0, None);
    }
    #[test]
    fn worker_order_does_not_change_fifo_and_finished_waiters_are_skipped() {
        let mut m = monitor();
        m.update(1, 400_000, vec![Activity::Voice(200_000)]);
        assert_eq!(m.snapshot().0, None); // other workers have not caught up
        m.update(0, 400_000, vec![Activity::Voice(100_000)]);
        m.update(2, 400_000, vec![Activity::Voice(250_000)]);
        assert_eq!(m.snapshot().1, ["a", "b", "c"]);
        m.update(0, 1_000_000, vec![Activity::Voice(900_000)]);
        m.update(2, 1_000_000, vec![Activity::Voice(950_000)]);
        advance(&mut m, 1_500_000); // b ended while waiting
        assert_eq!(m.snapshot().1, ["a", "c"]);
        m.update(2, 2_200_000, vec![Activity::Voice(1_600_000)]);
        advance(&mut m, 2_200_000);
        assert_eq!(m.snapshot().0.as_deref(), Some("c"));
        advance(&mut m, 3_000_000);
        assert_eq!(m.snapshot().0, None);
    }
    #[test]
    fn short_gaps_keep_priority_but_new_calls_and_resets_rejoin_at_the_end() {
        let mut m = monitor();
        m.update(
            0,
            400_000,
            vec![Activity::Voice(0), Activity::Voice(200_000)],
        );
        m.update(1, 400_000, vec![Activity::Voice(100_000)]);
        advance(&mut m, 500_000);
        assert_eq!(m.snapshot().0.as_deref(), Some("a"));
        m.update(
            0,
            800_000,
            vec![Activity::Reset(500_000), Activity::Voice(600_000)],
        );
        advance(&mut m, 800_000);
        assert_eq!(m.snapshot().1, ["b", "a"]);
        m.update(1, 2_000_000, vec![Activity::Voice(1_200_000)]);
        m.update(
            0,
            2_000_000,
            vec![Activity::Voice(1_000_000), Activity::Voice(1_700_000)],
        );
        advance(&mut m, 2_000_000);
        assert_eq!(m.snapshot().1, ["a", "b"]);
    }
}
