//! Private diagnostic provider over real PCTL and FSYS dispatch. No System
//! export or host service substitute participates. The read-only control file
//! starts a common future deadline while ordinary admission is still healthy.
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use mac_driver::{ClassId, Client, Completion, Driver, Error, Phase, Request, Result, sync::Mutex};
use mac_filesystem_interface::{self as fs, Arguments, Metadata, Operation};

use crate::Device;
const COUNT: usize = 44;
const SECOND: u64 = 10_000_000;
static MODULES: [AtomicU64; COUNT] = [const { AtomicU64::new(0) }; COUNT];
static REGISTERED: AtomicU64 = AtomicU64::new(0);
static START: AtomicU64 = AtomicU64::new(0);
static VOLUME: AtomicU32 = AtomicU32::new(0);
// Tokens below are supplied by the real File Manager, never issued here.
static OPEN: Mutex<[(u64, u32); 8]> = Mutex::new([(0, 0); 8]);
fn now() -> u64
{
    let value;
    // SAFETY: same private QEMU 10 MHz counter edge as the native worker fixture.
    unsafe {
        core::arch::asm!("rdtime {}", out(reg) value, options(nomem, nostack));
    }
    value
}
fn failure(_: fs::Error) -> Error
{
    Error(mac_abi::BAD_ARGUMENT)
}
fn reply(output: &mut [u8]) -> Result<usize>
{
    let out = output.get_mut(..32).ok_or(Error(mac_abi::BAD_ARGUMENT))?;
    let start = START.load(Ordering::Acquire);
    for (index, word) in [REGISTERED.load(Ordering::Acquire),
                          start,
                          if start == 0 { 0 } else { start + 20 * SECOND },
                          if start == 0 { 0 } else { start + 240 * SECOND }].into_iter()
                                                                            .enumerate()
    {
        out[index * 8..index * 8 + 8].copy_from_slice(&word.to_le_bytes());
    }
    Ok(32)
}
fn metadata(id: u32) -> Result<Metadata<'static>>
{
    let (name, directory, parent, length) = match id
    {
        2 => ("Pressure", true, 0, 0),
        3 => ("Begin", false, 2, 32),
        4 => ("Status", false, 2, 32),
        _ => return Err(Error(mac_abi::NOT_FOUND))
    };
    Ok(Metadata { id,
                  parent,
                  directory,
                  data_length: length,
                  resource_length: 0,
                  created: 0,
                  modified: 0,
                  backup: None,
                  child_count: if directory { 2 } else { 0 },
                  attributes: 0,
                  finder_info: [0; 32],
                  revision: 1,
                  name,
                  comment: "" })
}
impl Driver for Device
{
    fn initialize(&self, _: Client) -> Result<()>
    {
        Ok(())
    }

    fn request(&self, request: Request<'_>, output: &mut [u8]) -> Result<Completion>
    {
        if request.phase != Phase::Start
        {
            return Err(Error(mac_abi::BAD_ARGUMENT));
        }
        let count = if request.class == ClassId(*b"PCTL")
        {
            match request.operation
            {
                1 =>
                {
                    let input: [u8; 16] =
                        request.input.try_into().map_err(|_| Error(mac_abi::BAD_ARGUMENT))?;
                    let module = u64::from_le_bytes(input[..8].try_into().unwrap());
                    let ordinal = u64::from_le_bytes(input[8..].try_into().unwrap()) as usize;
                    let slot = MODULES.get(ordinal).ok_or(Error(mac_abi::BAD_ARGUMENT))?;
                    if module == 0
                    {
                        return Err(Error(mac_abi::BAD_ARGUMENT));
                    }
                    match slot.compare_exchange(0, module, Ordering::AcqRel, Ordering::Acquire)
                    {
                        Ok(_) =>
                        {
                            REGISTERED.fetch_add(1, Ordering::AcqRel);
                        }
                        Err(old) if old == module => (),
                        Err(_) => return Err(Error(mac_abi::ALREADY_EXISTS))
                    }
                }
                2 if request.input.is_empty() => (),
                _ => return Err(Error(mac_abi::BAD_ARGUMENT))
            }
            reply(output)?
        }
        else if request.class == ClassId(fs::CLASS)
        {
            let operation = Operation::try_from(request.operation).map_err(failure)?;
            let args = Arguments::decode(request.input).map_err(failure)?;
            if !matches!(operation, Operation::Exports | Operation::Mount)
               && args.volume != VOLUME.load(Ordering::Acquire)
            {
                return Err(Error(mac_abi::NOT_FOUND));
            }
            match operation
            {
                Operation::Exports =>
                {
                    output.get_mut(..4)
                          .ok_or(Error(mac_abi::BAD_ARGUMENT))?
                          .copy_from_slice(&1u32.to_le_bytes());
                    4
                }
                Operation::Mount =>
                {
                    VOLUME.compare_exchange(0, args.volume, Ordering::AcqRel, Ordering::Acquire)
                          .map_err(|_| Error(mac_abi::ALREADY_EXISTS))?;
                    fs::Mount { capabilities:       fs::READ,
                                filesystem:         *b"PRSS",
                                filesystem_version: 1,
                                capacity:           None,
                                root:               metadata(2)? }.encode(output)
                                                                  .map_err(failure)?
                }
                Operation::Catalog => metadata(args.node)?.encode(output).map_err(failure)?,
                Operation::Lookup =>
                {
                    if args.parent != 2
                    {
                        return Err(Error(mac_abi::NOT_FOUND));
                    }
                    let id = match args.name().map_err(failure)?
                    {
                        "Begin" => 3,
                        "Status" => 4,
                        _ => return Err(Error(mac_abi::NOT_FOUND))
                    };
                    metadata(id)?.encode(output).map_err(failure)?
                }
                Operation::Scan =>
                {
                    if args.parent != 2
                    {
                        return Err(Error(mac_abi::NOT_FOUND));
                    }
                    let id = match args.offset
                    {
                        0 => Some(3),
                        1 => Some(4),
                        _ => None
                    };
                    output.get_mut(..4)
                          .ok_or(Error(mac_abi::BAD_ARGUMENT))?
                          .copy_from_slice(&if id.is_some()
                                            {
                                                fs::SCAN_ENTRY
                                            }
                                            else
                                            {
                                                fs::SCAN_DONE
                                            }.to_le_bytes());
                    if let Some(id) = id
                    {
                        4 + metadata(id)?.encode(&mut output[4..]).map_err(failure)?
                    }
                    else
                    {
                        4
                    }
                }
                Operation::Open =>
                {
                    if args.node != 3 && args.node != 4 || args.token == 0
                    {
                        return Err(Error(mac_abi::BAD_ARGUMENT));
                    }
                    let out = output.get_mut(..16).ok_or(Error(mac_abi::BAD_ARGUMENT))?;
                    OPEN.with(|slots| {
                            let slot = slots.iter_mut()
                                            .find(|(token, _)| *token == 0)
                                            .ok_or(Error(mac_abi::SERVICE_BUSY))?;
                            *slot = (args.token, args.node);
                            Ok::<_, Error>(())
                        })??;
                    out[..8].copy_from_slice(&32u64.to_le_bytes());
                    out[8..].fill(0);
                    16
                }
                Operation::Read =>
                {
                    let node = OPEN.with(|slots| {
                                       slots.iter()
                                            .find(|(token, _)| *token == args.token)
                                            .map(|(_, node)| *node)
                                            .ok_or(Error(mac_abi::NOT_FOUND))
                                   })??;
                    if args.flags == fs::RESOURCE_FORK
                    {
                        return Ok(Completion::Complete(0));
                    }
                    if args.flags != fs::DATA_FORK
                    {
                        return Err(Error(mac_abi::BAD_ARGUMENT));
                    }
                    if node == 3 && args.offset == 0 && args.length != 0
                    {
                        if REGISTERED.load(Ordering::Acquire) != COUNT as u64
                        {
                            return Err(Error(mac_abi::SERVICE_BUSY));
                        }
                        let _ = START.compare_exchange(0,
                                                       now() + 5 * SECOND,
                                                       Ordering::AcqRel,
                                                       Ordering::Acquire);
                    }
                    let mut bytes = [0; 32];
                    reply(&mut bytes)?;
                    let from = usize::try_from(args.offset).unwrap_or(usize::MAX).min(bytes.len());
                    let count =
                        usize::try_from(args.length).unwrap_or(usize::MAX).min(bytes.len() - from);
                    output.get_mut(..count)
                          .ok_or(Error(mac_abi::BAD_ARGUMENT))?
                          .copy_from_slice(&bytes[from..from + count]);
                    count
                }
                Operation::Close =>
                {
                    OPEN.with(|slots| {
                            let slot = slots.iter_mut()
                                            .find(|(token, _)| *token == args.token)
                                            .ok_or(Error(mac_abi::NOT_FOUND))?;
                            *slot = (0, 0);
                            Ok::<_, Error>(())
                        })??;
                    0
                }
                Operation::Unmount =>
                {
                    if OPEN.with(|slots| slots.iter().any(|(token, _)| *token != 0))?
                    {
                        return Err(Error(mac_abi::SERVICE_BUSY));
                    }
                    VOLUME.store(0, Ordering::Release);
                    0
                }
                Operation::Refresh => 0,
                Operation::CheckDirectory =>
                {
                    let out = output.get_mut(..33).ok_or(Error(mac_abi::BAD_ARGUMENT))?;
                    out.fill(0);
                    33
                }
                _ => return Err(Error(mac_abi::READ_ONLY))
            }
        }
        else
        {
            return Err(Error(mac_abi::NOT_SUPPORTED));
        };
        Ok(Completion::Complete(count))
    }
}
