use super::Format;
#[derive(Clone)]
pub struct Packet {
    pub bytes: Vec<u8>,
    pub first: u64,
    pub last: u64,
}
pub struct Assembler {
    format: Format,
    pending: Vec<u8>,
    remaining: u8,
    last_slot: u8,
    first: u64,
    last: u64,
}
fn traffic(s: u8) -> bool {
    (3..=7).contains(&s) || (11..=15).contains(&s)
}
impl Assembler {
    pub fn new(format: Format) -> Self {
        Self {
            format,
            pending: Vec::new(),
            remaining: 0,
            last_slot: 0,
            first: 0,
            last: 0,
        }
    }
    pub fn reset(&mut self) {
        self.pending.clear();
        self.remaining = 0;
    }
    pub fn accept(&mut self, info: &[u8; 28], offset: u64) -> Option<Packet> {
        let slot = info[0] >> 4;
        if self.format == Format::Slotted && !traffic(slot) {
            return None;
        }
        let h = if self.format == Format::Common { 0 } else { 3 };
        let (kind, value, payload) = (info[h] & 0xc0, info[h] & 63, &info[h + 1..]);
        let mut distance = 1;
        if self.format == Format::Slotted {
            while !traffic((self.last_slot + distance) % 16) {
                distance += 1;
            }
        }
        let consecutive = self.remaining > 0
            && offset.checked_sub(self.last) == Some(384 * distance as u64)
            && (self.format == Format::Common || slot == (self.last_slot + distance) % 16);
        match kind {
            0x80 => {
                self.reset();
                if value < 1 || value as usize * payload.len() > 257 {
                    return None;
                }
                self.first = offset;
                self.remaining = value;
            }
            0 => {
                if !consecutive || value < 1 || value != self.remaining - 1 {
                    self.reset();
                    return None;
                }
                self.remaining = value;
            }
            _ => {
                let finish = kind == 0xc0 || (consecutive && self.remaining == 1);
                let first = if kind == 0xc0 { offset } else { self.first };
                let mut bytes = if kind == 0xc0 {
                    Vec::new()
                } else {
                    self.pending.clone()
                };
                self.reset();
                let n = value as usize;
                if !finish
                    || n > payload.len()
                    || bytes.len() + n > 257
                    || payload[n..].iter().any(|b| *b != 0)
                {
                    return None;
                }
                bytes.extend_from_slice(&payload[..n]);
                if bytes.len() < 2 || bytes[1] as usize + 2 != bytes.len() {
                    return None;
                }
                return Some(Packet {
                    bytes,
                    first,
                    last: offset,
                });
            }
        }
        if self.pending.len() + payload.len() > 257 {
            self.reset();
            return None;
        }
        self.pending.extend_from_slice(payload);
        self.last = offset;
        self.last_slot = slot;
        None
    }
}
