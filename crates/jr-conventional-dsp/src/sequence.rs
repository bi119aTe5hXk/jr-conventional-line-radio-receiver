//! Adaptive short-memory sequence detection with per-survivor channel estimates.
//!
//! Pi/4 alternation has already been removed. Each of the 64 states remembers
//! three QPSK symbols and a four-tap complex channel. Branch metrics use amplitude
//! as well as phase. No framing patterns or payload values are known here.
use crate::dsp::Complex32;

const STATES: usize = 64;
const DEPTH: usize = 32;
// Normalized LMS: 0.32 / four unit-energy symbols. Track the delayed paths
// directly, with the preceding CMA tap weights held fixed.
const CHANNEL_STEP: f32 = 0.08;
const LEVEL: f32 = std::f32::consts::FRAC_1_SQRT_2;
const SYMBOLS: [Complex32; 4] = [
    Complex32 {
        real: LEVEL,
        imaginary: LEVEL,
    },
    Complex32 {
        real: -LEVEL,
        imaginary: LEVEL,
    },
    Complex32 {
        real: -LEVEL,
        imaginary: -LEVEL,
    },
    Complex32 {
        real: LEVEL,
        imaginary: -LEVEL,
    },
];

pub struct Sequence {
    costs: [f32; STATES],
    channels: [[Complex32; 4]; STATES],
    history: [[u8; STATES]; DEPTH],
    write: usize,
    filled: usize,
    previous: Option<usize>,
    equalized: Option<Complex32>,
}

impl Default for Sequence {
    fn default() -> Self {
        let mut channel = [Complex32::default(); 4];
        channel[0].real = 1.0;
        Self {
            costs: [0.0; STATES],
            channels: [channel; STATES],
            history: [[0; STATES]; DEPTH],
            write: 0,
            filled: 0,
            previous: None,
            equalized: None,
        }
    }
}

impl Sequence {
    /// Current observation after survivor-channel ISI and complex-gain removal.
    /// This passive, unsliced estimate uses the same pre-update channel as the
    /// branch metric; it must not feed decisions or adaptation back into the DSP.
    pub fn equalized(&self) -> Option<Complex32> {
        self.equalized
    }

    pub fn process(&mut self, input: Complex32, reliable: bool) -> (Option<u8>, f32, f32) {
        let mut costs = [f32::INFINITY; STATES];
        let mut parents = [0; STATES];
        let mut predictions = [Complex32::default(); STATES];
        for prior in 0..STATES {
            for (current, &symbol) in SYMBOLS.iter().enumerate() {
                let symbols = [
                    symbol,
                    SYMBOLS[prior % 4],
                    SYMBOLS[(prior / 4) % 4],
                    SYMBOLS[prior / 16],
                ];
                let mut prediction = Complex32::default();
                for (h, s) in self.channels[prior].iter().zip(symbols) {
                    prediction.real += h.real * s.real - h.imaginary * s.imaginary;
                    prediction.imaginary += h.real * s.imaginary + h.imaginary * s.real;
                }
                // Missing observations advance the trellis without adding
                // evidence or training the channel on amplified fade noise.
                let error = if reliable {
                    ((input.real - prediction.real).powi(2)
                        + (input.imaginary - prediction.imaginary).powi(2))
                    .min(16.0)
                } else {
                    0.0
                };
                let cost = self.costs[prior] + error;
                let state = current + 4 * (prior % 16);
                if cost < costs[state] {
                    costs[state] = cost;
                    parents[state] = prior as u8;
                    predictions[state] = prediction;
                }
            }
        }
        // The trellis always has 64 states, so selection is non-fallible.
        // Seed with state 0 and retain the first minimum on ties, preserving
        // min_by's ordering (including non-finite costs) without an unwrap.
        let mut best = 0;
        for (state, cost) in costs.iter().enumerate().skip(1) {
            if cost.total_cmp(&costs[best]).is_lt() {
                best = state;
            }
        }
        let best_prediction = predictions[best];
        let error = Complex32 {
            real: input.real - best_prediction.real,
            imaginary: input.imaginary - best_prediction.imaginary,
        };
        let phase = (input.imaginary * best_prediction.real
            - input.real * best_prediction.imaginary)
            .atan2(input.real * best_prediction.real + input.imaginary * best_prediction.imaginary);
        // The detector emits hard dibits after traceback, not a filtered IQ
        // stream. For diagnostics, cancel the delayed paths of the current best
        // survivor from the received observation and divide by its main tap.
        // Do this BEFORE adaptation so the current noise/error is not fitted away.
        let prior = parents[best] as usize;
        let channel = &self.channels[prior];
        let main = channel[0];
        let power = main.real * main.real + main.imaginary * main.imaginary;
        self.equalized = if reliable && power.is_finite() && power > 1e-6 {
            let mut value = input;
            for (h, s) in channel[1..].iter().zip([
                SYMBOLS[prior % 4],
                SYMBOLS[(prior / 4) % 4],
                SYMBOLS[prior / 16],
            ]) {
                value.real -= h.real * s.real - h.imaginary * s.imaginary;
                value.imaginary -= h.real * s.imaginary + h.imaginary * s.real;
            }
            let value = Complex32 {
                real: (value.real * main.real + value.imaginary * main.imaginary) / power,
                imaginary: (value.imaginary * main.real - value.real * main.imaginary) / power,
            };
            (value.real.is_finite() && value.imaginary.is_finite()).then_some(value)
        } else {
            None
        };
        let mut channels = self.channels;
        for state in 0..STATES {
            let prior = parents[state] as usize;
            let symbols = [
                SYMBOLS[state % 4],
                SYMBOLS[prior % 4],
                SYMBOLS[(prior / 4) % 4],
                SYMBOLS[prior / 16],
            ];
            let error = Complex32 {
                real: input.real - predictions[state].real,
                imaginary: input.imaginary - predictions[state].imaginary,
            };
            channels[state] = self.channels[prior];
            if reliable {
                for (h, s) in channels[state].iter_mut().zip(symbols) {
                    h.real += CHANNEL_STEP * (error.real * s.real + error.imaginary * s.imaginary);
                    h.imaginary +=
                        CHANNEL_STEP * (error.imaginary * s.real - error.real * s.imaginary);
                }
                let norm: f32 = channels[state]
                    .iter()
                    .map(|h| h.real * h.real + h.imaginary * h.imaginary)
                    .sum();
                if norm > 16.0 {
                    let gain = 4.0 / norm.sqrt();
                    for h in &mut channels[state] {
                        h.real *= gain;
                        h.imaginary *= gain;
                    }
                }
            }
        }
        self.channels = channels;
        // Common-offset removal bounds accumulated costs for an unlimited stream.
        self.costs = costs.map(|value| value - costs[best]);
        self.history[self.write] = parents;
        self.write = (self.write + 1) % DEPTH;
        self.filled = (self.filled + 1).min(DEPTH);
        let mut output = None;
        if self.filled == DEPTH {
            let mut state = best;
            for back in 0..DEPTH - 1 {
                state = self.history[(self.write + DEPTH - 1 - back) % DEPTH][state] as usize;
            }
            let current = state % 4;
            output = Some(
                self.previous
                    .map_or(0, |last| [0, 1, 3, 2][(current + 4 - last) % 4]),
            );
            self.previous = Some(current);
        }
        (
            output,
            if reliable { phase } else { 0.0 },
            if reliable {
                error.real * error.real + error.imaginary * error.imaginary
            } else {
                1.0
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn symbols(count: usize) -> Vec<usize> {
        let mut state = 0x8123_4567_u32;
        (0..count)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                (state >> 30) as usize
            })
            .collect()
    }

    #[test]
    fn diagnostic_cancels_all_delayed_paths_and_complex_gain_without_slicing() {
        let mut detector = Sequence::default();
        let prior = 2 + 4 + 16 * 3;
        detector.costs.fill(f32::INFINITY);
        detector.costs[prior] = 0.0;
        let channel = [
            Complex32 {
                real: 0.6,
                imaginary: 0.3,
            },
            Complex32 {
                real: 0.4,
                imaginary: -0.2,
            },
            Complex32 {
                real: -0.2,
                imaginary: 0.15,
            },
            Complex32 {
                real: 0.1,
                imaginary: 0.05,
            },
        ];
        detector.channels[prior] = channel;
        // A noisy observation must remain noisy after channel correction.
        let expected = Complex32 {
            real: SYMBOLS[1].real + 0.12,
            imaginary: SYMBOLS[1].imaginary - 0.07,
        };
        let mut received = Complex32::default();
        for (h, s) in channel
            .iter()
            .zip([expected, SYMBOLS[2], SYMBOLS[1], SYMBOLS[3]])
        {
            received.real += h.real * s.real - h.imaginary * s.imaginary;
            received.imaginary += h.real * s.imaginary + h.imaginary * s.real;
        }
        assert!(detector.equalized().is_none());
        let (dibit, _, _) = detector.process(received, true);
        assert!(
            dibit.is_none(),
            "diagnostics do not wait for hard traceback"
        );
        let actual = detector.equalized().unwrap();
        assert!((actual.real - expected.real).abs() < 1e-6, "{actual:?}");
        assert!(
            (actual.imaginary - expected.imaginary).abs() < 1e-6,
            "{actual:?}"
        );

        detector.process(received, false);
        assert!(
            detector.equalized().is_none(),
            "a fade must clear the observation"
        );
    }

    #[test]
    fn diagnostic_does_not_invert_an_unusable_main_tap() {
        for gain in [0.0, 1e-4] {
            let mut detector = Sequence::default();
            for channel in &mut detector.channels {
                channel[0].real = gain;
            }
            detector.process(SYMBOLS[0], true);
            assert!(detector.equalized().is_none());
        }
    }

    #[test]
    fn random_symbols_survive_isi_and_traceback_across_a_missing_interval() {
        let transmitted = symbols(6000);
        let mut detector = Sequence::default();
        let mut recovered = Vec::new();
        for (i, &q) in transmitted.iter().enumerate() {
            let previous = SYMBOLS[transmitted[i.saturating_sub(1)]];
            let sample = Complex32 {
                real: (SYMBOLS[q].real + 0.6 * previous.real) / 1.1661904,
                imaginary: (SYMBOLS[q].imaginary + 0.6 * previous.imaginary) / 1.1661904,
            };
            let reliable = !(2000..2240).contains(&i);
            let (value, _, _) = detector.process(
                if reliable {
                    sample
                } else {
                    Complex32::default()
                },
                reliable,
            );
            if let Some(dibit) = value {
                recovered.push(dibit);
            }
            assert!(detector
                .costs
                .iter()
                .all(|cost| cost.is_finite() && *cost >= 0.0));
        }
        assert_eq!(recovered.len(), transmitted.len() - DEPTH + 1);
        for i in (500..1900).chain(2500..recovered.len()) {
            assert_eq!(
                recovered[i],
                [0, 1, 3, 2][(transmitted[i] + 4 - transmitted[i - 1]) % 4],
                "symbol {i}"
            );
        }
    }

    #[test]
    fn long_silence_preserves_bounded_channel_state_and_recovers() {
        let mut detector = Sequence::default();
        for _ in 0..10000 {
            detector.process(Complex32::default(), false);
        }
        assert!(detector.channels.iter().all(|h| h[0].real == 1.0));
        let transmitted = symbols(1000);
        let output: Vec<_> = transmitted
            .iter()
            .filter_map(|&q| detector.process(SYMBOLS[q], true).0)
            .collect();
        for (i, &dibit) in output.iter().enumerate().skip(100) {
            let at = i + 1 - DEPTH;
            assert_eq!(
                dibit,
                [0, 1, 3, 2][(transmitted[at] + 4 - transmitted[at - 1]) % 4]
            );
        }
    }
}
