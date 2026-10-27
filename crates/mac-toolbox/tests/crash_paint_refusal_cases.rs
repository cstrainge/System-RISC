//! Real native failure-UI assertions; no resident fault hook or private API.
#![cfg(target_arch = "riscv64")]
use core::fmt::Write;

use mac_toolbox::{Error, Rect, System, crashes::ReportFlags, events::EventMask};
const PANIC_LABELS: i16 = -16400;
const REPORT: &str = "Paint transaction refusal must preserve this complete report.\nCafé\tsecond \
                      column\nEND_OF_REPORT";
struct Text
{
    bytes:  [u8; 512],
    length: usize
}
impl Text
{
    const fn new() -> Self
    {
        Self { bytes:  [0; 512],
               length: 0 }
    }

    fn as_str(&self) -> &str
    {
        core::str::from_utf8(&self.bytes[..self.length]).unwrap()
    }
}
impl Write for Text
{
    fn write_str(&mut self, text: &str) -> core::fmt::Result
    {
        let end = self.length.checked_add(text.len()).ok_or(core::fmt::Error)?;
        self.bytes
            .get_mut(self.length..end)
            .ok_or(core::fmt::Error)?
            .copy_from_slice(text.as_bytes());
        self.length = end;
        Ok(())
    }
}
pub fn assertion_failed(system: &System<'_>, info: &core::panic::PanicInfo<'_>)
{
    let _ = system.log("CRASH_PAINT_FAIL assertion\n");
    let mut text = Text::new();
    let _ = writeln!(&mut text, "{info}");
    let _ = system.log(text.as_str());
}
fn checkpoint(system: &System<'_>, stage: &str, ack: &str) -> Result<(), Error>
{
    // Crash input consumes keyDown while its report has focus. Its physical
    // key map remains public, so acknowledge a held Space without changing focus.
    system.next_event(EventMask::from_bits_retain(0))?;
    assert!(!system.events().keys()?.is_down(0x31));
    system.log(stage)?;
    let start = system.events().uptime_ticks()?;
    while !system.events().keys()?.is_down(0x31)
    {
        assert!(system.events().uptime_ticks()? - start < 1800,
                "host did not acknowledge checkpoint");
        system.wait_event(EventMask::from_bits_retain(0), 1)?;
    }
    system.log(ack)?;
    while system.events().keys()?.is_down(0x31)
    {
        assert!(system.events().uptime_ticks()? - start < 1800, "host left checkpoint key held");
        system.wait_event(EventMask::from_bits_retain(0), 1)?;
    }
    Ok(())
}
pub fn run(system: &System<'_>) -> Result<(), Error>
{
    system.log("DESKTOP_READY: crash paint refusal userspace regression\n")?;
    let mut label = [0; 256];
    assert_eq!(system.resources().system().indexed_string(PANIC_LABELS, 0, &mut label)?,
               "Application panic");
    assert_eq!(system.resources().system().indexed_string(PANIC_LABELS, 9, &mut label),
               Err(Error::NoMemory));
    let reports = system.crashes();
    reports.begin()?;
    reports.append(REPORT)?;
    let baseline = reports.show(ReportFlags::NONE)?;
    assert_ne!(baseline.window.0, 0);
    let content = system.windows().info(baseline.window)?.bounds;
    let mut text = Text::new();
    writeln!(&mut text,
             "CRASH_PAINT_CONTENT {} {} {} {}",
             content.left, content.top, content.right, content.bottom).map_err(|_| {
                                                                          Error::BadArgument
                                                                      })?;
    system.log(text.as_str())?;
    checkpoint(system, "CRASH_PAINT_STAGE baseline\n", "CRASH_PAINT_ACK baseline\n")?;

    let refused = reports.show(ReportFlags::TRUNCATED);
    assert_eq!(refused, Err(Error::NoMemory), "oversized warning must preserve typed exhaustion");
    let info = reports.info()?;
    assert_eq!(info.window, baseline.window);
    assert_eq!(info.report_bytes, REPORT.len());
    assert!(info.flags.contains(ReportFlags::TRUNCATED));
    assert_eq!(system.windows().info(info.window)?.bounds, content);
    text.length = 0;
    write!(&mut text, "{}", Error::NoMemory).map_err(|_| Error::BadArgument)?;
    assert!(text.as_str().contains("memory"), "refusal needs an informative diagnostic");
    text.length = 0;
    writeln!(&mut text,
             "CRASH_PAINT_REFUSED status={} report=retained window=live",
             Error::NoMemory.status()).map_err(|_| Error::BadArgument)?;
    system.log(text.as_str())?;
    checkpoint(system, "CRASH_PAINT_STAGE refused\n", "CRASH_PAINT_ACK refused\n")?;

    // A leaked full BeginUpdate would reject this independent transaction on
    // the same window. Drop must discard these vivid uncommitted pixels too.
    let graphics = system.quickdraw().graphics(info.window);
    {
        let _pending = graphics.masked_rectangles(&[Rect::new(8, 8, 40, 40)])?;
        graphics.colors(0xff00ff, 0)?;
        graphics.paint_rect(Rect::new(0,
                                      0,
                                      content.right - content.left,
                                      content.bottom - content.top))?;
    }
    assert_eq!(system.windows().info(info.window)?.bounds, content);
    checkpoint(system, "CRASH_PAINT_STAGE after-mask\n", "CRASH_PAINT_ACK after-mask\n")?;

    reports.begin()?;
    assert_eq!(system.windows().info(baseline.window), Err(Error::NotFound));
    reports.append(REPORT)?;
    let retried = reports.show(ReportFlags::NONE)?;
    assert_ne!(retried.window.0, 0);
    assert_ne!(retried.window, baseline.window);
    assert_eq!(retried.report_bytes, REPORT.len());
    assert_eq!(retried.flags, ReportFlags::NONE);
    assert_eq!(system.windows().info(retried.window)?.bounds, content);
    let mut bytes = [0; REPORT.len()];
    assert_eq!(reports.read(0, &mut bytes)?, REPORT.len());
    assert_eq!(&bytes, REPORT.as_bytes());
    checkpoint(system, "CRASH_PAINT_STAGE retry\n", "CRASH_PAINT_ACK retry\n")?;
    system.log("CRASH_PAINT_USERSPACE_PASS full_cancel=passed masked_retry=passed \
                reset_retry=passed\n")?;
    loop
    {
        system.wait_event(EventMask::from_bits_retain(0), 30)?;
    }
}
