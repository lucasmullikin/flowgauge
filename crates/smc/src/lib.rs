//! Minimal AppleSMC client over IOKit. Reads need no privileges; writes need root.
//! Every write checks the key's reported type and size first.

use std::ffi::{CString, c_void};
use std::fmt;
use std::mem::size_of;

const OP_READ: u8 = 5;
const OP_WRITE: u8 = 6;
const OP_KEY_AT_INDEX: u8 = 8;
const OP_KEY_INFO: u8 = 9;
const SELECTOR: u32 = 2;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Vers {
    major: u8,
    minor: u8,
    build: u8,
    reserved: u8,
    release: u16,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct PLimit {
    version: u16,
    length: u16,
    cpu: u32,
    gpu: u32,
    mem: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct RawInfo {
    size: u32,
    data_type: u32,
    attributes: u8,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Packet {
    key: u32,
    vers: Vers,
    plimit: PLimit,
    info: RawInfo,
    result: u8,
    status: u8,
    op: u8,
    data32: u32,
    bytes: [u8; 32],
}

const _: () = assert!(size_of::<Packet>() == 80);

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    static mach_task_self_: u32;
    fn IOServiceMatching(name: *const std::ffi::c_char) -> *mut c_void;
    fn IOServiceGetMatchingService(main_port: u32, matching: *mut c_void) -> u32;
    fn IOServiceOpen(service: u32, task: u32, kind: u32, connect: *mut u32) -> i32;
    fn IOServiceClose(connect: u32) -> i32;
    fn IOObjectRelease(object: u32) -> i32;
    fn IOConnectCallStructMethod(
        connection: u32,
        selector: u32,
        input: *const c_void,
        input_size: usize,
        output: *mut c_void,
        output_size: *mut usize,
    ) -> i32;
}

#[derive(Debug, Clone, PartialEq)]
pub enum Error {
    NoService,
    Open(i32),
    Kernel(i32),
    /// The SMC's own result byte, e.g. 0x84 for an unknown key.
    Smc(u8),
    BadKey(String),
    TypeMismatch { key: String, found: String, size: u32 },
    /// The reply was shorter than a full packet, so none of it can be trusted.
    ShortReply(usize),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NoService => write!(f, "AppleSMC service not found"),
            Error::Open(c) => write!(f, "IOServiceOpen failed: {:#x}", *c as u32),
            Error::Kernel(c) => write!(f, "IOKit call failed: {:#x}", *c as u32),
            Error::Smc(c) => write!(f, "SMC result {c:#04x}"),
            Error::BadKey(k) => write!(f, "invalid key {k:?}"),
            Error::ShortReply(n) => write!(f, "SMC replied with {n} bytes, expected {}", size_of::<Packet>()),
            Error::TypeMismatch { key, found, size } => {
                write!(f, "{key} is {found:?} size {size}, refusing")
            }
        }
    }
}

impl std::error::Error for Error {}

pub fn fourcc(key: &str) -> Result<u32, Error> {
    let bytes: [u8; 4] = key
        .as_bytes()
        .try_into()
        .map_err(|_| Error::BadKey(key.to_string()))?;
    Ok(u32::from_be_bytes(bytes))
}

pub fn fourcc_str(code: u32) -> String {
    String::from_utf8_lossy(&code.to_be_bytes()).into_owned()
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct KeyInfo {
    pub size: u32,
    pub data_type: u32,
}

impl KeyInfo {
    pub fn type_name(&self) -> String {
        fourcc_str(self.data_type)
    }
}

pub struct Smc {
    connection: u32,
}

impl Smc {
    pub fn open() -> Result<Smc, Error> {
        let name = CString::new("AppleSMC").expect("static name");
        // SAFETY: pointers are valid for the calls; GetMatchingService consumes `matching`.
        unsafe {
            let service = IOServiceGetMatchingService(0, IOServiceMatching(name.as_ptr()));
            if service == 0 {
                return Err(Error::NoService);
            }
            let mut connection = 0;
            let code = IOServiceOpen(service, mach_task_self_, 0, &mut connection);
            IOObjectRelease(service);
            if code != 0 {
                return Err(Error::Open(code));
            }
            Ok(Smc { connection })
        }
    }

    fn call(&self, input: &Packet) -> Result<Packet, Error> {
        let mut output = Packet::default();
        let mut output_size = size_of::<Packet>();
        // SAFETY: both buffers are live, correctly sized repr(C) packets.
        let code = unsafe {
            IOConnectCallStructMethod(
                self.connection,
                SELECTOR,
                (input as *const Packet).cast(),
                size_of::<Packet>(),
                (&mut output as *mut Packet).cast(),
                &mut output_size,
            )
        };
        if code != 0 {
            return Err(Error::Kernel(code));
        }
        if output_size < size_of::<Packet>() {
            return Err(Error::ShortReply(output_size));
        }
        if output.result != 0 {
            return Err(Error::Smc(output.result));
        }
        Ok(output)
    }

    pub fn info(&self, key: &str) -> Result<KeyInfo, Error> {
        let output = self.call(&Packet {
            key: fourcc(key)?,
            op: OP_KEY_INFO,
            ..Packet::default()
        })?;
        Ok(KeyInfo {
            size: output.info.size,
            data_type: output.info.data_type,
        })
    }

    pub fn read(&self, key: &str) -> Result<(KeyInfo, [u8; 32]), Error> {
        let info = self.info(key)?;
        let output = self.call(&Packet {
            key: fourcc(key)?,
            op: OP_READ,
            info: RawInfo {
                size: info.size,
                ..RawInfo::default()
            },
            ..Packet::default()
        })?;
        Ok((info, output.bytes))
    }

    fn checked(&self, key: &str, want: &str, size: u32) -> Result<KeyInfo, Error> {
        let info = self.info(key)?;
        if info.data_type != fourcc(want)? || info.size != size {
            return Err(Error::TypeMismatch {
                key: key.to_string(),
                found: info.type_name(),
                size: info.size,
            });
        }
        Ok(info)
    }

    pub fn read_u8(&self, key: &str) -> Result<u8, Error> {
        self.checked(key, "ui8 ", 1)?;
        Ok(self.read(key)?.1[0])
    }

    pub fn read_f32(&self, key: &str) -> Result<f32, Error> {
        self.checked(key, "flt ", 4)?;
        let bytes = self.read(key)?.1;
        Ok(f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn write(&self, key: &str, data: &[u8]) -> Result<(), Error> {
        let mut packet = Packet {
            key: fourcc(key)?,
            op: OP_WRITE,
            info: RawInfo {
                size: data.len() as u32,
                ..RawInfo::default()
            },
            ..Packet::default()
        };
        packet.bytes[..data.len()].copy_from_slice(data);
        self.call(&packet).map(|_| ())
    }

    pub fn write_u8(&self, key: &str, value: u8) -> Result<(), Error> {
        self.checked(key, "ui8 ", 1)?;
        self.write(key, &[value])
    }

    pub fn write_f32(&self, key: &str, value: f32) -> Result<(), Error> {
        self.checked(key, "flt ", 4)?;
        self.write(key, &value.to_le_bytes())
    }

    /// For keys already known to be `flt `: one IOKit call instead of three, for frequent polling.
    pub fn read_known_f32(&self, key: u32) -> Result<f32, Error> {
        let output = self.call(&Packet {
            key,
            op: OP_READ,
            info: RawInfo {
                size: 4,
                ..RawInfo::default()
            },
            ..Packet::default()
        })?;
        let b = output.bytes;
        Ok(f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub fn key_count(&self) -> Result<u32, Error> {
        let bytes = self.read("#KEY")?.1;
        Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    pub fn key_at(&self, index: u32) -> Result<String, Error> {
        let output = self.call(&Packet {
            op: OP_KEY_AT_INDEX,
            data32: index,
            ..Packet::default()
        })?;
        Ok(fourcc_str(output.key))
    }
}

impl Drop for Smc {
    fn drop(&mut self) {
        // SAFETY: the connection was opened by us and is closed once.
        unsafe {
            IOServiceClose(self.connection);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fourcc_round_trips() {
        assert_eq!(fourcc_str(fourcc("F0Md").unwrap()), "F0Md");
        assert_eq!(fourcc("#KEY").unwrap(), 0x234b4559);
    }

    #[test]
    fn fourcc_rejects_wrong_length() {
        assert!(matches!(fourcc("F0M"), Err(Error::BadKey(_))));
        assert!(matches!(fourcc("F0Mdd"), Err(Error::BadKey(_))));
    }

    #[test]
    fn reads_fan_count_from_real_smc() {
        let smc = Smc::open().unwrap();
        assert!(smc.key_count().unwrap() > 0);
        assert!(smc.read_u8("FNum").is_ok());
    }

    #[test]
    fn known_read_matches_the_checked_read() {
        let smc = Smc::open().unwrap();
        // Fanless Macs have no fan keys to compare.
        let Ok(checked) = smc.read_f32("F0Mx") else { return };
        assert_eq!(smc.read_known_f32(fourcc("F0Mx").unwrap()).unwrap(), checked);
    }

    #[test]
    fn typed_read_refuses_wrong_type() {
        let smc = Smc::open().unwrap();
        assert!(matches!(
            smc.read_f32("FNum"),
            Err(Error::TypeMismatch { .. })
        ));
    }

    #[test]
    fn unknown_key_is_an_smc_error() {
        let smc = Smc::open().unwrap();
        assert_eq!(smc.info("Zzz9"), Err(Error::Smc(0x84)));
    }
}
