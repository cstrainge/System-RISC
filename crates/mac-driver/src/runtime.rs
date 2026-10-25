//! Platform-owned driver panic path. No allocation, registry lookup or silent
//! spin: retain bounded diagnostics, then enter System's privileged-fault path.
//! Arbitrary privileged-fault recovery remains the unproven M-D04 gate.
#[cfg(target_arch = "riscv64")]
mod riscv64;
#[cfg(target_arch = "riscv64")]
pub fn panic(info: &core::panic::PanicInfo<'_>) -> !
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
    let failed = write!(&mut text, "DRIVER_PANIC_FATAL: {info}\n").is_err();
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
