//! Allocation-free validation before any device-selected array access.
#![forbid(unsafe_code)]
use core::fmt;

pub const QUEUE_LEN: usize = 64;
pub const DEVICE_COUNT: usize = 2;
pub const EVENT_BYTES: u32 = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error
{
    Dma
    {
        buffer: &'static str,
        cpu:    usize,
        bytes:  usize
    },
    Identity
    {
        base:    usize,
        magic:   u32,
        version: u32,
        device:  u32
    },
    Reset
    {
        base:   usize,
        status: u32
    },
    VersionFeature
    {
        base: usize
    },
    Features
    {
        base:   usize,
        status: u32
    },
    QueueReady
    {
        base:  usize,
        ready: u32
    },
    QueueSize
    {
        base:    usize,
        maximum: u32
    },
    Axis
    {
        base:    usize,
        axis:    u8,
        minimum: u32,
        maximum: u32
    },
    DeviceFailed
    {
        base:   usize,
        status: u32
    },
    UsedAdvance
    {
        base: usize,
        last: u16,
        used: u16
    },
    Completion
    {
        base:  usize,
        id:    u32,
        bytes: u32
    },
    Duplicate
    {
        base: usize,
        id:   u32
    },
    DeviceCount
    {
        found: usize
    },
    NotPrepared,
    AlreadyPrepared,
    AlreadyTaken
}
impl fmt::Display for Error
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::Dma { buffer,
                        cpu,
                        bytes } =>
            {
                write!(f,
                       "input {buffer} CPU buffer {cpu:#x}+{bytes} bytes has no contiguous bus \
                        mapping; queue was not published")
            }
            Self::Identity { base,
                             magic,
                             version,
                             device } =>
            {
                write!(f,
                       "input device at {base:#x} is not modern virtio input (magic={magic:#x}, \
                        version={version}, device={device}); queue was not published")
            }
            Self::Reset { base,
                          status } =>
            {
                write!(f,
                       "input device at {base:#x} did not acknowledge bounded reset \
                        (status={status:#x}); retained buffers will not be reused")
            }
            Self::VersionFeature { base } =>
            {
                write!(f,
                       "input device at {base:#x} lacks VIRTIO_F_VERSION_1; queue was not \
                        published")
            }
            Self::Features { base,
                             status } =>
            {
                write!(f,
                       "input device at {base:#x} rejected negotiated features \
                        (status={status:#x}); queue was not published")
            }
            Self::QueueReady { base,
                               ready } =>
            {
                write!(f,
                       "input device at {base:#x} reports queue zero already ready ({ready}); \
                        queue was not replaced")
            }
            Self::QueueSize { base,
                              maximum } =>
            {
                write!(f,
                       "input device at {base:#x} supports {maximum} descriptors, fewer than the \
                        required {QUEUE_LEN}; queue was not published")
            }
            Self::Axis { base,
                         axis,
                         minimum,
                         maximum } =>
            {
                write!(f,
                       "input device at {base:#x} axis {axis} has unsupported range \
                        {minimum}..{maximum}; expected 0..32767")
            }
            Self::DeviceFailed { base,
                                 status } =>
            {
                write!(f,
                       "input device at {base:#x} failed or requested reset (status={status:#x}); \
                        endpoint was retired with its buffers pinned")
            }
            Self::UsedAdvance { base,
                                last,
                                used } =>
            {
                write!(f,
                       "input device at {base:#x} advanced used index from {last} to {used} \
                        beyond its {QUEUE_LEN} admitted descriptors; endpoint was retired")
            }
            Self::Completion { base,
                               id,
                               bytes } =>
            {
                write!(f,
                       "input device at {base:#x} returned invalid completion id={id}, \
                        bytes={bytes}; expected id below {QUEUE_LEN} and {EVENT_BYTES} bytes; \
                        endpoint was retired before reading event storage")
            }
            Self::Duplicate { base,
                              id } =>
            {
                write!(f,
                       "input device at {base:#x} returned descriptor {id} twice before recycling \
                        was published; endpoint was retired")
            }
            Self::DeviceCount { found } =>
            {
                write!(f,
                       "terminal input found {found} modern input devices; this platform requires \
                        {DEVICE_COUNT}; queues were not published")
            }
            Self::NotPrepared =>
            {
                f.write_str("terminal input was not admitted during bootstrap; normal input \
                             cannot be borrowed for emergency use")
            }
            Self::AlreadyPrepared =>
            {
                f.write_str("terminal input is already admitted; retain the original prepared \
                             storage")
            }
            Self::AlreadyTaken =>
            {
                f.write_str("terminal input takeover has already consumed its unique preparation; \
                             normal input must not resume")
            }
        }
    }
}
impl core::error::Error for Error
{
}

/// Terminal takeover owns a fixed pair of endpoints. Retirement is permanent:
/// preserve its first cause until reported and keep an unaffected peer usable.
pub(crate) struct TerminalEndpoints
{
    live:    [bool; DEVICE_COUNT],
    pending: [Option<Error>; DEVICE_COUNT]
}
impl TerminalEndpoints
{
    pub(crate) fn from_admission(results: [Result<(), Error>; DEVICE_COUNT]) -> Self
    {
        Self { live:    results.map(|result| result.is_ok()),
               pending: results.map(Result::err) }
    }

    pub(crate) fn is_live(&self, endpoint: usize) -> bool
    {
        self.live[endpoint]
    }

    pub(crate) fn has_live_device(&self) -> bool
    {
        self.live.iter().any(|live| *live)
    }

    pub(crate) fn retire(&mut self, endpoint: usize, cause: Error)
    {
        if self.live[endpoint]
        {
            self.live[endpoint] = false;
            self.pending[endpoint] = Some(cause);
        }
    }

    pub(crate) fn take_errors(&mut self) -> [Option<Error>; DEVICE_COUNT]
    {
        core::mem::replace(&mut self.pending, [None; DEVICE_COUNT])
    }
}

pub fn identity(base: usize, magic: u32, version: u32, device: u32) -> Result<(), Error>
{
    if magic != 0x7472_6976 || version != 2 || device != 18
    {
        return Err(Error::Identity { base,
                                     magic,
                                     version,
                                     device });
    }
    Ok(())
}

pub fn used_count(base: usize, last: u16, used: u16) -> Result<usize, Error>
{
    let count = usize::from(used.wrapping_sub(last));
    if count > QUEUE_LEN
    {
        Err(Error::UsedAdvance { base,
                                 last,
                                 used })
    }
    else
    {
        Ok(count)
    }
}

pub fn version_feature(base: usize, features: u32) -> Result<(), Error>
{
    if features & 1 == 0 { Err(Error::VersionFeature { base }) } else { Ok(()) }
}

pub fn features(base: usize, status: u32) -> Result<(), Error>
{
    if status & 8 == 0 || status & (64 | 128) != 0
    {
        Err(Error::Features { base,
                              status })
    }
    else
    {
        Ok(())
    }
}

pub fn queue(base: usize, ready: u32, maximum: u32) -> Result<(), Error>
{
    if ready != 0
    {
        return Err(Error::QueueReady { base,
                                       ready });
    }
    if maximum < QUEUE_LEN as u32
    {
        return Err(Error::QueueSize { base,
                                      maximum });
    }
    Ok(())
}

pub fn axis(base: usize, axis: u8, minimum: u32, maximum: u32) -> Result<(), Error>
{
    if minimum != 0 || maximum != 32767
    {
        Err(Error::Axis { base,
                          axis,
                          minimum,
                          maximum })
    }
    else
    {
        Ok(())
    }
}

pub fn completion(base: usize, id: u32, bytes: u32, recycled: u64) -> Result<usize, Error>
{
    if id >= QUEUE_LEN as u32 || bytes != EVENT_BYTES
    {
        return Err(Error::Completion { base,
                                       id,
                                       bytes });
    }
    if recycled & (1u64 << id) != 0
    {
        return Err(Error::Duplicate { base,
                                      id });
    }
    Ok(id as usize)
}
