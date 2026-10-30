//! Disk-booted private application. Assertions are canonical aggregate test code;
//! this file supplies only the live userspace service table and outcome logging.
#![no_std]
#![no_main]
use core::{fmt::{self, Write},
           sync::atomic::{AtomicPtr, Ordering}};

use mac_toolbox::{System, SystemServices};
#[path = "../../../conformance/mod.rs"]
mod conformance;
mod reflow;
mod reflow_tree;
mod reflow_grid;
mod reflow_window;
mod table_lines;
mod reflow_automatic;
mod reflow_wrapped;
mod reflow_scroll;
static SERVICES: AtomicPtr<SystemServices> = AtomicPtr::new(core::ptr::null_mut());
struct LogBuffer
{
    bytes:  [u8; 512],
    length: usize
}
impl LogBuffer
{
    fn new() -> Self
    {
        Self { bytes:  [0; 512],
               length: 0 }
    }

    fn text(&self) -> &str
    {
        core::str::from_utf8(&self.bytes[..self.length]).unwrap_or("TOOLBOX_USERSPACE_FAIL \
                                                                    invalid diagnostic\n")
    }
}
impl Write for LogBuffer
{
    fn write_str(&mut self, text: &str) -> fmt::Result
    {
        let remaining = self.bytes.len() - self.length;
        let mut n = text.len().min(remaining);
        while !text.is_char_boundary(n)
        {
            n -= 1;
        }
        self.bytes[self.length..self.length + n].copy_from_slice(&text.as_bytes()[..n]);
        self.length += n;
        if n != text.len() { Err(fmt::Error) } else { Ok(()) }
    }
}
#[cfg(not(any(feature = "alert-tracking",
              feature = "memory-inspection",
              feature = "cpu-inspection",
              feature = "textedit-clipboard")))]
fn run(system: &System<'_>, gate: mac_abi::TrapGate) -> Result<(), conformance::Failure>
{
    reflow::run(system)?;
    conformance::managed_controls::regressions(system, gate)?;
    conformance::controls::author_families(system, gate)?;
    conformance::run_all(system)?;
    conformance::dates::filesystem(system)?;
    conformance::strings::shared(system, gate)?;
    conformance::scrap::legacy_text(system, gate)?;
    conformance::textedit::legacy(system, gate)?;
    conformance::strings::graphical(system, gate)?;
    conformance::ownership::graphical(system, gate)?;
    // Preserve the earlier sole-slot control reuse scenario before this grid
    // deliberately grows the registered pool's permanent generation history.
    reflow_tree::run(system)?;
    reflow_grid::run(system)?;
    reflow_window::run(system)?;
    table_lines::run(system)?;
    reflow_automatic::run(system)?;
    reflow_wrapped::run(system)?;
    reflow_scroll::run(system)?;
    Ok(())
}
#[cfg(feature = "textedit-clipboard")]
#[path = "../../../conformance/textedit_clipboard.rs"]
mod textedit_clipboard;
#[cfg(feature = "textedit-clipboard")]
fn run(system: &System<'_>, _gate: mac_abi::TrapGate) -> Result<(), conformance::Failure>
{
    textedit_clipboard::run(system)
}
#[cfg(feature = "memory-inspection")]
#[path = "../../../conformance/memory_inspection.rs"]
mod memory_inspection;
#[cfg(feature = "memory-inspection")]
fn run(system: &System<'_>, _gate: mac_abi::TrapGate) -> Result<(), conformance::Failure>
{
    memory_inspection::run(system)
}
#[cfg(feature = "cpu-inspection")]
#[path = "../../../conformance/cpu_inspection.rs"]
mod cpu_inspection;
#[cfg(feature = "cpu-inspection")]
fn run(system: &System<'_>, _gate: mac_abi::TrapGate) -> Result<(), conformance::Failure>
{
    cpu_inspection::run(system)
}
#[cfg(feature = "alert-tracking")]
fn run(system: &System<'_>, _gate: mac_abi::TrapGate) -> Result<(), conformance::Failure>
{
    conformance::ownership::alert_tracking(system, |name, bounds| {
        let mut text = LogBuffer::new();
        let _ = writeln!(text,
                         "ALERT_TRACKING_READY stage={name} bounds={},{},{},{}",
                         bounds.left, bounds.top, bounds.right, bounds.bottom);
        system.log(text.text())?;
        loop
        {
            let event = system.events().wait_event(mac_toolbox::EventMask::ALL, 1)?;
            if event.as_record().kind == mac_abi::EventKind::KeyDown
               && event.as_record().character() == b' '
            {
                break;
            }
        }
        Ok(())
    })
}
/// # Safety
/// The loader supplies a live table that remains valid throughout the application.
#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.entry")]
pub unsafe extern "C" fn _start(services: *const SystemServices) -> mac_toolbox::abi::Status
{
    let system = match unsafe { System::from_raw(services) }
    {
        Ok(s) => s,
        Err(e) => return e.status()
    };
    SERVICES.store(services.cast_mut(), Ordering::Release);
    let _ = system.log("DESKTOP_READY: aggregate public Toolbox conformance fixture\n");
    match run(&system, unsafe { (*services).trap })
    {
        Ok(()) =>
        {
            #[cfg(not(any(feature = "alert-tracking",
                          feature = "memory-inspection",
                          feature = "cpu-inspection",
                          feature = "textedit-clipboard")))]
            {
                let _ = system.log("TEXTEDIT_USERSPACE_PASS original=356 native=20 legacy=6 \
                                    navigation=49\n");
                let _ = system.log("SCRAP_USERSPACE_PASS original=18 native=17 legacy=12\n");
                let _ = system.log("TOOLBOX_USERSPACE_PASS geometry=20 ovals=267 handles=48 \
                                    controls=86 controlpixels=269600 controlhits=8424 \
                                    strings=passed dates=passed\n");
            }
            #[cfg(feature = "cpu-inspection")]
            let _ = system.log("TOOLBOX_USERSPACE_PASS cpu-inspection=passed\n");
            #[cfg(feature = "memory-inspection")]
            let _ = system.log("TOOLBOX_USERSPACE_PASS memory-mappings=passed\n");
            #[cfg(feature = "alert-tracking")]
            let _ = system.log("ALERT_TRACKING_USERSPACE_PASS cancel=passed release=passed \
                                cleanup=passed\n");
            0
        }
        Err(error) =>
        {
            let mut text = LogBuffer::new();
            let _ = writeln!(text, "TOOLBOX_USERSPACE_FAIL {error:?}");
            let _ = system.log(text.text());
            -1
        }
    }
}
#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> !
{
    let pointer = SERVICES.load(Ordering::Acquire);
    if let Ok(system) = unsafe { System::from_raw(pointer) }
    {
        let mut text = LogBuffer::new();
        let _ = writeln!(text, "TOOLBOX_USERSPACE_FAIL panic: {info}");
        let _ = system.log(text.text());
    }
    loop
    {
        core::hint::spin_loop();
    }
}
