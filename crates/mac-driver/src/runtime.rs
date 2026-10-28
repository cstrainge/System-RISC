//! Platform-owned driver panic path. No allocation, registry lookup or silent
//! spin: retain bounded diagnostics, then enter System's privileged-fault path.
//! Arbitrary privileged-fault recovery remains the unproven M-D04 gate.
#[cfg(target_arch = "riscv64")]
mod riscv64;
/// Resident allocator forwarding and synchronous retained-state selection.
pub mod allocation;
pub mod workers;
mod scope;

pub(crate) fn critical_section<R>(operation: impl FnOnce() -> R) -> crate::Result<R>
{
    let host = allocation::host().ok_or(crate::Error(mac_abi::NOT_SUPPORTED))?;
    let callback = host.critical_section.ok_or(crate::Error(mac_abi::NOT_SUPPORTED))?;
    scope::invoke(host.context, callback, operation)
}

#[cfg(target_arch = "riscv64")]
pub fn panic(info: &core::panic::PanicInfo<'_>) -> !
{
    fatal(format_args!("DRIVER_PANIC_FATAL: {info}\n"))
}

/// An infallible allocation returned no storage. This does not identify budget
/// refusal, backing exhaustion or another cause that the allocator did not
/// report. Ordinary fallible allocation errors remain on their service paths.
pub fn allocation_failed(layout: core::alloc::Layout) -> !
{
    fatal(format_args!("DRIVER_ALLOCATION_FAILED_FATAL: requested_bytes={} alignment={}\n",
                       layout.size(),
                       layout.align()))
}

/// A resident callback violated the forwarding contract or refused disposal.
/// Continuing would lose the original allocation's receipt. Preserve the
/// observed cause separately from an ordinary allocation refusal.
pub(crate) fn allocation_fault(details: core::fmt::Arguments<'_>) -> !
{
    fatal(format_args!("DRIVER_ALLOCATOR_PROTOCOL_FATAL: {details}\n"))
}

/// A CREATE response with ambiguous ownership cannot be treated as failure.
pub(crate) fn worker_fault(details: core::fmt::Arguments<'_>) -> !
{
    fatal(format_args!("DRIVER_WORKER_PROTOCOL_FATAL: {details}\n"))
}

fn fatal(arguments: core::fmt::Arguments<'_>) -> !
{
    #[cfg(target_arch = "riscv64")]
    {
        diagnostic(arguments);
        riscv64::fault()
    }
    #[cfg(not(target_arch = "riscv64"))]
    {
        extern "C" fn unsupported() -> !
        {
            panic!("native driver fatal diagnostic on unsupported platform")
        }
        let _ = arguments;
        unsupported()
    }
}

/// One bounded formatter for panic/failure and retained cleanup diagnostics.
/// Native output allocates no memory and takes no SDK/resident registry lock.
pub(crate) fn diagnostic(arguments: core::fmt::Arguments<'_>)
{
    #[cfg(target_arch = "riscv64")]
    {
        use core::fmt::Write;
        struct Text
        {
            bytes:     [u8; 1024],
            length:    usize,
            truncated: bool
        }
        impl Write for Text
        {
            fn write_str(&mut self, value: &str) -> core::fmt::Result
            {
                let mut count = value.len().min(self.bytes.len() - self.length);
                while !value.is_char_boundary(count)
                {
                    count -= 1;
                }
                self.bytes[self.length..self.length + count].copy_from_slice(&value.as_bytes()
                                                                                 [..count]);
                self.length += count;
                self.truncated |= count != value.len();
                Ok(())
            }
        }
        let mut text = Text { bytes:     [0; 1024],
                              length:    0,
                              truncated: false };
        let failed = text.write_fmt(arguments).is_err();
        riscv64::log(&text.bytes[..text.length]);
        if text.truncated
        {
            riscv64::log(b"DRIVER_DIAGNOSTIC_TRUNCATED\n");
        }
        if failed
        {
            riscv64::log(b"DRIVER_DIAGNOSTIC_FORMAT_FAILED\n");
        }
    }
    #[cfg(not(target_arch = "riscv64"))]
    {
        let _ = arguments;
    }
}
