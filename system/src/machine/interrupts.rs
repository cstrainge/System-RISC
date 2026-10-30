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

/// Affine ownership of this hart's prior SIE state. It is deliberately not
/// Send/Sync; a scheduler/metadata guard must drop before interrupt restoration.
pub(crate) struct InterruptMask
{
    previous: usize,
    _affine:  core::marker::PhantomData<*mut ()>
}
impl Drop for InterruptMask
{
    fn drop(&mut self)
    {
        const SIE: usize = 1 << 1;
        if self.previous & SIE != 0
        {
            // SAFETY: restore only this hart's originally enabled SIE bit.
            // No nomem: protected accesses cannot cross restoration.
            unsafe {
                asm!("csrs sstatus, {mask}",mask=in(reg)SIE,options(nostack));
            }
        }
    }
}
pub(crate) fn mask() -> InterruptMask
{
    const SIE: usize = 1 << 1;
    let previous;
    // SAFETY: System executes in S mode; read/clear is atomic wrt local entry.
    unsafe {
        asm!("csrrc {previous}, sstatus, {mask}",previous=out(reg)previous,mask=in(reg)SIE,options(nostack));
    }
    InterruptMask { previous,
                    _affine: core::marker::PhantomData }
}
/// The same RAII gate is used for bounded closures and guarded transactions.
pub(crate) fn with_masked<R>(operation: impl FnOnce() -> R) -> R
{
    let _restore = mask();
    operation()
}
