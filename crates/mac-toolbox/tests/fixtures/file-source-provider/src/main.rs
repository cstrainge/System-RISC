//! Test-only filesystem: exposes the backing bytes as Payload. No fabricated
//! production StuffIt support; real class callbacks exercise resident leases.
#![no_std]
#![no_main]
use mac_driver::{Client, Completion, Driver, Error, Phase, Request, Result,
                 file_sources::FileSources};
use mac_filesystem_interface::{self as fs, Arguments, Metadata, Operation};
use spinning_top::Spinlock;

#[derive(Clone, Copy)]
struct Mount
{
    volume: u32,
    source: u32,
    length: u64
}
#[derive(Clone, Copy)]
struct Open
{
    volume: u32,
    token:  u32
}
struct State
{
    sources: Option<FileSources>,
    mounts:  [Option<Mount>; 16],
    opens:   [Option<Open>; 16],
    retired: u32
}
static STATE: Spinlock<State> = Spinlock::new(State { sources: None,
                                                      mounts:  [None; 16],
                                                      opens:   [None; 16],
                                                      retired: 0 });
struct Filesystem;
mac_driver::export_driver!(Filesystem);
#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> !
{
    loop
    {
        core::hint::spin_loop();
    }
}
fn invalid<T>() -> Result<T>
{
    Err(Error(-1))
}
fn metadata(mount: Mount, file: bool) -> Metadata<'static>
{
    let mut finder_info = [0; 32];
    if file
    {
        finder_info[..4].copy_from_slice(b"TEXT");
    }
    Metadata { id: if file { 16 } else { 2 },
               parent: if file { 2 } else { 0 },
               directory: !file,
               data_length: if file { mount.length } else { 0 },
               resource_length: 0,
               created: 0,
               modified: 0,
               backup: None,
               child_count: if file { 0 } else { 1 },
               attributes: 0,
               finder_info,
               revision: 1,
               name: if file { "Payload" } else { "Leased Source" },
               comment: "" }
}
impl Driver for Filesystem
{
    fn initialize(&self, client: Client) -> Result<()>
    {
        let sources = FileSources::new(client)?;
        STATE.lock().sources = Some(sources);
        Ok(())
    }

    fn request(&self, call: Request<'_>, output: &mut [u8]) -> Result<Completion>
    {
        if call.phase != Phase::Start || call.class.0 != fs::CLASS
        {
            return Err(Error(-2));
        }
        let op = Operation::try_from(call.operation).map_err(|_| Error(-2))?;
        let args = Arguments::decode(call.input).map_err(|_| Error(-2))?;
        let sources = STATE.lock().sources.ok_or(Error(-1))?;
        if op == Operation::Mount
        {
            let source = u32::try_from(args.auxiliary).map_err(|_| Error(-2))?;
            let length = sources.info(source)?.length;
            if length != 131089
            {
                return Err(Error(-2));
            }
            let retired = STATE.lock().retired;
            if retired != 0 && sources.info(retired) != Err(Error(-3))
            {
                return invalid();
            }
            // Validate both ends independently; ranged reads are positional.
            for offset in [0, length - 17]
            {
                let mut bytes = [0; 17];
                let mut ticket = 0;
                if sources.read(source, offset, &mut ticket, Phase::Start, &mut bytes)?
                   != Completion::Complete(bytes.len())
                {
                    return invalid();
                }
                for (i, byte) in bytes.into_iter().enumerate()
                {
                    if byte != ((offset as usize + i) * 31 + 7) as u8
                    {
                        return invalid();
                    }
                }
            }
            let mount = Mount { volume: args.volume,
                                source,
                                length };
            let descriptor = fs::Mount { capabilities:       fs::READ,
                                         filesystem:         *b"TEST",
                                         filesystem_version: 1,
                                         capacity:           None,
                                         root:               metadata(mount, false) };
            let count = descriptor.encode(output).map_err(|_| Error(-2))?;
            let mut state = STATE.lock();
            *state.mounts.iter_mut().find(|m| m.is_none()).ok_or(Error(-5))? = Some(mount);
            return Ok(Completion::Complete(count));
        }
        let mount = STATE.lock()
                         .mounts
                         .iter()
                         .flatten()
                         .find(|m| m.volume == args.volume)
                         .copied()
                         .ok_or(Error(-3))?;
        let count = match op
        {
            Operation::Catalog =>
            {
                if ![2, 16].contains(&args.node)
                {
                    return Err(Error(-3));
                }
                metadata(mount, args.node == 16).encode(output).map_err(|_| Error(-2))?
            }
            Operation::Lookup =>
            {
                if args.parent != 2 || !args.payload.eq_ignore_ascii_case(b"Payload")
                {
                    return Err(Error(-3));
                }
                metadata(mount, true).encode(output).map_err(|_| Error(-2))?
            }
            Operation::Scan =>
            {
                if args.parent != 2
                {
                    return Err(Error(-2));
                }
                output.get_mut(..4).ok_or(Error(-2))?.copy_from_slice(&(if args.offset == 0
                                                                       {
                                                                           fs::SCAN_ENTRY
                                                                       }
                                                                       else
                                                                       {
                                                                           fs::SCAN_DONE
                                                                       }).to_le_bytes());
                if args.offset == 0
                {
                    4 + metadata(mount, true).encode(&mut output[4..]).map_err(|_| Error(-2))?
                }
                else
                {
                    4
                }
            }
            Operation::Open =>
            {
                if args.node != 16
                {
                    return Err(Error(-3));
                }
                let bytes = output.get_mut(..16).ok_or(Error(-2))?;
                bytes[..8].copy_from_slice(&mount.length.to_le_bytes());
                bytes[8..].copy_from_slice(&0u64.to_le_bytes());
                let mut state = STATE.lock();
                *state.opens.iter_mut().find(|m| m.is_none()).ok_or(Error(-5))? =
                    Some(Open { volume: args.volume,
                                token:  args.token });
                16
            }
            Operation::Read =>
            {
                if !STATE.lock()
                         .opens
                         .iter()
                         .flatten()
                         .any(|m| m.volume == args.volume && m.token == args.token)
                {
                    return Err(Error(-3));
                }
                if args.flags != 0
                {
                    return Ok(Completion::Complete(0));
                }
                let capacity = usize::try_from(args.length).map_err(|_| Error(-2))?;
                let bytes = output.get_mut(..capacity).ok_or(Error(-2))?;
                let mut ticket = 0;
                return sources.read(mount.source, args.offset, &mut ticket, Phase::Start, bytes);
            }
            Operation::Close =>
            {
                let mut state = STATE.lock();
                *state.opens
                      .iter_mut()
                      .find(|m| m.is_some_and(|m| m.volume == args.volume && m.token == args.token))
                      .ok_or(Error(-3))? = None;
                0
            }
            Operation::Unmount =>
            {
                if STATE.lock().opens.iter().flatten().any(|m| m.volume == args.volume)
                {
                    return Err(Error(-7));
                }
                // The source must still be live inside the teardown callback.
                if sources.info(mount.source)?.length != mount.length
                {
                    return invalid();
                }
                let mut state = STATE.lock();
                *state.mounts
                      .iter_mut()
                      .find(|m| m.is_some_and(|m| m.volume == args.volume))
                      .ok_or(Error(-3))? = None;
                state.retired = mount.source;
                0
            }
            _ => return Err(Error(-4))
        };
        Ok(Completion::Complete(count))
    }
}
