//! Bounded level-1 LH5 reader, ported from the reference's memory-only reader.
use super::Result;
use std::collections::HashMap;
struct Bits<'a> {
    bytes: &'a [u8],
    pos: usize,
}
impl Bits<'_> {
    fn read(&mut self, n: usize) -> Result<usize> {
        if n > 16 || self.pos + n > self.bytes.len() * 8 {
            return Err("Truncated LH5 stream");
        }
        let mut v = 0;
        for _ in 0..n {
            v = (v << 1) | ((self.bytes[self.pos / 8] >> (7 - self.pos % 8)) & 1) as usize;
            self.pos += 1;
        }
        Ok(v)
    }
}
struct Tree {
    constant: Option<usize>,
    symbols: HashMap<usize, usize>,
}
impl Tree {
    fn constant(s: usize, alphabet: usize) -> Result<Self> {
        if s >= alphabet {
            return Err("Invalid constant Huffman symbol");
        }
        Ok(Self {
            constant: Some(s),
            symbols: HashMap::new(),
        })
    }
    fn new(lengths: &[usize]) -> Result<Self> {
        let mut counts = [0; 17];
        let mut next = [0; 17];
        for &l in lengths {
            if l > 16 {
                return Err("Invalid Huffman length");
            }
            if l != 0 {
                counts[l] += 1;
            }
        }
        let mut code = 0;
        for l in 1..=16 {
            code = (code + counts[l - 1]) << 1;
            if code + counts[l] > 1 << l {
                return Err("Oversubscribed Huffman tree");
            }
            next[l] = code;
        }
        if code + counts[16] != 65536 {
            return Err("Incomplete Huffman tree");
        }
        let mut symbols = HashMap::new();
        for (s, &l) in lengths.iter().enumerate() {
            if l != 0 {
                symbols.insert((1 << l) | next[l], s);
                next[l] += 1;
            }
        }
        Ok(Self {
            constant: None,
            symbols,
        })
    }
    fn decode(&self, b: &mut Bits) -> Result<usize> {
        if let Some(s) = self.constant {
            return Ok(s);
        }
        let mut code = 1;
        for _ in 0..16 {
            code = (code << 1) | b.read(1)?;
            if let Some(s) = self.symbols.get(&code) {
                return Ok(*s);
            }
        }
        Err("Invalid Huffman symbol")
    }
}
fn lengths(b: &mut Bits, alphabet: usize, count_bits: usize, skip: usize) -> Result<Tree> {
    let count = b.read(count_bits)?;
    if count == 0 {
        return Tree::constant(b.read(count_bits)?, alphabet);
    }
    if count > alphabet {
        return Err("Invalid Huffman length count");
    }
    let mut ls = vec![0; alphabet];
    let mut i = 0;
    while i < count {
        let mut l = b.read(3)?;
        if l == 7 {
            while b.read(1)? != 0 {
                l += 1;
                if l > 16 {
                    return Err("Huffman code too long");
                }
            }
        }
        ls[i] = l;
        i += 1;
        if i == skip {
            i += b.read(2)?;
            if i > count {
                return Err("Invalid length run");
            }
        }
    }
    Tree::new(&ls)
}
fn literals(b: &mut Bits, tree: &Tree) -> Result<Tree> {
    let count = b.read(9)?;
    if count == 0 {
        return Tree::constant(b.read(9)?, 510);
    }
    if count > 510 {
        return Err("Invalid literal count");
    }
    let mut ls = [0; 510];
    let mut i = 0;
    while i < count {
        let symbol = tree.decode(b)?;
        if symbol > 2 {
            ls[i] = symbol - 2;
            i += 1;
        } else {
            i += match symbol {
                0 => 1,
                1 => b.read(4)? + 3,
                _ => b.read(9)? + 20,
            };
            if i > count {
                return Err("Invalid literal run");
            }
        }
    }
    Tree::new(&ls)
}
fn le16(b: &[u8]) -> usize {
    u16::from_le_bytes([b[0], b[1]]) as usize
}
pub fn expand(a: &[u8]) -> Result<Vec<u8>> {
    if a.len() < 27 || &a[2..7] != b"-lh5-" || a[20] != 1 {
        return Err("Unsupported archive");
    }
    let end = a[0] as usize + 2;
    let name = a[21] as usize;
    if end > a.len() || end < 27 + name {
        return Err("Truncated archive header");
    }
    if a[2..end].iter().fold(0u8, |s, b| s.wrapping_add(*b)) != a[1] {
        return Err("Archive header checksum mismatch");
    }
    let mut packed = u32::from_le_bytes(a[7..11].try_into().unwrap()) as usize;
    let size = u32::from_le_bytes(a[11..15].try_into().unwrap()) as usize;
    if size == 0 || size > 65536 || packed > 65536 {
        return Err("Archive size limit");
    }
    let expected = le16(&a[22 + name..]);
    let mut ext = le16(&a[end - 2..]);
    let mut start = end;
    while ext != 0 {
        if ext < 3 || ext > packed || start + ext > a.len() {
            return Err("Invalid archive extension");
        }
        packed -= ext;
        start += ext;
        ext = le16(&a[start - 2..]);
    }
    let end = start + packed;
    if end > a.len() || a.len() > end + 1 || a.len() == end + 1 && a[end] != 0 {
        return Err("Invalid archive length");
    }
    let mut bits = Bits {
        bytes: &a[start..end],
        pos: 0,
    };
    let mut output = Vec::with_capacity(size);
    while output.len() < size {
        let symbols = bits.read(16)?;
        if symbols == 0 {
            return Err("Empty compressed block");
        }
        let codes = lengths(&mut bits, 19, 5, 3)?;
        let literal = literals(&mut bits, &codes)?;
        let distance = lengths(&mut bits, 14, 4, usize::MAX)?;
        for _ in 0..symbols {
            if output.len() == size {
                return Err("Compressed block exceeds size");
            }
            let s = literal.decode(&mut bits)?;
            if s < 256 {
                output.push(s as u8);
                continue;
            }
            let count = s - 256 + 3;
            let db = distance.decode(&mut bits)?;
            let d = if db == 0 {
                0
            } else {
                (1 << (db - 1)) + bits.read(db - 1)?
            };
            if count > size - output.len() {
                return Err("Compressed match exceeds size");
            }
            for _ in 0..count {
                let v = if output.len() <= d {
                    0x20
                } else {
                    output[output.len() - d - 1]
                };
                output.push(v);
            }
        }
    }
    let mut crc = 0u16;
    for &v in &output {
        crc ^= v as u16;
        for _ in 0..8 {
            crc = (crc >> 1) ^ if crc & 1 != 0 { 0xa001 } else { 0 };
        }
    }
    if crc as usize != expected {
        return Err("Expanded archive CRC mismatch");
    }
    Ok(output)
}
