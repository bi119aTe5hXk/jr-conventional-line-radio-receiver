//! Acquisition and cadence follow the reference's 384-bit radio envelope.
use std::collections::VecDeque;

#[derive(Clone, Debug)]
pub struct Frame {
    pub bit_offset: u64,
    pub sync_matched: bool,
    pub marker_errors: u32,
    pub received: [u8; 48],
}

#[derive(Default)]
pub struct Synchronizer {
    bits: VecDeque<u8>,
    origin: u64,
    failures: u8,
    pub locked: bool,
    pub frames: u64,
    pub synchronized_frames: u64,
}
impl Synchronizer {
    fn markers(&self, offset: usize) -> (bool, u32) {
        let mut matching = 0;
        let mut errors = 0;
        for (m, expected) in [0x3d22u16, 0xe867, 0xe867].iter().enumerate() {
            let mut value = 0u16;
            for i in 0..16 {
                value = (value << 1) | self.bits[offset + 128 * m + i] as u16;
            }
            matching += usize::from(value & 0x3fff == expected & 0x3fff);
            errors += (value ^ expected).count_ones();
        }
        (matching >= 2, errors)
    }
    fn drop_bits(&mut self, count: usize) {
        self.bits.drain(..count);
        self.origin += count as u64;
    }
    pub fn push(&mut self, dibit: u8, output: &mut Vec<Frame>) {
        assert!(dibit < 4);
        self.bits.push_back(dibit >> 1);
        self.bits.push_back(dibit & 1);
        if !self.locked && self.bits.len() >= 1152 {
            let mut pos = 0;
            while pos + 1152 <= self.bits.len() {
                if self.markers(pos).0 && self.markers(pos + 384).0 && self.markers(pos + 768).0 {
                    self.locked = true;
                    self.failures = 0;
                    break;
                }
                pos += 1;
            }
            self.drop_bits(pos);
        }
        while self.locked && self.bits.len() >= 384 {
            let (sync_matched, marker_errors) = self.markers(0);
            let mut received = [0; 48];
            for i in 0..384 {
                received[i / 8] |= self.bits[i] << (7 - i % 8);
            }
            if sync_matched {
                self.failures = 0;
                self.synchronized_frames += 1;
            } else {
                self.failures += 1;
            }
            self.frames += 1;
            output.push(Frame {
                bit_offset: self.origin,
                sync_matched,
                marker_errors,
                received,
            });
            self.drop_bits(384);
            if self.failures >= 3 {
                self.locked = false;
            }
        }
    }
    pub fn packed(&mut self, bytes: &[u8]) -> Vec<Frame> {
        let mut frames = Vec::new();
        for byte in bytes {
            for shift in [6, 4, 2, 0] {
                self.push((byte >> shift) & 3, &mut frames);
            }
        }
        frames
    }
}
