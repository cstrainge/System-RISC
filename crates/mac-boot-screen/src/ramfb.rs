//! RAMFB wire preparation. CPU addresses and device-visible bus addresses are
//! distinct even when a particular bootstrap happens to use identity mapping.
use core::fmt;

#[allow(unsafe_code)]
mod native;
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
                f.write_str("RAMFB fw_cfg signature does not match the expected device; select \
                             the correct platform register window")
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
fn directory_count(observed: u32) -> Result<(), Error>
{
    if observed > 4096 { Err(Error::DirectoryCount { observed }) } else { Ok(()) }
}
fn configuration_size(observed: u32) -> Result<(), Error>
{
    if observed != 28 { Err(Error::ConfigurationSize { observed }) } else { Ok(()) }
}
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
#[repr(C, align(8))]
struct Dma
{
    control: u32,
    length:  u32,
    address: u64
}
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
struct Ownership(core::sync::atomic::AtomicU8);
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
struct Claim<'a>
{
    owner:    &'a Ownership,
    previous: u8,
    retained: bool,
    armed:    bool
}
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

/// The QEMU fw_cfg RAMFB backend. `translate` proves the pixel buffer's
/// device-visible address; a bootstrap that identity-maps passes
/// `|pointer, _| Some(pointer as u64)`.
pub struct Ramfb
{
    registers:   usize,
    framebuffer: u64,
    width:       usize,
    height:      usize,
    translate:   fn(*const u8, usize) -> Option<u64>
}
#[allow(unsafe_code)]
impl Ramfb
{
    /// # Safety
    /// `registers` names a valid mapped fw_cfg register window of at least 24
    /// bytes, accessible until reset. `framebuffer` is pinned device-visible
    /// physical guest RAM for `width * height * 4` bytes. `translate` must
    /// prove contiguous device-visible addresses for the caller's pinned CPU
    /// spans and must never treat a virtual pointer as a physical address. All
    /// access to the same fw_cfg window must obey [`configure`]'s ownership
    /// protocol; the backend is a convenience over it, not a second one.
    pub const unsafe fn new(registers: usize, framebuffer: u64, width: usize, height: usize,
                            translate: fn(*const u8, usize) -> Option<u64>)
                            -> Self
    {
        Self { registers,
               framebuffer,
               width,
               height,
               translate }
    }
}
#[allow(unsafe_code)]
impl crate::backend::Backend for Ramfb
{
    type Error = Error;

    fn acquire<'a>(&mut self, pixels: &'a mut [u32]) -> Result<crate::backend::Frame<'a>, Error>
    {
        let geometry = || {
            Error::Geometry { width:  self.width,
                              height: self.height }
        };
        let words = self.width.checked_mul(self.height).ok_or_else(geometry)?;
        if pixels.len() < words
        {
            return Err(geometry());
        }
        // SAFETY: the constructor's contract, unchanged. `translate` is a plain
        // function pointer, so it borrows nothing from this call.
        unsafe {
            configure(self.registers, self.framebuffer, self.width, self.height, self.translate)?;
        }
        Ok(crate::backend::Frame { width:  self.width,
                                   height: self.height,
                                   stride: self.width,
                                   pixels: &mut pixels[..words] })
    }
}

struct Configuration([u8; 28]);
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
