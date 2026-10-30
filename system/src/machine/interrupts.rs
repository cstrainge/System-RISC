//! Local interrupt/preemption gate for synchronous resident metadata operations.
//! CSR encoding belongs here, never in the shared handle/page models.
use core::arch::asm;

pub(crate) struct NativeInvocationGate;
impl mac_object_table::shared::InvocationGate for NativeInvocationGate
{
    fn run<R>(operation: impl FnOnce() -> R) -> R
    {
        with_masked(operation)
    }

    fn with_system_allocation<R>(operation: impl FnOnce() -> R) -> R
    {
        crate::allocator::with_system(crate::allocator::SystemClass::Ordinary, operation)
    }

    fn with_foundation_allocation<R>(operation: impl FnOnce() -> R) -> R
    {
        crate::allocator::with_system(crate::allocator::SystemClass::Foundation, operation)
    }

    fn scope_closed(owner: mac_object_table::domain::DomainId)
    {
        if owner.kind() == mac_object_table::domain::DomainKind::Module
        {
            crate::drivers::dependencies_changed();
            crate::drivers::recovery_changed();
        }
    }

    fn resource_released(_: mac_object_table::domain::DomainId)
    {
        crate::recovery::acknowledgement_changed();
    }

    fn publication_released(owner: mac_object_table::domain::DomainId)
    {
        use mac_object_table::domain::DomainKind;
        match owner.kind()
        {
            DomainKind::Process => crate::toolbox::processes::retirement::resources_released(),
            DomainKind::Module => crate::drivers::recovery_changed(),
            DomainKind::System => crate::recovery::acknowledgement_changed()
        }
    }

    fn released(owner: mac_object_table::domain::DomainId)
    {
        if owner.kind() == mac_object_table::domain::DomainKind::Process
        {
            crate::toolbox::processes::retirement::resources_released();
        }
        else if owner.kind() == mac_object_table::domain::DomainKind::Module
        {
            crate::drivers::recovery_changed();
        }
    }
}

/// Keeps a bounded synchronous operation on this hart. Nested entry preserves
/// an already-masked state. The guard and its restoration cannot escape.
pub(crate) fn with_masked<R>(operation: impl FnOnce() -> R) -> R
{
    const SIE: usize = 1 << 1;
    struct Restore(usize);
    impl Drop for Restore
    {
        fn drop(&mut self)
        {
            if self.0 & SIE != 0
            {
                // SAFETY: restores only this hart's previously enabled SIE bit.
                // No nomem: the compiler must not move protected memory access
                // beyond interrupt restoration.
                unsafe {
                    asm!("csrs sstatus, {mask}", mask = in(reg) SIE, options(nostack));
                }
            }
        }
    }
    let previous;
    // SAFETY: System executes in S mode. CSR read/clear is atomic w.r.t. local
    // interrupt entry. The memory clobber prevents hoisting lock access before it.
    unsafe {
        asm!("csrrc {previous}, sstatus, {mask}", previous = out(reg) previous,
                  mask = in(reg) SIE, options(nostack));
    }
    let _restore = Restore(previous);
    operation()
}
