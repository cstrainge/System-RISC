//! Shared bootloader logic: everything a bootstrap needs that is not the
//! architecture's own entry, page tables or handoff.
//!
//! Each platform keeps its entry code, its page-table construction and its
//! supervisor jump, because those are the instructions and register forms of
//! one architecture. Discovery, the virtio transports and the volume walk are
//! the same work on any board, so they live here and both bootloaders call
//! them rather than keeping a copy each.
#![no_std]
extern crate alloc;

use core::sync::atomic::{AtomicPtr, Ordering};

pub mod completion;
pub mod image_regions;
pub mod virtio_blk;
pub mod volume;

/// Where shared diagnostics go. The bootstrap installs its own console before
/// any device work, so this code never names a UART.
static SINK: AtomicPtr<()> = AtomicPtr::new(core::ptr::null_mut());

pub fn install_log(sink: fn(core::fmt::Arguments<'_>))
{
    SINK.store(sink as *mut (), Ordering::Release);
}

pub fn log(arguments: core::fmt::Arguments<'_>)
{
    let raw = SINK.load(Ordering::Acquire);
    if raw.is_null()
    {
        return;
    }
    // SAFETY: `install_log` is the only writer and stores a `fn` pointer of
    // exactly this type; the null case returned above.
    let sink: fn(core::fmt::Arguments<'_>) = unsafe { core::mem::transmute(raw) };
    sink(arguments);
}

/// A `fmt::Write` over the installed console, so `writeln!` works in shared
/// code without it knowing which console that is.
pub struct Log;

impl core::fmt::Write for Log
{
    fn write_str(&mut self, text: &str) -> core::fmt::Result
    {
        log(core::format_args!("{text}"));
        Ok(())
    }
}
