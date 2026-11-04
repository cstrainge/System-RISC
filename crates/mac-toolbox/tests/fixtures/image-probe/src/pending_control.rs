//! Whole-Process stop while an existing ControlWork author is Running.
//! No App/DDEF shared flag or guessed timer establishes that state: the private
//! collector must observe the real broker/THRD/claim in read-only GDB first.
use mac_toolbox::{Error, ProcessId, System,
                  abi::native_threads,
                  controls::{AxisConstraint, Constraints, Extent, Invalidation, Reflow,
                             StaticTextSpec, TextAlignment, TextLayout},
                  events::Modifiers};

const TITLE: &str = "pending-probe/ask-size";
const DEADLINE_NS: u64 = 600_000_000_000;
const POLL_NS: u32 = 50_000_000;

fn stop_on_physical_shift(system: &System<'_>) -> Result<(), Error>
{
    let owner = system.processes().current()?.id;
    let thread = system.client().current_native_thread().map_err(|cause| cause.cause())?;
    super::isolation::log_value(system, owner, "PENDING_STOP_CHILD_THREAD", thread.0)?;
    if system.events().input_state()?.modifiers.contains(Modifiers::SHIFT)
    {
        super::isolation::log_failure(system, "pending-child-initial-shift", Error::BadArgument);
        return Err(Error::BadArgument);
    }
    let deadline =
        system.events().monotonic_nanoseconds()?.checked_add(DEADLINE_NS).ok_or(Error::BadAbi)?;
    // This means only that the real child is waiting for physical input. It is
    // not evidence that the main request or callback has reached Running.
    system.log("PENDING_STOP_CHILD_ARMED_INPUT_STATE\n")?;
    loop
    {
        if system.events().input_state()?.modifiers.contains(Modifiers::SHIFT)
        {
            system.log("PENDING_STOP_PHYSICAL_SHIFT\n")?;
            panic!("PENDING_CONTROL_OWNER_STOP: actual child panic after physical Shift; whole \
                    Process stop, not a surviving-envelope cancellation");
        }
        if system.events().monotonic_nanoseconds()? >= deadline
        {
            super::isolation::log_failure(system, "pending-child-deadline", Error::Cancelled);
            return Err(Error::Cancelled);
        }
        let mut call = native_threads::Call { operation: native_threads::SLEEP,
                                              nanoseconds: POLL_NS,
                                              ..Default::default() };
        // SAFETY: this scalar-only public native SLEEP transfers no pointer or
        // ownership. The actual child is in its normal installed App runtime.
        unsafe { system.client().native_thread(&mut call) }.map_err(|cause| cause.cause())?;
    }
}

/// The native CREATE contract enters this genuine RX function in a child of
/// the same Process. Every path terminates in the installed panic/abort path;
/// no unwind/ordinary return crosses the C ABI without required TLS teardown.
unsafe extern "C" fn child_entry(_data: *mut u8)
{
    let system = match System::new()
    {
        Ok(system) => system,
        Err(error) => panic!("PENDING_STOP_CHILD_STARTUP_FAILED: {error}")
    };
    let result = stop_on_physical_shift(&system);
    if let Err(error) = result
    {
        super::isolation::log_failure(&system, "pending-child-trigger-failed", error);
        panic!("PENDING_STOP_CHILD_TRIGGER_FAILED: {error}; no intended-stop coverage");
    }
    panic!("PENDING_STOP_CHILD_UNEXPECTED_RETURN: no intended-stop coverage");
}

pub(super) fn run(system: &System<'_>) -> Result<(), Error>
{
    let mut stage = "pending-current-process";
    let result = (|| {
        let owner: ProcessId = system.processes().current()?.id;
        stage = "pending-main-thread";
        let thread = system.client().current_native_thread().map_err(|cause| cause.cause())?;
        super::isolation::log_value(system, owner, "PENDING_STOP_MAIN_THREAD", thread.0)?;
        stage = "pending-control-create";
        // WindowId(0) is the public supported windowless-Control sentinel,
        // not a forged Window target. No compositor or automatic Event poll.
        let control =
            system.controls()
                  .create_static_text_with(mac_toolbox::WindowId(0),
                                           StaticTextSpec { bounds:    mac_toolbox::Rect::new(0,
                                                                                              0,
                                                                                              120,
                                                                                              20),
                                                            text:      TITLE,
                                                            font:      Default::default(),
                                                            alignment: TextAlignment::Left,
                                                            layout:    TextLayout::Ellipsis,
                                                            reflow:    Reflow::Yes })?;
        super::isolation::log_value(system, owner, "PENDING_CONTROL_CREATED", control.id().0)?;
        stage = "pending-child-create";
        // SAFETY: genuine Process RX entry, null opaque data (nothing to free on
        // uncertain transfer), default real charged stack. The affine join right
        // stays on this creating thread; Drop is never used as cancellation.
        let child = unsafe {
                        system.client().create_native_thread(child_entry, core::ptr::null_mut(), 0)
                    }.map_err(|cause| cause.cause().cause())?;
        super::isolation::log_value(system, owner, "PENDING_STOP_CHILD_CREATED", child.thread().0)?;
        stage = "pending-control-invalidate";
        control.control().invalidate_layout(Invalidation::Measurement)?;
        stage = "pending-control-ask-size";
        let constraints =
            Constraints { width:  AxisConstraint::Exact(Extent::new(173).map_err(|_| {
                                                                            Error::BadArgument
                                                                        })?),
                          height: AxisConstraint::AtMost(Extent::new(79).map_err(|_| {
                                                                            Error::BadArgument
                                                                        })?) };
        // A pre-request marker only. Read-only debugger proof of actual broker
        // Running + original CTRL claim must precede the physical Shift input.
        system.log("PENDING_CONTROL_ASK_SIZE_BEGIN\n")?;
        let measured = control.control().ask_size(constraints);
        match measured
        {
            Ok(_) => system.log("PENDING_CONTROL_ASK_SIZE_UNEXPECTED_RETURN result=OK\n")?,
            Err(error) =>
            {
                super::isolation::log_failure(system, "pending-ask-size-unexpected-return", error);
                return Err(error);
            }
        }
        // Keep both real owners alive throughout the blocked request. A returned
        // measurement is an explicit failure, never a cleanup/pass witness.
        drop(child);
        Err(Error::BadAbi)
    })();
    if let Err(error) = result
    {
        super::isolation::log_failure(system, stage, error);
    }
    result
}
