//! Platform-owned driver panic path. No allocation, registry lookup or silent
//! spin: retain bounded diagnostics, then enter System's privileged-fault path.
//! Arbitrary privileged-fault recovery remains the unproven M-D04 gate.
#[cfg(target_arch = "riscv64")]
mod riscv64;
/// Resident allocator forwarding and synchronous retained-state selection.
pub mod allocation;

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

/// One bounded, allocation-free formatter for native terminal diagnostics.
fn fatal(arguments: core::fmt::Arguments<'_>) -> !
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
        riscv64::fault()
    }
    #[cfg(not(target_arch = "riscv64"))]
    {
        // This architecture has no driver fatal backend. Keep the existing
        // refusal terminal without allowing an unwind through GlobalAlloc.
        extern "C" fn unsupported() -> !
        {
            panic!("native driver fatal diagnostic on unsupported platform")
        }
        let _ = arguments;
        unsupported()
    }
}
