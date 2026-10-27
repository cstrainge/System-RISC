//! RAMFB wire preparation. CPU addresses and device-visible bus addresses are
//! distinct even when a particular bootstrap happens to use identity mapping.
use core::fmt;

#[cfg(target_arch = "riscv64")]
#[allow(unsafe_code)]
mod native;
#[cfg(target_arch = "riscv64")]
pub use native::{PreparedTerminal, configure, prepare_terminal};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error
{
    Geometry
    {
        width:  usize,
        height: usize
    },
    FramebufferAddress
    {
        physical: u64,
        bytes:    u64
    },
    DmaAddress
    {
        cpu_address: usize,
        bytes:       usize
    },
    Signature,
    DmaSignature,
    MissingFile,
    DirectoryCount
    {
        observed: u32
    },
    ConfigurationSize
    {
        observed: u32
    },
    TransferFailed
    {
        control: u32
    },
    InFlight,
    Timeout
}
impl Error
{
    /// Errors produced by this binding for which device/DMA quiescence is not
    /// proven. Firmware must not hand the device to a separately linked System
    /// with a fresh ownership guard after either refusal. Other errors arise
    /// before submission or after the device acknowledges command completion.
    pub const fn dma_ownership_unknown(self) -> bool
    {
        match self
        {
            Self::InFlight | Self::Timeout => true,
            Self::Geometry { .. }
            | Self::FramebufferAddress { .. }
            | Self::DmaAddress { .. }
            | Self::Signature
            | Self::DmaSignature
            | Self::MissingFile
            | Self::DirectoryCount { .. }
            | Self::ConfigurationSize { .. }
            | Self::TransferFailed { .. } => false
        }
    }
}
impl fmt::Display for Error
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::Geometry { width,
                             height } =>
            {
                write!(f,
                       "RAMFB dimensions {width}x{height} are empty or exceed the 32-bit width, \
                        height or byte-stride fields")
            }
            Self::FramebufferAddress { physical,
                                       bytes } =>
            {
                write!(f, "RAMFB physical framebuffer {physical:#x}+{bytes:#x} overflows")
            }
            Self::DmaAddress { cpu_address,
                               bytes } =>
            {
                write!(f,
                       "RAMFB CPU buffer {cpu_address:#x}+{bytes:#x} has no contiguous \
                        device-visible RAM mapping")
            }
            Self::Signature =>
            {
                f.write_str("RAMFB fw_cfg signature is not QEMU; select the correct platform \
                             register window")
            }
            Self::DmaSignature =>
            {
                f.write_str("RAMFB fw_cfg does not expose the required DMA interface")
            }
            Self::MissingFile =>
            {
                f.write_str("RAMFB configuration file is unavailable; attach a ramfb device")
            }
            Self::DirectoryCount { observed } =>
            {
                write!(f,
                       "RAMFB fw_cfg directory has {observed} entries, exceeding the 4096-entry \
                        bootstrap bound")
            }
            Self::ConfigurationSize { observed } =>
            {
                write!(f, "RAMFB configuration is {observed} bytes; this platform requires 28")
            }
            Self::TransferFailed { control } =>
            {
                write!(f, "RAMFB configuration DMA failed with control bits {control:#x}")
            }
            Self::InFlight =>
            {
                f.write_str("RAMFB device or permanent DMA scratch is owned by another operation \
                             or terminal mode; an earlier timed-out transfer requires reset \
                             before retrying")
            }
            Self::Timeout =>
            {
                f.write_str("RAMFB configuration DMA did not complete within the bounded \
                             bootstrap poll")
            }
        }
    }
}
#[cfg(any(target_arch = "riscv64", test))]
fn directory_count(observed: u32) -> Result<(), Error>
{
    if observed > 4096 { Err(Error::DirectoryCount { observed }) } else { Ok(()) }
}
#[cfg(any(target_arch = "riscv64", test))]
fn configuration_size(observed: u32) -> Result<(), Error>
{
    if observed != 28 { Err(Error::ConfigurationSize { observed }) } else { Ok(()) }
}
#[cfg(any(target_arch = "riscv64", test))]
fn completion(control: u32) -> Option<Result<(), Error>>
{
    if control == 0
    {
        Some(Ok(()))
    }
    else if control & 1 != 0
    {
        Some(Err(Error::TransferFailed { control }))
    }
    else
    {
        None
    }
}
#[cfg(any(target_arch = "riscv64", test))]
#[repr(C, align(8))]
struct Dma
{
    control: u32,
    length:  u32,
    address: u64
}
#[cfg(any(target_arch = "riscv64", test))]
impl Dma
{
    const fn empty() -> Self
    {
        Self { control: 0,
               length:  0,
               address: 0 }
    }

    fn configuration(selector: u16, data_bus: u64) -> Self
    {
        Self { control: ((u32::from(selector) << 16) | 8 | 16).to_be(),
               length:  28u32.to_be(),
               address: data_bus.to_be() }
    }
}
/// One device's exclusive ownership, shared by normal and terminal paths.
/// No lock or waiter is needed: contending callers receive a typed refusal.
#[cfg(any(target_arch = "riscv64", test))]
struct Ownership(core::sync::atomic::AtomicU8);
#[cfg(any(target_arch = "riscv64", test))]
impl Ownership
{
    const IDLE: u8 = 0;
    const NORMAL: u8 = 1;
    const PREPARED: u8 = 3;
    const PREPARED_NORMAL: u8 = 4;
    const PREPARING: u8 = 2;
    const TERMINAL: u8 = 5;
    const TIMED_OUT: u8 = 6;

    const fn new() -> Self
    {
        Self(core::sync::atomic::AtomicU8::new(Self::IDLE))
    }

    fn normal(&self) -> Result<Claim<'_>, Error>
    {
        use core::sync::atomic::Ordering;
        let previous = self.0.load(Ordering::Acquire);
        let active = match previous
        {
            Self::IDLE => Self::NORMAL,
            Self::PREPARED => Self::PREPARED_NORMAL,
            _ => return Err(Error::InFlight)
        };
        self.0
            .compare_exchange(previous, active, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| Error::InFlight)?;
        Ok(Claim { owner: self,
                   previous,
                   retained: false,
                   armed: true })
    }

    fn prepare(&self) -> Result<Claim<'_>, Error>
    {
        use core::sync::atomic::Ordering;
        self.0
            .compare_exchange(Self::IDLE, Self::PREPARING, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| Error::InFlight)?;
        Ok(Claim { owner:    self,
                   previous: Self::IDLE,
                   retained: false,
                   armed:    true })
    }

    fn terminal(&self) -> Result<(), Error>
    {
        use core::sync::atomic::Ordering;
        self.0
            .compare_exchange(Self::PREPARED, Self::TERMINAL, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| ())
            .map_err(|_| Error::InFlight)
    }
}
#[cfg(any(target_arch = "riscv64", test))]
struct Claim<'a>
{
    owner:    &'a Ownership,
    previous: u8,
    retained: bool,
    armed:    bool
}
#[cfg(any(target_arch = "riscv64", test))]
impl Claim<'_>
{
    fn submitted(&mut self)
    {
        self.retained = true;
    }

    fn completed(&mut self)
    {
        self.retained = false;
    }

    fn publish(mut self)
    {
        self.owner.0.store(Ownership::PREPARED, core::sync::atomic::Ordering::Release);
        self.armed = false;
    }
}
#[cfg(any(target_arch = "riscv64", test))]
impl Drop for Claim<'_>
{
    fn drop(&mut self)
    {
        if self.armed
        {
            self.owner.0.store(if self.retained { Ownership::TIMED_OUT } else { self.previous },
                               core::sync::atomic::Ordering::Release);
        }
    }
}
impl core::error::Error for Error
{
}

#[cfg(any(target_arch = "riscv64", test))]
struct Configuration([u8; 28]);
#[cfg(any(target_arch = "riscv64", test))]
impl Configuration
{
    fn new(physical: u64, width: usize, height: usize) -> Result<Self, Error>
    {
        let invalid = || {
            Error::Geometry { width,
                              height }
        };
        let w = u32::try_from(width).ok().filter(|&n| n != 0).ok_or_else(invalid)?;
        let h = u32::try_from(height).ok().filter(|&n| n != 0).ok_or_else(invalid)?;
        let stride = w.checked_mul(4).ok_or_else(invalid)?;
        let bytes = u64::from(stride) * u64::from(h);
        if physical.checked_add(bytes).is_none()
        {
            return Err(Error::FramebufferAddress { physical,
                                                   bytes });
        }
        let mut output = [0; 28];
        output[0..8].copy_from_slice(&physical.to_be_bytes());
        output[8..12].copy_from_slice(&u32::from_le_bytes(*b"XR24").to_be_bytes());
        output[16..20].copy_from_slice(&w.to_be_bytes());
        output[20..24].copy_from_slice(&h.to_be_bytes());
        output[24..28].copy_from_slice(&stride.to_be_bytes());
        Ok(Self(output))
    }
}
#[cfg(any(target_arch = "riscv64", test))]
fn bus_address(pointer: *const u8, bytes: usize, alignment: u64,
               translate: &mut impl FnMut(*const u8, usize) -> Option<u64>)
               -> Result<u64, Error>
{
    translate(pointer, bytes).filter(|bus| {
                                 bus.is_multiple_of(alignment)
                                 && bus.checked_add(bytes as u64).is_some()
                             })
                             .ok_or(Error::DmaAddress { cpu_address: pointer as usize,
                                                        bytes })
}
#[cfg(test)]
mod tests
{
    extern crate std;
    use std::string::ToString;

    use super::*;

    #[test]
    fn configuration_uses_physical_framebuffer_and_checked_wire_geometry()
    {
        let config = Configuration::new(0x12345000, 800, 600).unwrap();
        assert_eq!(&config.0[0..8], &0x12345000u64.to_be_bytes());
        assert_eq!(&config.0[8..12], &u32::from_le_bytes(*b"XR24").to_be_bytes());
        assert_eq!(&config.0[12..16], &[0; 4]);
        assert_eq!(&config.0[16..20], &800u32.to_be_bytes());
        assert_eq!(&config.0[20..24], &600u32.to_be_bytes());
        assert_eq!(&config.0[24..28], &3200u32.to_be_bytes());
        for (width, height) in [(0, 600), (800, 0), (usize::MAX, 600), (u32::MAX as usize, 1)]
        {
            let error = Configuration::new(0x12345000, width, height).err().unwrap();
            assert_eq!(error,
                       Error::Geometry { width,
                                         height });
            assert!(error.to_string().contains("dimensions"));
        }
        assert!(matches!(Configuration::new(u64::MAX - 1, 800, 600),
                         Err(Error::FramebufferAddress { .. })));
        assert_eq!(&Configuration::new(0, 800, 600).unwrap().0[..8], &[0; 8]);
    }

    #[test]
    fn firmware_handoff_requires_proven_dma_quiescence_for_every_error_family()
    {
        for error in [Error::InFlight, Error::Timeout]
        {
            assert!(error.dma_ownership_unknown(), "{error}");
            assert!(!error.to_string().is_empty());
        }
        for error in [Error::Geometry { width:  0,
                                        height: 600 },
                      Error::FramebufferAddress { physical: u64::MAX,
                                                  bytes:    4 },
                      Error::DmaAddress { cpu_address: 0,
                                          bytes:       16 },
                      Error::Signature,
                      Error::DmaSignature,
                      Error::MissingFile,
                      Error::DirectoryCount { observed: 4097 },
                      Error::ConfigurationSize { observed: 27 },
                      Error::TransferFailed { control: 1 }]
        {
            assert!(!error.dma_ownership_unknown(), "{error}");
            assert!(!error.to_string().is_empty());
        }
    }

    #[test]
    fn dma_uses_the_binding_translation_and_retains_typed_refusal_details()
    {
        let data = [0u8; 28];
        let mut calls = 0;
        let translated = bus_address(data.as_ptr(), data.len(), 1, &mut |pointer, bytes| {
                             assert_eq!(pointer, data.as_ptr());
                             assert_eq!(bytes, data.len());
                             calls += 1;
                             Some(0x24680000)
                         }).unwrap();
        assert_eq!(calls, 1);
        assert_eq!(translated, 0x24680000);
        assert_ne!(translated, data.as_ptr() as u64);
        assert_eq!(bus_address(data.as_ptr(), data.len(), 8, &mut |_, _| Some(0)).unwrap(), 0);
        for bus in [None, Some(0x24680001), Some(u64::MAX - 7)]
        {
            let error = bus_address(data.as_ptr(), data.len(), 8, &mut |_, _| bus).unwrap_err();
            assert_eq!(error,
                       Error::DmaAddress { cpu_address: data.as_ptr() as usize,
                                           bytes:       data.len() });
            assert!(error.to_string().contains("contiguous device-visible RAM mapping"));
        }
    }
    #[test]
    fn admitted_directory_and_dma_wire_preserve_sizes_selectors_and_endianness()
    {
        for count in [0, 1, 4096]
        {
            directory_count(count).unwrap();
        }
        assert_eq!(directory_count(4097), Err(Error::DirectoryCount { observed: 4097 }));
        assert!(directory_count(u32::MAX).unwrap_err().to_string().contains("4096-entry"));
        configuration_size(28).unwrap();
        for length in [0, 27, 29, u32::MAX]
        {
            assert_eq!(configuration_size(length),
                       Err(Error::ConfigurationSize { observed: length }));
        }
        assert_eq!(size_of::<Dma>(), 16);
        assert_eq!(align_of::<Dma>(), 8);
        let empty = Dma::empty();
        assert_eq!((empty.control, empty.length, empty.address), (0, 0, 0));
        let command = Dma::configuration(0x1234, 0x2468_ace0_0000_1000);
        assert_eq!(command.control.to_ne_bytes(), [0x12, 0x34, 0, 24]);
        assert_eq!(command.length.to_ne_bytes(), [0, 0, 0, 28]);
        assert_eq!(command.address.to_ne_bytes(), [0x24, 0x68, 0xac, 0xe0, 0, 0, 0x10, 0]);
        assert_eq!(completion(0), Some(Ok(())));
        assert_eq!(completion(24), None);
        assert_eq!(completion(u32::MAX), Some(Err(Error::TransferFailed { control: u32::MAX })));
    }
    #[test]
    fn preparation_and_normal_admission_roll_back_before_device_submission()
    {
        let owner = Ownership::new();
        assert_eq!(owner.terminal(), Err(Error::InFlight));
        {
            let _claim = owner.prepare().unwrap();
            assert!(matches!(owner.prepare(), Err(Error::InFlight)));
            assert!(matches!(owner.normal(), Err(Error::InFlight)));
            assert_eq!(owner.terminal(), Err(Error::InFlight));
        }
        // Any pre-submission validation/translation refusal drops its claim.
        drop(owner.normal().unwrap());
        owner.prepare().unwrap().publish();
        assert!(matches!(owner.prepare(), Err(Error::InFlight)));
        drop(owner.normal().unwrap());
        owner.terminal().unwrap();
        assert!(matches!(owner.normal(), Err(Error::InFlight)));
        assert!(matches!(owner.prepare(), Err(Error::InFlight)));
        assert_eq!(owner.terminal(), Err(Error::InFlight));
    }
    #[test]
    fn submitted_timeout_keeps_normal_device_and_dma_ownership_until_reset()
    {
        for prepared in [false, true]
        {
            let owner = Ownership::new();
            if prepared
            {
                owner.prepare().unwrap().publish();
            }
            let mut claim = owner.normal().unwrap();
            claim.submitted();
            assert_eq!(owner.terminal(), Err(Error::InFlight));
            drop(claim); // A timeout cannot prove DMA quiescence.
            assert!(matches!(owner.normal(), Err(Error::InFlight)));
            assert!(matches!(owner.prepare(), Err(Error::InFlight)));
            assert_eq!(owner.terminal(), Err(Error::InFlight));
        }
        assert!(Error::InFlight.to_string().contains("requires reset"));
    }
    #[test]
    fn completed_normal_dma_releases_device_but_terminal_claim_never_does()
    {
        for result in [completion(0).unwrap(), completion(1).unwrap()]
        {
            let owner = Ownership::new();
            owner.prepare().unwrap().publish();
            let mut claim = owner.normal().unwrap();
            claim.submitted();
            assert_eq!(owner.terminal(), Err(Error::InFlight));
            assert!(matches!(result, Ok(()) | Err(Error::TransferFailed { .. })));
            claim.completed();
            drop(claim);
            owner.terminal().unwrap();
            assert!(matches!(owner.normal(), Err(Error::InFlight)));
            assert_eq!(owner.terminal(), Err(Error::InFlight));
        }
    }
    #[test]
    fn terminal_and_normal_claims_cannot_win_the_same_prepared_device()
    {
        use std::{sync::{Arc, Barrier},
                  thread};
        for _ in 0..32
        {
            let owner = Arc::new(Ownership::new());
            owner.prepare().unwrap().publish();
            let start = Arc::new(Barrier::new(2));
            let finish = Arc::new(Barrier::new(2));
            let normal = {
                let (owner, start, finish) = (owner.clone(), start.clone(), finish.clone());
                thread::spawn(move || {
                    start.wait();
                    let claim = owner.normal();
                    finish.wait(); // Keep a successful normal claim pinned.
                    claim.is_ok()
                })
            };
            start.wait();
            let terminal = owner.terminal().is_ok();
            finish.wait();
            assert_ne!(normal.join().unwrap(), terminal);
        }
    }
}
