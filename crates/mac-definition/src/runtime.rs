//! Platform panic entry for native definition binaries. Formatting is bounded
//! and allocation-free. The System authenticates the current invocation; this
//! runtime neither claims privileged unwind nor owns a recovery UI/event loop.
use core::fmt::{self, Write};
#[cfg(systemrisc)]
use core::panic::PanicInfo;

use mac_abi::definitions::DefinitionPanic;
#[cfg(target_arch = "riscv64")]
mod riscv64;
#[cfg(target_arch = "aarch64")]
mod aarch64;
#[cfg(target_arch = "riscv64")]
use riscv64 as arch;
#[cfg(target_arch = "aarch64")]
use aarch64 as arch;
struct Diagnostic
{
    report: DefinitionPanic
}
impl Write for Diagnostic
{
    fn write_str(&mut self, text: &str) -> fmt::Result
    {
        let start = self.report.length as usize;
        let mut count = text.len().min(self.report.message.len() - start);
        while !text.is_char_boundary(count)
        {
            count -= 1;
        }
        self.report.message[start..start + count].copy_from_slice(&text.as_bytes()[..count]);
        self.report.length += count as u32;
        if count != text.len()
        {
            self.report.flags |= DefinitionPanic::TRUNCATED;
        }
        Ok(())
    }
}
#[cfg(systemrisc)]
pub fn panic(info: &PanicInfo<'_>) -> !
{
    let mut diagnostic = Diagnostic { report: DefinitionPanic::default() };
    if write!(diagnostic, "{info}").is_err()
    {
        diagnostic.report.flags |= DefinitionPanic::FORMAT_FAILED;
    }
    arch::report(&mut diagnostic.report)
}

