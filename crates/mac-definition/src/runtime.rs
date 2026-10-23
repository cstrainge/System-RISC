//! Platform panic entry for native definition binaries. Formatting is bounded
//! and allocation-free. The System authenticates the current invocation; this
//! runtime neither claims privileged unwind nor owns a recovery UI/event loop.
use core::fmt::{self, Write};
#[cfg(target_arch = "riscv64")]
use core::panic::PanicInfo;

use mac_abi::definitions::DefinitionPanic;
#[cfg(target_arch = "riscv64")]
mod riscv64;
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
#[cfg(target_arch = "riscv64")]
pub fn panic(info: &PanicInfo<'_>) -> !
{
    let mut diagnostic = Diagnostic { report: DefinitionPanic::default() };
    if write!(diagnostic, "{info}").is_err()
    {
        diagnostic.report.flags |= DefinitionPanic::FORMAT_FAILED;
    }
    riscv64::report(&mut diagnostic.report)
}

#[cfg(test)]
mod tests
{
    use super::*;
    #[test]
    fn bounded_diagnostic_preserves_utf8_and_reports_truncation()
    {
        let mut diagnostic = Diagnostic { report: DefinitionPanic::default() };
        diagnostic.write_str(core::str::from_utf8(&[b'x'; 1023]).unwrap()).unwrap();
        diagnostic.write_str("é").unwrap();
        assert_eq!(diagnostic.report.length, 1023);
        assert_eq!(diagnostic.report.flags, DefinitionPanic::TRUNCATED);
        assert!(core::str::from_utf8(&diagnostic.report.message[..1023]).is_ok());
        diagnostic.write_str("!").unwrap();
        assert_eq!(diagnostic.report.length, 1024);
        diagnostic.write_str("overflow").unwrap();
        assert_eq!(diagnostic.report.length, 1024);
        assert_eq!(diagnostic.report.flags, DefinitionPanic::TRUNCATED);
    }
}
