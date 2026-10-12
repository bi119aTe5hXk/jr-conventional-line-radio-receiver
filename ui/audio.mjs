const SAMPLE_RATE = 8000;
const FRAME_SECONDS = 320 / SAMPLE_RATE;
// Cover synthesis lookahead (160 ms) plus a delayed UI poll (up to 280 ms)
// when the first half-second input burst arrives. Subsequent frames stay on
// the same audio timeline.
const BUFFER_SECONDS = 0.44;
const MAX_GAP_SECONDS = 1;
// Leave room for the initial lead, a half-second TCP burst, polling jitter,
// and gradual clock correction without prematurely dropping valid PCM.
const MAX_BUFFER_SECONDS = 1.5;

// Keep interpolation history and fractional input position across PCM frames.
// One input sample of delay lets each output interpolate between real adjacent
// samples, including the last sample of one frame and the first of the next.
// Resample here so separate Web Audio sources never restart their resampler or
// round independent playback-rate durations at each 40 ms boundary.
class PcmResampler {
  constructor(outputRate) {
    this.outputRate = outputRate;
    this.reset();
  }

  reset() {
    this.position = 0;
    this.previous = undefined;
  }

  length(inputLength, rate) {
    const step = (SAMPLE_RATE * rate) / this.outputRate;
    return Math.max(0, Math.ceil((inputLength - this.position) / step - 1e-9));
  }

  write(pcm, rate, output) {
    const step = (SAMPLE_RATE * rate) / this.outputRate;
    for (let i = 0; i < output.length; i++) {
      const position = this.position + i * step;
      const index = Math.floor(position);
      const fraction = position - index;
      const left = index === 0 ? (this.previous ?? pcm[0]) : pcm[index - 1];
      output[i] = (left + (pcm[index] - left) * fraction) / 32768;
    }
    this.position += output.length * step - pcm.length;
    if (Math.abs(this.position) < 1e-9) this.position = 0;
    this.previous = pcm.at(-1);
  }
}

// Schedule on the audio clock. IPC arrival times may jitter; they must never
// cancel a frame that is still playing. Reset only on an explicit stop/switch
// or a new decoder epoch.
export class AudioPlayer {
  constructor(context, destination) {
    this.context = context;
    this.destination = destination;
    this.resampler = new PcmResampler(context.sampleRate);
    this.sources = new Set();
    this.reset();
  }

  get active() {
    return this.sources.size !== 0;
  }

  reset() {
    for (const node of this.sources) {
      node.onended = null;
      node.stop();
      node.disconnect();
    }
    this.sources.clear();
    this.channel = undefined;
    this.epoch = undefined;
    this.index = -1;
    this.end = undefined;
    this.rate = 1;
    this.resampler.reset();
  }

  enqueue(frames, channel) {
    if (this.context.state !== "running") return;
    let adjusted = false;
    for (const frame of frames) {
      if (frame.channel !== channel) continue;
      if (this.channel !== channel || frame.epoch !== this.epoch) {
        // An older snapshot must not rewind the decoder's timeline.
        if (this.channel === channel && frame.epoch < this.epoch) continue;
        this.reset();
        this.channel = channel;
        this.epoch = frame.epoch;
        adjusted = false;
      }
      if (frame.index <= this.index) continue;
      const now = this.context.currentTime;
      const gap =
        this.index < 0 ? 0 : (frame.index - this.index - 1) * FRAME_SECONDS;
      if (!adjusted && gap === 0 && this.end > now) {
        // Gently track SDR/audio-device clock drift, rather than periodically
        // dropping/restarting audio. Smooth batch jitter and limit pitch change
        // to 0.5%. Apply it to the continuous resampler, never independently
        // to Web Audio nodes whose boundaries may land between output samples.
        const correction = Math.max(
          -0.005,
          Math.min(0.005, (this.end - now - BUFFER_SECONDS) * 0.02),
        );
        this.rate += (1 + correction - this.rate) * 0.02;
      }
      adjusted = true;
      let at = this.end === undefined ? NaN : this.end + gap / this.rate;
      if (!Number.isFinite(at) || at < now || gap > MAX_GAP_SECONDS) {
        // Start/rebuffer only when input really ran out or a call resumed.
        // Keep any queued tail intact even when the next call has a new origin.
        at = Math.max(this.end ?? 0, now + BUFFER_SECONDS);
        this.rate = 1;
        this.resampler.reset();
      }
      if (gap > 0) this.resampler.reset();
      const sampleRate = this.context.sampleRate;
      const startSample = Math.round(at * sampleRate);
      const length = this.resampler.length(frame.pcm.length, this.rate);
      const end = (startSample + length) / sampleRate;
      if (end > now + MAX_BUFFER_SECONDS) {
        // Bound exceptional catch-up bursts without stopping scheduled audio.
        // Dropped live-monitor frames must not later reappear as a timed gap.
        this.index = frame.index;
        continue;
      }
      const buffer = this.context.createBuffer(1, length, sampleRate);
      const data = buffer.getChannelData(0);
      this.resampler.write(frame.pcm, this.rate, data);
      const node = this.context.createBufferSource();
      node.buffer = buffer;
      // Native-rate buffers with integral sample boundaries use the engine's
      // direct copy path. Fractional phase belongs to our continuous stream.
      node.connect(this.destination);
      this.sources.add(node);
      node.onended = () => {
        this.sources.delete(node);
        node.disconnect();
      };
      node.start(startSample / sampleRate);
      this.index = frame.index;
      this.end = end;
    }
  }
}
