//! SoapySDR source using the installed, stable 0.8 C ABI. No device-specific backend.
#[cfg(feature = "soapy")]
use crate::Complex32;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Device {
    pub label: String,
    pub args: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RadioConfig {
    pub args: String,
    pub sample_rate: f64,
    pub center_hz: f64,
    pub gain_db: Option<f64>,
    pub antenna: String,
    pub channel: usize,
}
#[derive(Debug)]
pub enum Read {
    Samples(usize),
    Timeout,
    Discontinuity,
}
#[cfg(feature = "soapy")]
mod soapy {
    use super::*;
    use std::{
        ffi::{c_char, c_int, c_long, c_void, CStr, CString},
        ptr,
    };
    #[repr(C)]
    struct Kwargs {
        size: usize,
        keys: *mut *mut c_char,
        vals: *mut *mut c_char,
    }
    // All device and stream pointers remain on the creating acquisition thread.
    // CF32 is two contiguous IEEE binary32 values, matching Complex32's repr(C).
    extern "C" {
        fn SoapySDRDevice_enumerateStrArgs(args: *const c_char, len: *mut usize) -> *mut Kwargs;
        fn SoapySDRKwargs_toString(args: *const Kwargs) -> *mut c_char;
        fn SoapySDRKwargs_get(args: *const Kwargs, key: *const c_char) -> *const c_char;
        fn SoapySDRKwargsList_clear(args: *mut Kwargs, len: usize);
        fn SoapySDR_free(p: *mut c_void);
        fn SoapySDRDevice_lastError() -> *const c_char;
        fn SoapySDRDevice_lastStatus() -> c_int;
        fn SoapySDRDevice_makeStrArgs(args: *const c_char) -> *mut c_void;
        fn SoapySDRDevice_unmake(dev: *mut c_void) -> c_int;
        fn SoapySDRDevice_getNumChannels(dev: *const c_void, direction: c_int) -> usize;
        fn SoapySDRDevice_setSampleRate(
            dev: *mut c_void,
            direction: c_int,
            ch: usize,
            rate: f64,
        ) -> c_int;
        fn SoapySDRDevice_getSampleRate(dev: *const c_void, direction: c_int, ch: usize) -> f64;
        fn SoapySDRDevice_setFrequency(
            dev: *mut c_void,
            direction: c_int,
            ch: usize,
            freq: f64,
            args: *const Kwargs,
        ) -> c_int;
        fn SoapySDRDevice_getFrequency(dev: *const c_void, direction: c_int, ch: usize) -> f64;
        fn SoapySDRDevice_setGain(
            dev: *mut c_void,
            direction: c_int,
            ch: usize,
            gain: f64,
        ) -> c_int;
        fn SoapySDRDevice_setAntenna(
            dev: *mut c_void,
            direction: c_int,
            ch: usize,
            antenna: *const c_char,
        ) -> c_int;
        fn SoapySDRDevice_setupStream(
            dev: *mut c_void,
            direction: c_int,
            format: *const c_char,
            channels: *const usize,
            count: usize,
            args: *const Kwargs,
        ) -> *mut c_void;
        fn SoapySDRDevice_activateStream(
            dev: *mut c_void,
            stream: *mut c_void,
            flags: c_int,
            time: i64,
            elems: usize,
        ) -> c_int;
        fn SoapySDRDevice_deactivateStream(
            dev: *mut c_void,
            stream: *mut c_void,
            flags: c_int,
            time: i64,
        ) -> c_int;
        fn SoapySDRDevice_closeStream(dev: *mut c_void, stream: *mut c_void) -> c_int;
        fn SoapySDRDevice_readStream(
            dev: *mut c_void,
            stream: *mut c_void,
            buffs: *const *mut c_void,
            count: usize,
            flags: *mut c_int,
            time: *mut i64,
            timeout: c_long,
        ) -> c_int;
    }
    fn string(p: *const c_char) -> String {
        if p.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
        }
    }
    fn error(action: &str) -> String {
        format!(
            "{action}: {}",
            string(unsafe { SoapySDRDevice_lastError() })
        )
    }
    fn cstring(s: &str) -> Result<CString, String> {
        CString::new(s).map_err(|_| "Device argument contains NUL".into())
    }
    fn check(code: c_int, action: &str) -> Result<(), String> {
        if code == 0 {
            Ok(())
        } else {
            Err(error(action))
        }
    }
    pub fn enumerate() -> Result<Vec<Device>, String> {
        unsafe {
            let mut count = 0;
            let list = SoapySDRDevice_enumerateStrArgs(c"".as_ptr(), &mut count);
            if list.is_null() {
                return if SoapySDRDevice_lastStatus() == 0 {
                    Ok(Vec::new())
                } else {
                    Err(error("SDR enumeration failed"))
                };
            }
            let mut devices = Vec::new();
            for i in 0..count {
                let kwargs = list.add(i);
                let raw = SoapySDRKwargs_toString(kwargs);
                let args = string(raw);
                SoapySDR_free(raw.cast());
                let label = string(SoapySDRKwargs_get(kwargs, c"label".as_ptr()));
                devices.push(Device {
                    label: if label.is_empty() {
                        args.clone()
                    } else {
                        label
                    },
                    args,
                });
            }
            SoapySDRKwargsList_clear(list, count);
            Ok(devices)
        }
    }
    pub struct Sdr {
        dev: *mut c_void,
        stream: *mut c_void,
        pub sample_rate: f64,
        pub center_hz: f64,
        active: bool,
    }
    impl Sdr {
        pub fn open(c: &RadioConfig) -> Result<Self, String> {
            if !c.sample_rate.is_finite()
                || !(9600.0..=20_000_000.0).contains(&c.sample_rate)
                || !c.center_hz.is_finite()
                || c.center_hz <= 0.0
                || c.gain_db.is_some_and(|g| !g.is_finite())
            {
                return Err("Invalid radio configuration".into());
            }
            let args = cstring(&c.args)?;
            unsafe {
                let dev = SoapySDRDevice_makeStrArgs(args.as_ptr());
                if dev.is_null() {
                    return Err(error("Cannot open SDR"));
                }
                let mut s = Self {
                    dev,
                    stream: ptr::null_mut(),
                    sample_rate: 0.0,
                    center_hz: 0.0,
                    active: false,
                };
                if c.channel >= SoapySDRDevice_getNumChannels(dev, 1) {
                    return Err("RX channel does not exist".into());
                }
                check(
                    SoapySDRDevice_setSampleRate(dev, 1, c.channel, c.sample_rate),
                    "Set sample rate",
                )?;
                check(
                    SoapySDRDevice_setFrequency(dev, 1, c.channel, c.center_hz, ptr::null()),
                    "Tune SDR",
                )?;
                if let Some(g) = c.gain_db {
                    check(SoapySDRDevice_setGain(dev, 1, c.channel, g), "Set gain")?;
                }
                if !c.antenna.is_empty() {
                    let a = cstring(&c.antenna)?;
                    check(
                        SoapySDRDevice_setAntenna(dev, 1, c.channel, a.as_ptr()),
                        "Set antenna",
                    )?;
                }
                s.sample_rate = SoapySDRDevice_getSampleRate(dev, 1, c.channel);
                s.center_hz = SoapySDRDevice_getFrequency(dev, 1, c.channel);
                s.stream = SoapySDRDevice_setupStream(
                    dev,
                    1,
                    c"CF32".as_ptr(),
                    &c.channel,
                    1,
                    ptr::null(),
                );
                if s.stream.is_null() {
                    return Err(error("Create CF32 RX stream"));
                }
                check(
                    SoapySDRDevice_activateStream(dev, s.stream, 0, 0, 0),
                    "Activate RX stream",
                )?;
                s.active = true;
                Ok(s)
            }
        }
        pub fn read(&mut self, buf: &mut [Complex32]) -> Result<Read, String> {
            let buffers = [buf.as_mut_ptr().cast()];
            let mut flags = 0;
            let mut time = 0;
            let count = unsafe {
                SoapySDRDevice_readStream(
                    self.dev,
                    self.stream,
                    buffers.as_ptr(),
                    buf.len(),
                    &mut flags,
                    &mut time,
                    100_000,
                )
            };
            match count {
                -1 | 0 => Ok(Read::Timeout),
                -4 => Ok(Read::Discontinuity),
                n if n > 0 && n as usize <= buf.len() => {
                    if flags & 8 != 0 {
                        Ok(Read::Discontinuity)
                    } else {
                        Ok(Read::Samples(n as usize))
                    }
                }
                _ => Err(format!(
                    "SDR read failed ({count}): {}",
                    error("readStream")
                )),
            }
        }
    }
    impl Drop for Sdr {
        fn drop(&mut self) {
            unsafe {
                if self.active {
                    SoapySDRDevice_deactivateStream(self.dev, self.stream, 0, 0);
                }
                if !self.stream.is_null() {
                    SoapySDRDevice_closeStream(self.dev, self.stream);
                }
                SoapySDRDevice_unmake(self.dev);
            }
        }
    }
}
#[cfg(feature = "soapy")]
pub use soapy::{enumerate, Sdr};
#[cfg(not(feature = "soapy"))]
pub fn enumerate() -> Result<Vec<Device>, String> {
    Err("SoapySDR was disabled at build time".into())
}
