//! Real native trap refusals followed by signed explicit process exit.
use mac_toolbox::{System, ToolboxResult,
                  abi::{self, processes::NativeExitCall, tls}};

unsafe extern "C" fn unexpected_destructor(_: *mut u8)
{
    panic!("explicit process exit must not claim or execute user TLS cleanup");
}

fn malformed(mut call: NativeExitCall)
{
    let mut result = core::ptr::from_mut(&mut call).addr();
    // SAFETY: exclusive, aligned live request. Only scalar reserved/flag fields
    // are malformed; the production trap must refuse before closing admission.
    unsafe {
        core::arch::asm!("ecall", inlateout("a0") result,
            in("a1") core::mem::size_of::<NativeExitCall>(),
            in("a7") abi::traps::PROCESS, clobber_abi("C"));
    }
    assert_eq!(result as i64,
               i64::from(abi::BAD_ARGUMENT),
               "malformed native exit must return its typed argument refusal");
}

pub fn run(system: &System<'static>) -> ToolboxResult<()>
{
    let before = system.client().current_native_thread().expect("canonical entry thread");
    malformed(NativeExitCall { flags: u32::MAX,
                               ..Default::default() });
    malformed(NativeExitCall { reserved: 1,
                               ..Default::default() });
    assert_eq!(system.client().current_native_thread().expect("refusal preserves execution"),
               before);
    let mut create = tls::Call { operation: tls::CREATE,
                                 destructor: unexpected_destructor as *const () as usize,
                                 ..Default::default() };
    // SAFETY: callback has the required C signature and belongs to this image.
    unsafe { system.client().tls(&mut create) }.expect("refusal preserves TLS admission");
    let mut set = tls::Call { operation: tls::SET,
                              key: create.key,
                              value: 1,
                              ..Default::default() };
    // SAFETY: this opaque sentinel is never dereferenced by the callback.
    unsafe { system.client().tls(&mut set) }.expect("native TLS value publication");
    #[cfg(not(feature = "exit-late"))]
    {
        system.log("NATIVE_EXIT_PROBE malformed=true current=true tls_live=true code=-23\n")?;
        mac_toolbox::application::exit(-23)
    }
    #[cfg(feature = "exit-late")]
    {
        use mac_toolbox::crashes::{Crashes, ReportFlags};
        let reports = Crashes::new(system.client());
        reports.begin()?;
        reports.append("Native late-abort probe: dismiss this report before Abort is \
                        submitted.\n")?;
        reports.show(ReportFlags::default())?;
        system.log("NATIVE_LATE_ABORT_REPORT_SHOWN\n")?;
        while !reports.info()?.dismissed
        {
            system.client().wait_event(0, 60)?;
        }
        system.log("NATIVE_LATE_ABORT_AFTER_DISMISS code=101\n")?;
        let mut call = NativeExitCall { code:     101,
                                        flags:    abi::processes::NATIVE_EXIT_ABORT,
                                        reserved: 0 };
        let mut result = core::ptr::from_mut(&mut call).addr();
        // SAFETY: exact live ABI request, submitted by this actual Process.
        unsafe {
            core::arch::asm!("ecall", inlateout("a0") result,
                in("a1") core::mem::size_of::<NativeExitCall>(),
                in("a7") abi::traps::PROCESS, clobber_abi("C"));
        }
        panic!("accepted late Abort unexpectedly returned {result}");
    }
}
