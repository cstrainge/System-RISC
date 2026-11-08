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
        use mac_object_table::domain::DomainKind;
        match owner.kind()
        {
            DomainKind::Module =>
            {
                crate::drivers::dependencies_changed();
                crate::drivers::recovery_changed();
            }
            DomainKind::Process =>
            {
                crate::drivers::dependencies_changed();
                crate::toolbox::processes::retirement::resources_released();
            }
            DomainKind::System => ()
        }
    }

    fn resource_released(_: mac_object_table::domain::DomainId)
    {
        crate::recovery::acknowledgement_changed();
        crate::kernel::control_resource_acknowledged();
        crate::toolbox::windows::manager::ack::acknowledgement_changed();
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

    fn released(owner: &mac_object_table::shared::OwnerRef<Self>)
    {
        if owner.owner().kind() == mac_object_table::domain::DomainKind::Process
        {
            // Exact-key ACK and source/reader notifications have independent
            // paths. A live Process's account release cannot retire that owner.
            // accepting() is its original admission AtomicBool, with no lookup.
            if !owner.accepting()
            {
                crate::toolbox::processes::retirement::resources_released();
            }
        }
        else if owner.owner().kind() == mac_object_table::domain::DomainKind::Module
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

/// Masks IRQ permanently, with no restore. Only the terminal path uses this:
/// execution never resumes, so there is no scope to restore into.
pub(crate) fn disable()
{
    // SAFETY: clearing only this CPU's SIE bit; the caller never returns.
    unsafe {
        core::arch::asm!("csrci sstatus, 2", options(nostack));
    }
}

/// Nothing on this machine has to be programmed before a deadline can be
/// delivered: the timer is a supervisor CSR (`stimecmp`), its interrupt is a
/// pending bit in `sip`, and both are taken through `stvec` without a separate
/// controller in the path. Admission therefore has nothing to configure and
/// nothing it could refuse.
///
/// The type is stated rather than the function omitted so the shared bring-up
/// reads the same on every machine instead of testing which one it is on.
pub(crate) enum Refusal
{}
impl core::fmt::Display for Refusal
{
    fn fmt(&self, _: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        match *self
        {}
    }
}

/// Nothing to bring up: see [`Refusal`].
pub(crate) fn admit(_info: &mac_abi::BootInfo) -> Result<(), Refusal>
{
    Ok(())
}
