//! Rust port of the reference FrameDecoder, including bounded joint correction.
// Keep protocol bit positions explicit for comparison with the reference.
#![allow(clippy::needless_range_loop)]
use crate::coding::{crc16, descramble, Galois};
const COORDINATES: [usize; 188] = include!("voice_coordinates.in");
const CRC_POS: [usize; 8] = [0, 1, 2, 3, 82, 83, 84, 85];

#[derive(Debug, PartialEq)]
pub struct Speech {
    pub payload: [u8; 20],
    pub corrected_bits: u16,
}
fn bit(data: &[u8], i: usize) -> u8 {
    (data[i / 8] >> (i % 8)) & 1
}
fn set(data: &mut [u8], i: usize, v: u8) {
    data[i / 8] |= v << (i % 8);
}
fn parity(v: usize) -> u8 {
    (v.count_ones() & 1) as u8
}
fn crc8(data: &[u8]) -> u8 {
    let mut state = 255u16;
    for i in 0..86 {
        state = (state << 1) | if i < 78 { data[i] as u16 } else { 0 };
        if state & 256 != 0 {
            state ^= 0x19b;
        }
    }
    state as u8
}
fn convolution(input: &[u8; 94]) -> [u8; 176] {
    let mut result = [0; 176];
    let mut state = 0;
    for i in 0..94 {
        state = ((state << 1) | input[i] as usize) & 511;
        if COORDINATES[2 * i] != 255 {
            result[COORDINATES[2 * i]] = parity(state & 0x1af);
        }
        result[COORDINATES[2 * i + 1]] = parity(state & 0x11d);
    }
    result
}
fn protected(word: &[u8; 42]) -> [u8; 176] {
    let mut r = [0; 176];
    for i in 0..88 {
        r[2 * i] = bit(&word[5..], i);
        r[2 * i + 1] = bit(&word[21..], i);
    }
    r
}
fn write_protected(word: &mut [u8; 42], data: &[u8; 176]) {
    word[5..16].fill(0);
    word[21..32].fill(0);
    for i in 0..88 {
        set(&mut word[5..], i, data[2 * i]);
        set(&mut word[21..], i, data[2 * i + 1]);
    }
}
fn inverse(received: &[u8; 176]) -> [u8; 94] {
    let mut r = [0; 94];
    let mut state = 0;
    for t in 0..94 {
        r[t] = received[COORDINATES[2 * t + 1]] ^ parity(state & (0x11d >> 1));
        state = ((state << 1) | r[t] as usize) & 255;
    }
    r
}
fn viterbi(received: &[u8; 176]) -> [u8; 94] {
    let mut costs = [255u16; 256];
    costs[0] = 0;
    let mut next = [0u16; 256];
    let mut survivors = [[0u8; 32]; 94];
    for t in 0..94 {
        let punctured = COORDINATES[2 * t] == 255;
        let observed = if punctured { 1 } else { 2 };
        let a = received[COORDINATES[2 * t + 1]];
        let b = if punctured {
            0
        } else {
            received[COORDINATES[2 * t]]
        };
        for prior in 0..128 {
            let symbol = prior * 2;
            let distance = (parity(symbol & 0x11d) ^ a) as u16
                + if punctured {
                    0
                } else {
                    (parity(symbol & 0x1af) ^ b) as u16
                };
            let complement = observed - distance;
            let (left, right) = (costs[prior], costs[prior + 128]);
            next[2 * prior] = 255.min((left + distance).min(right + complement));
            next[2 * prior + 1] = 255.min((left + complement).min(right + distance));
            set(
                &mut survivors[t],
                2 * prior,
                u8::from(right + complement < left + distance),
            );
            set(
                &mut survivors[t],
                2 * prior + 1,
                u8::from(right + distance < left + complement),
            );
        }
        std::mem::swap(&mut costs, &mut next);
    }
    let mut state = 0;
    let mut r = [0; 94];
    for t in (0..94).rev() {
        r[t] = (state & 1) as u8;
        state = (state >> 1) | ((bit(&survivors[t], state) as usize) << 7);
    }
    r
}
fn tail(word: &mut [u8; 42]) {
    let crc = crc16(&word[28..34]);
    word[34..36].copy_from_slice(&crc.to_be_bytes());
    let mut work = [0; 14];
    work[..8].copy_from_slice(&word[28..36]);
    let gf = Galois::get();
    for i in 0..8 {
        let c = work[i];
        for (j, g) in [1, 63, 1, 218, 32, 227, 38].iter().enumerate() {
            work[i + j] ^= gf.mul(c, *g);
        }
    }
    word[36..].copy_from_slice(&work[8..]);
}
fn checked(word: &[u8; 42], received: &[u8; 176], input: &[u8; 94], max: u16) -> Option<Speech> {
    if input[86..].iter().any(|b| *b != 0) {
        return None;
    }
    let checksum = CRC_POS
        .iter()
        .enumerate()
        .fold(0, |v, (i, p)| v | (input[*p] << i));
    if checksum != crc8(&input[4..]) {
        return None;
    }
    let encoded = convolution(input);
    let distance: u16 = encoded
        .iter()
        .zip(received)
        .map(|(a, b)| (a ^ b) as u16)
        .sum();
    if distance > max {
        return None;
    }
    let mut corrected = *word;
    write_protected(&mut corrected, &encoded);
    tail(&mut corrected);
    if corrected[34..] != word[34..] {
        return None;
    }
    let mut payload = [0; 20];
    for i in 0..78 {
        set(&mut payload, i, input[4 + i]);
    }
    for i in 0..40 {
        set(&mut payload, 78 + i, bit(word, i));
        set(&mut payload, 118 + i, bit(&word[16..], i));
    }
    Some(Speech {
        payload,
        corrected_bits: distance,
    })
}
fn mode(word: &[u8; 42]) -> bool {
    word[32] == 0x80 && word[33] <= 1
}
fn joint(word: &[u8; 42], received: &[u8; 176], source: &[u8; 94]) -> Option<Speech> {
    let mut c = *word;
    write_protected(&mut c, &convolution(source));
    c[32] = 0x80;
    for alternate in 0..2 {
        c[33] = (word[33] & 1) ^ alternate;
        if (word[32] ^ c[32]).count_ones() + (word[33] ^ c[33]).count_ones() > 2 {
            continue;
        }
        tail(&mut c);
        let (mut outer_bits, mut outer_symbols, mut extra) = (0, 0, 0);
        for i in 28..42 {
            let bits = (c[i] ^ word[i]).count_ones();
            outer_bits += bits;
            outer_symbols += u32::from(bits != 0);
            if i >= 32 {
                extra += bits;
            }
        }
        if outer_bits > 6 || outer_symbols > 4 {
            continue;
        }
        if let Some(mut speech) = checked(&c, received, source, 8) {
            if speech.corrected_bits as u32 + extra <= 12 {
                speech.corrected_bits += extra as u16;
                return Some(speech);
            }
        }
    }
    None
}
pub fn decode(envelope: &[u8; 48]) -> Option<Speech> {
    let word = descramble(envelope);
    let received = protected(&word);
    let inverted = inverse(&received);
    let is_mode = mode(&word);
    if is_mode {
        if let Some(s) = checked(&word, &received, &inverted, 8) {
            return Some(s);
        }
    }
    let mut repaired = word;
    if Galois::get().correct(&mut repaired[28..], 6).is_some()
        && mode(&repaired)
        && crc16(&repaired[28..34]) == u16::from_be_bytes([repaired[34], repaired[35]])
        && repaired != word
    {
        let outer = protected(&repaired);
        let accepted = checked(&repaired, &received, &inverse(&outer), 8)
            .or_else(|| checked(&repaired, &received, &viterbi(&outer), 8));
        if let Some(mut s) = accepted {
            s.corrected_bits += (32..42)
                .map(|i| (word[i] ^ repaired[i]).count_ones() as u16)
                .sum::<u16>();
            return Some(s);
        }
    }
    let mut decoded = [0; 94];
    if is_mode {
        decoded = viterbi(&received);
        if let Some(s) = checked(&word, &received, &decoded, 8) {
            return Some(s);
        }
    }
    if (word[32] ^ 0x80).count_ones() + (word[33] & 0xfe).count_ones() > 2 {
        return None;
    }
    if let Some(s) = joint(&word, &received, &inverted) {
        return Some(s);
    }
    if !is_mode {
        decoded = viterbi(&received);
    }
    joint(&word, &received, &decoded)
}
