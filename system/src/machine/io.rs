//! Synchronous bootstrap-I/O identity handoff. Resident work already executes
//! on the per-hart System stack; no second stack or borrowed guard is needed.
pub fn run<F: FnOnce() -> R, R>(function: F) -> R
{
    super::interrupts::with_masked(|| {
        if super::scheduler::current_context().identity().thread == crate::context::BOOT_THREAD
        {
            return function();
        }
        struct RestoreIo(bool);
        impl Drop for RestoreIo
        {
            fn drop(&mut self)
            {
                super::scheduler::io_leave(self.0);
            }
        }
        let resources = crate::kernel::current_resource_selection();
        let subject = crate::context::io_subject();
        let allocation = crate::context::selected_allocation();
        let _restore = RestoreIo(super::scheduler::io_enter());
        crate::context::with_bootstrap_allocation(allocation, || {
            crate::context::with_bootstrap_io_subject(subject, || {
                crate::context::with_bootstrap_resources(Some(resources), function)
            })
        })
    })
}
