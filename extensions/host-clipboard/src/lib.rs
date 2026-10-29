//! Guest-only Host Clipboard extension over standard QEMU vdagent.
#![no_std]
#![forbid(unsafe_code)]
#![feature(allocator_ext)]
extern crate alloc;
mod io;
mod pump;

use mac_abi::{NOT_SUPPORTED, SERVICE_BUSY};
use mac_byte_channel::ByteChannel;
use mac_driver::{Client, Completion, Driver, Error, Request, Result, lifecycle::Startup,
                 preferences::Preferences, sync::Mutex};
use mac_driver_alloc::{Worker, Workers, scrap::Scrap};

static WORKER: Mutex<Option<Worker>> = Mutex::new(None);
pub struct Extension;
pub static EXTENSION: Extension = Extension;
impl Driver for Extension
{
    fn initialize(&self, client: Client) -> Result<()>
    {
        if WORKER.with(|worker| worker.is_some())?
        {
            return Err(Error(SERVICE_BUSY));
        }
        let startup = Startup::defer(client)?;
        let channel = ByteChannel::resolve(client)?;
        let preferences = Preferences::new(client)?;
        let scrap = client.with_module_allocation(|| Scrap::new(client))?
                          .map_err(|cause| Error(cause.status()))?;
        let prepared = client.prepare_worker("Host Clipboard", move |context| {
                                 pump::run(context, startup, preferences, channel, scrap)
                             })
                             .map_err(|failure| failure.cause)?;
        let worker = prepared.start().map_err(|failure| failure.cause)?;
        // The controller is published only after real worker admission. No
        // start, park or provider callback occurs under this short mutex.
        WORKER.with(|slot| *slot = Some(worker))?;
        Ok(())
    }

    fn request(&self, _: Request<'_>, _: &mut [u8]) -> Result<Completion>
    {
        Err(Error(NOT_SUPPORTED))
    }

    fn quiesce(&self) -> Result<()>
    {
        let Some(worker) = WORKER.with(Option::take)?
        else
        {
            return Ok(());
        };
        let result = worker.try_join().and_then(|result| {
                                          if let Some(result) = result
                                          {
                                              result
                                          }
                                          else
                                          {
                                              worker.cancel()?;
                                              Err(Error(SERVICE_BUSY))
                                          }
                                      });
        if result.is_err()
        {
            WORKER.with(|slot| *slot = Some(worker))?;
        }
        result
    }
}
