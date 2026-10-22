//! Minimal loader boundary; canonical Rust assertions live beside SDK tests.
#![no_std]
#![no_main]
use core::sync::atomic::{AtomicPtr, Ordering};

use mac_toolbox::{System, SystemServices};
static SERVICES: AtomicPtr<SystemServices> = AtomicPtr::new(core::ptr::null_mut());
#[allow(dead_code)]
#[path = "../../../../../../apps/finder/src/browser.rs"]
mod browser;
#[allow(dead_code)]
#[path = "../../../../../../apps/finder/src/cache.rs"]
mod cache;
#[allow(dead_code)]
#[path = "../../../../../../apps/finder/src/selection.rs"]
mod selection;
#[allow(dead_code)]
#[path = "../../../../../../apps/finder/src/get_info/mod.rs"]
mod get_info;
#[allow(dead_code)]
#[path = "../../../../../../apps/finder/src/icon_view.rs"]
mod icon_view;
#[allow(dead_code)]
#[path = "../../../../../../apps/finder/src/ui.rs"]
mod ui;
#[allow(dead_code)]
#[path = "../../../../../../apps/finder/src/menus.rs"]
mod menus;
#[path = "../../../finder_selection_cases.rs"]
mod cases;
unsafe extern "C" fn observe(number: u32, pointer: *mut (), size: usize) -> i64
{
    // SAFETY: the public SDK lends a live request synchronously to its gate.
    unsafe {
        cases::observe(number, pointer, size);
        let status = ((*SERVICES.load(Ordering::Acquire)).trap)(number, pointer, size);
        if status >= 0
        {
            cases::observed(number, pointer, size);
        }
        status
    }
}
/// # Safety
/// Loader supplies a validated service table for this application lifetime.
#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.entry")]
pub unsafe extern "C" fn _start(services: *const SystemServices) -> i32
{
    SERVICES.store(services.cast_mut(), Ordering::Release);
    // SAFETY: preserve the loader's service table and forward every request.
    let original = unsafe { &*services };
    let traced = SystemServices { header:       original.header,
                                  trap:         observe,
                                  capabilities: original.capabilities };
    let system = match unsafe { System::from_raw(&traced) }
    {
        Ok(system) => system,
        Err(error) => return error.status()
    };
    if let Err(error) = cases::run(&system)
    {
        use core::fmt::Write;
        let mut message = browser::Text::<256>::default();
        let _ = writeln!(&mut message,
                         "FINDER_SELECTION_FAIL service: {error} ({error:?}, status={})",
                         error.status());
        let _ = system.log(message.as_str());
        return error.status();
    }
    0
}
#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> !
{
    loop
    {
        core::hint::spin_loop();
    }
}
