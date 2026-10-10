//! Safe, owned interface for the standalone receiver.
use super::{Complex32, Demodulator, Snapshot};

pub struct Dsp(Demodulator);
impl Dsp {
    pub fn new(rate: f64, offset: f64) -> Result<Self, String> {
        if !rate.is_finite() || !(9600.0..=20_000_000.0).contains(&rate) {
            return Err("IQ sample rate must be 9,600–20,000,000 Hz".into());
        }
        if !offset.is_finite() || offset.abs() + 4200.0 >= rate / 2.0 {
            return Err("Channel lies outside the IQ sample bandwidth".into());
        }
        Ok(Self(Demodulator::new(rate, offset, 0)))
    }
    pub fn process(&mut self, iq: &[Complex32], bits: &mut Vec<u8>) -> Result<Snapshot, String> {
        if iq
            .iter()
            .any(|s| !s.real.is_finite() || !s.imaginary.is_finite())
        {
            return Err("IQ contains non-finite samples".into());
        }
        bits.resize(iq.len() * 2 + 4, 0);
        let count = self.0.process(iq, bits);
        bits.truncate(count);
        Ok(self.0.snapshot())
    }
    pub fn signal(&self) -> (Vec<[f32; 2]>, f64) {
        let mut symbols = [Complex32::default(); 256];
        let (count, evm) = self.0.read_signal(&mut symbols);
        (
            symbols[..count]
                .iter()
                .map(|s| [s.real, s.imaginary])
                .collect(),
            evm,
        )
    }
}
