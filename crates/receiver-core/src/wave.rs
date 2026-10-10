//! Streaming stereo IQ WAV input and mono PCM16 output. No whole-file allocation.
use crate::Complex32;
use std::{
    fs::{File, OpenOptions},
    io::{BufReader, Read, Seek, SeekFrom, Write},
    path::Path,
};
fn err(s: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, s)
}
fn u16le(b: &[u8]) -> u16 {
    u16::from_le_bytes(b[..2].try_into().unwrap())
}
fn u32le(b: &[u8]) -> u32 {
    u32::from_le_bytes(b[..4].try_into().unwrap())
}
pub struct IqWave {
    reader: BufReader<File>,
    remaining: u64,
    format: u16,
    bytes: usize,
    pub sample_rate: f64,
    pub samples: u64,
}
impl IqWave {
    pub fn open(path: impl AsRef<Path>) -> std::io::Result<Self> {
        let file = File::open(path)?;
        let length = file.metadata()?.len();
        let mut r = BufReader::new(file);
        let mut h = [0; 12];
        r.read_exact(&mut h)?;
        if &h[..4] != b"RIFF" || &h[8..] != b"WAVE" {
            return Err(err("Expected RIFF WAVE IQ input"));
        }
        let end = 8 + u32le(&h[4..]) as u64;
        if end != length || end < 12 {
            return Err(err("RIFF length differs from file length (wrapped recordings over 4 GiB must be converted to valid WAV chunks first)"));
        }
        let mut fmt = None;
        let mut data = None;
        while r.stream_position()? + 8 <= end {
            let mut ch = [0; 8];
            r.read_exact(&mut ch)?;
            let size = u32le(&ch[4..]) as u64;
            let pos = r.stream_position()?;
            if pos + size > end {
                return Err(err("Truncated WAV chunk"));
            }
            match &ch[..4] {
                b"fmt " => {
                    if fmt.is_some() || size < 16 {
                        return Err(err("Invalid or duplicate WAV format"));
                    }
                    let mut f = [0; 16];
                    r.read_exact(&mut f)?;
                    fmt = Some(f);
                }
                b"data" => {
                    if data.is_some() {
                        return Err(err("Duplicate IQ data chunk"));
                    }
                    data = Some((pos, size));
                }
                _ => {}
            }
            if pos + size + (size & 1) > end {
                return Err(err("Missing WAV chunk padding"));
            }
            r.seek(SeekFrom::Start(pos + size + (size & 1)))?;
        }
        let f = fmt.ok_or_else(|| err("Missing WAV format"))?;
        let (start, size) = data.ok_or_else(|| err("Missing IQ data"))?;
        let format = u16le(&f);
        let bits = u16le(&f[14..]);
        let bytes = (bits / 8) as usize;
        if u16le(&f[2..]) != 2
            || !matches!((format, bits), (3, 32) | (1, 16))
            || u16le(&f[12..]) as usize != 2 * bytes
            || size % (2 * bytes) as u64 != 0
        {
            return Err(err(
                "IQ WAV requires stereo float32 or PCM16, I left / Q right",
            ));
        }
        let sample_rate = u32le(&f[4..]) as f64;
        if !(9600.0..=20_000_000.0).contains(&sample_rate)
            || u32le(&f[8..]) as f64 != sample_rate * (2 * bytes) as f64
        {
            return Err(err("Unsupported IQ rate or invalid byte rate"));
        }
        r.seek(SeekFrom::Start(start))?;
        Ok(Self {
            reader: r,
            remaining: size,
            format,
            bytes,
            sample_rate,
            samples: size / (2 * bytes) as u64,
        })
    }
    pub fn read(&mut self, out: &mut [Complex32]) -> std::io::Result<usize> {
        let n = out
            .len()
            .min((self.remaining / (2 * self.bytes) as u64) as usize);
        let mut raw = vec![0; n * 2 * self.bytes];
        self.reader.read_exact(&mut raw)?;
        self.remaining -= raw.len() as u64;
        for (sample, b) in out.iter_mut().zip(raw.chunks_exact(2 * self.bytes)) {
            let value = |x: &[u8]| {
                if self.format == 3 {
                    f32::from_le_bytes(x[..4].try_into().unwrap())
                } else {
                    i16::from_le_bytes(x[..2].try_into().unwrap()) as f32 / 32768.0
                }
            };
            *sample = Complex32 {
                real: value(b),
                imaginary: value(&b[self.bytes..]),
            };
        }
        Ok(n)
    }
}
pub struct PcmWave {
    file: File,
    samples: u64,
}
impl PcmWave {
    pub fn samples(&self) -> u64 {
        self.samples
    }
    pub fn create(path: impl AsRef<Path>) -> std::io::Result<Self> {
        let mut s = Self {
            file: OpenOptions::new().write(true).create_new(true).open(path)?,
            samples: 0,
        };
        s.header()?;
        Ok(s)
    }
    pub fn write(&mut self, pcm: &[i16]) -> std::io::Result<()> {
        if (self.samples + pcm.len() as u64) * 2 > u32::MAX as u64 - 36 {
            return Err(err("WAV size limit reached"));
        }
        let b: Vec<_> = pcm.iter().flat_map(|v| v.to_le_bytes()).collect();
        self.file.write_all(&b)?;
        self.samples += pcm.len() as u64;
        Ok(())
    }
    pub fn silence(&mut self, mut samples: u64) -> std::io::Result<()> {
        if samples > (u32::MAX as u64 - 36) / 2 - self.samples {
            return Err(err("WAV gap exceeds RIFF size limit"));
        }
        let zero = [0; 8000];
        while samples > 0 {
            let n = samples.min(8000);
            self.write(&zero[..n as usize])?;
            samples -= n;
        }
        Ok(())
    }
    fn header(&mut self) -> std::io::Result<()> {
        let size = (self.samples * 2) as u32;
        self.file.seek(SeekFrom::Start(0))?;
        self.file.write_all(b"RIFF")?;
        self.file.write_all(&(36 + size).to_le_bytes())?;
        self.file.write_all(b"WAVEfmt ")?;
        for b in [
            16u32.to_le_bytes(),
            [1, 0, 1, 0],
            8000u32.to_le_bytes(),
            16000u32.to_le_bytes(),
            [2, 0, 16, 0],
        ] {
            self.file.write_all(&b)?;
        }
        self.file.write_all(b"data")?;
        self.file.write_all(&size.to_le_bytes())?;
        self.file.seek(SeekFrom::End(0))?;
        Ok(())
    }
    pub fn finish(mut self) -> std::io::Result<()> {
        // A failed write may have appended part of a sample block. Only fully
        // accepted samples belong to the finalized RIFF payload.
        self.file.set_len(44 + self.samples * 2)?;
        self.header()?;
        self.file.sync_all()
    }
}
