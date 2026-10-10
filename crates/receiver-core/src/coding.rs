// Indexed elimination follows the reference matrix coordinates directly.
#![allow(clippy::needless_range_loop)]
use std::sync::OnceLock;

pub fn descramble(envelope: &[u8; 48]) -> [u8; 42] {
    let mut pn = [1u8; 336];
    let mut word = [0; 42];
    for i in 0..336 {
        if i >= 9 {
            pn[i] = pn[i - 9] ^ pn[i - 5];
        }
        let pos = 128 * (i / 112) + 16 + i % 112;
        word[i / 8] |= (((envelope[pos / 8] >> (7 - pos % 8)) & 1) ^ pn[i]) << (i % 8);
    }
    word
}
pub fn crc16(bytes: &[u8]) -> u16 {
    let mut crc = 0xffffu16;
    for &b in bytes {
        crc ^= (b as u16) << 8;
        for _ in 0..8 {
            crc = (crc << 1) ^ if crc & 0x8000 != 0 { 0x1021 } else { 0 };
        }
    }
    crc
}
pub struct Galois {
    pub powers: [u8; 510],
    logs: [usize; 256],
}
impl Galois {
    pub fn get() -> &'static Self {
        static GF: OnceLock<Galois> = OnceLock::new();
        GF.get_or_init(|| {
            let mut g = Galois {
                powers: [0; 510],
                logs: [0; 256],
            };
            let mut v = 1usize;
            for i in 0..255 {
                g.powers[i] = v as u8;
                g.powers[i + 255] = v as u8;
                g.logs[v] = i;
                v <<= 1;
                if v & 256 != 0 {
                    v ^= 0x11d;
                }
            }
            g
        })
    }
    pub fn mul(&self, a: u8, b: u8) -> u8 {
        if a == 0 || b == 0 {
            0
        } else {
            self.powers[self.logs[a as usize] + self.logs[b as usize]]
        }
    }
    pub fn div(&self, a: u8, b: u8) -> u8 {
        assert_ne!(b, 0);
        if a == 0 {
            0
        } else {
            self.powers[(self.logs[a as usize] + 255 - self.logs[b as usize]) % 255]
        }
    }
    fn syndromes(&self, word: &[u8], roots: usize) -> Vec<u8> {
        (0..roots)
            .map(|r| word.iter().fold(0, |v, b| self.mul(v, self.powers[r]) ^ b))
            .collect()
    }
    /// Berlekamp–Massey, Chien search and a bounded GF linear solve. All roots verified.
    pub fn correct(&self, word: &mut [u8], roots: usize) -> Option<usize> {
        let syn = self.syndromes(word, roots);
        if syn.iter().all(|s| *s == 0) {
            return Some(0);
        }
        let mut loc = vec![0; roots + 1];
        let mut prev = loc.clone();
        loc[0] = 1;
        prev[0] = 1;
        let (mut degree, mut gap, mut last) = (0, 1, 1);
        for step in 0..roots {
            let mut d = syn[step];
            for i in 1..=degree {
                d ^= self.mul(loc[i], syn[step - i]);
            }
            if d == 0 {
                gap += 1;
                continue;
            }
            let saved = loc.clone();
            let scale = self.div(d, last);
            for i in 0..roots + 1 - gap {
                loc[i + gap] ^= self.mul(scale, prev[i]);
            }
            if 2 * degree <= step {
                degree = step + 1 - degree;
                prev = saved;
                last = d;
                gap = 1;
            } else {
                gap += 1;
            }
        }
        if degree == 0 || degree > roots / 2 {
            return None;
        }
        let n = word.len();
        let positions: Vec<_> = (0..n)
            .filter(|&p| {
                let inverse = self.powers[(p + 255 - (n - 1)) % 255];
                (0..=degree)
                    .rev()
                    .fold(0, |v, i| self.mul(v, inverse) ^ loc[i])
                    == 0
            })
            .collect();
        if positions.len() != degree {
            return None;
        }
        let mut mat = vec![vec![0; degree + 1]; degree];
        for row in 0..degree {
            for col in 0..degree {
                mat[row][col] = self.powers[(row * (n - 1 - positions[col])) % 255];
            }
            mat[row][degree] = syn[row];
        }
        for col in 0..degree {
            let pivot = (col..degree).find(|&r| mat[r][col] != 0)?;
            mat.swap(pivot, col);
            let scale = mat[col][col];
            for i in col..=degree {
                mat[col][i] = self.div(mat[col][i], scale);
            }
            for row in 0..degree {
                if row == col {
                    continue;
                }
                let factor = mat[row][col];
                for i in col..=degree {
                    mat[row][i] ^= self.mul(factor, mat[col][i]);
                }
            }
        }
        for i in 0..degree {
            word[positions[i]] ^= mat[i][degree];
        }
        self.syndromes(word, roots)
            .iter()
            .all(|s| *s == 0)
            .then_some(degree)
    }
}
