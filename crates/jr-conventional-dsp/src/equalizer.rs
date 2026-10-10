//! Half-symbol-spaced waveform and timing front end.
//!
//! Keep the center tap fixed and let the following adaptive sequence detector
//! model the delayed paths. CMA adaptation degraded recovery near fading nulls
//! in the capture and independent delayed-path experiments. Retain the window
//! and timing derivative so the validated sample alignment remains unchanged.
use crate::dsp::Complex32;

pub const TAPS: usize = 21;
const ADAPTATION: f32 = 0.0;

pub struct Equalizer {
    weights: [Complex32; TAPS],
}

impl Default for Equalizer {
    fn default() -> Self {
        let mut weights = [Complex32::default(); TAPS];
        weights[TAPS / 2].real = 1.0;
        Self { weights }
    }
}

impl Equalizer {
    pub fn process(
        &mut self,
        input: &[Complex32; TAPS],
        derivatives: &[Complex32; TAPS],
        reliable: bool,
    ) -> (Complex32, f32) {
        let mut value = Complex32::default();
        let mut derivative = Complex32::default();
        let mut energy = 0.0;
        for ((w, x), dx) in self.weights.iter().zip(input).zip(derivatives) {
            value.real += w.real * x.real - w.imaginary * x.imaginary;
            value.imaginary += w.real * x.imaginary + w.imaginary * x.real;
            derivative.real += w.real * dx.real - w.imaginary * dx.imaginary;
            derivative.imaginary += w.real * dx.imaginary + w.imaginary * dx.real;
            energy += x.real * x.real + x.imaginary * x.imaginary;
        }
        let power = value.real * value.real + value.imaginary * value.imaginary;
        if reliable && energy > 0.01 && energy.is_finite() && power.is_finite() {
            let step = ADAPTATION * (1.0 - power).clamp(-2.0, 1.0) / energy;
            for (w, x) in self.weights.iter_mut().zip(input) {
                w.real += step * (value.real * x.real + value.imaginary * x.imaginary);
                w.imaginary += step * (value.imaginary * x.real - value.real * x.imaginary);
            }
            let norm: f32 = self
                .weights
                .iter()
                .map(|w| w.real * w.real + w.imaginary * w.imaginary)
                .sum();
            if norm > 16.0 {
                let gain = 4.0 / norm.sqrt();
                for w in &mut self.weights {
                    w.real *= gain;
                    w.imaginary *= gain;
                }
            }
        }
        // Recover timing from the selected waveform and its derivative;
        // the following sequence detector handles ISI. Coast during a fade.
        (
            value,
            if reliable {
                (0.5 * (value.real * derivative.real + value.imaginary * derivative.imaginary))
                    .clamp(-1.0, 1.0)
            } else {
                0.0
            },
        )
    }
}
