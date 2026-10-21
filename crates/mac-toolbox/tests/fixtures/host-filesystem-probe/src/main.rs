//! Run only during private boot, before apps start. This verifies actual native
//! module callbacks and transport, not app scheduling or mounted File APIs.
#![no_std]
#![no_main]
use mac_driver::{ClassId, Client, Completion, Driver, Error, Phase, Request, Result};
use mac_filesystem_interface::{self as fs, Arguments, Operation};
struct Probe;
mac_driver::export_driver!(Probe);
impl Driver for Probe
{
    fn initialize(&self, host: Client) -> Result<()>
    {
        let provider = host.resolve(ClassId(fs::CLASS), fs::VERSION)?;
        let mut input = [0; fs::HEADER_BYTES];
        Arguments::default().encode(&mut input).map_err(|_| Error(-2))?;
        let mut ticket = 0;
        for _ in 0..1_000_000
        {
            let phase = if ticket == 0 { Phase::Start } else { Phase::Poll };
            let mut output = [0xa5; 4];
            match provider.invoke(&mut ticket,
                                  phase,
                                  Operation::Exports as u32,
                                  if phase == Phase::Start { &input } else { &[] },
                                  &mut output)
            {
                Ok(Completion::Complete(4)) if ticket == 0 && output == 1u32.to_le_bytes() =>
                {
                    return Ok(());
                }
                Ok(Completion::Pending) if ticket != 0 => (),
                Err(Error(-7)) => (),
                Err(error) => return Err(error),
                _ => return Err(Error(-1))
            }
            core::hint::spin_loop();
        }
        // The module remains pinned. Report the failed acceptance rather than
        // entering an unbounded boot loop on an unavailable transport.
        Err(Error(-9))
    }

    fn request(&self, _: Request<'_>, _: &mut [u8]) -> Result<Completion>
    {
        Err(Error(-4))
    }
}
#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> !
{
    loop
    {
        core::hint::spin_loop();
    }
}
