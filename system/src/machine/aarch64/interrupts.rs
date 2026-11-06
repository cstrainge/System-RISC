//! Interrupt masking and the resident invocation gate.
//!
//! AArch64 keeps the interrupt mask in PSTATE.DAIF: bit 7 masks IRQ, bit 6 FIQ,
//! bit 8 SError and bit 9 debug. Only the IRQ bit is touched here, and only the
//! caller's previous value is restored, so a nested mask cannot leave interrupts
//! enabled that an outer scope had disabled.
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

/// PSTATE.DAIF bit 7 is the IRQ mask.
const IRQ_MASK: u64 = 1 << 7;

/// Affine ownership of this CPU's prior IRQ state. It is deliberately not
/// Send/Sync; a scheduler or metadata guard must drop before restoration.
pub(crate) struct InterruptMask
{
    previous: u64,
    _affine:  core::marker::PhantomData<*mut ()>
}
impl Drop for InterruptMask
{
    fn drop(&mut self)
    {
        if self.previous & IRQ_MASK == 0
        {
            // SAFETY: restores only this CPU's originally unmasked IRQ bit.
            // No nomem: protected accesses cannot cross restoration.
            unsafe { asm!("msr daifclr, #2", options(nostack)) };
        }
    }
}

/// Masks IRQ and returns the previous state. FIQ and SError are left alone:
/// masking them would also hide faults the resident path is meant to report.
pub(crate) fn mask() -> InterruptMask
{
    let previous: u64;
    // SAFETY: System runs at EL1; reading DAIF and masking IRQ is atomic with
    // respect to local exception entry.
    unsafe {
        asm!("mrs {previous}, daif", previous = out(reg) previous, options(nostack));
        asm!("msr daifset, #2", options(nostack));
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
    // SAFETY: setting only this CPU's IRQ mask bit; the caller never returns.
    unsafe { asm!("msr daifset, #2", options(nostack)) };
}

/// Whether IRQ is currently unmasked on this CPU.
pub(crate) fn enabled() -> bool
{
    let state: u64;
    // SAFETY: reading DAIF has no side effects.
    unsafe { asm!("mrs {state}, daif", state = out(reg) state, options(nostack)) };
    state & IRQ_MASK == 0
}
