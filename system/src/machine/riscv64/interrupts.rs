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

/// The timer needs nothing programmed before a deadline can be delivered: it
/// is a supervisor CSR (`stimecmp`), its interrupt is a pending bit in `sip`,
/// and both are taken through `stvec` without a controller in the path. So
/// admission cannot refuse, and the type says so.
///
/// Devices are a different matter: their lines reach the hart through the
/// platform-level interrupt controller, which [`admit`] brings up when the
/// machine's tree describes one. A machine without one still boots; its
/// drivers are told no line can be taken and carry on being asked.
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

const PLIC: &[&str] = &["sifive,plic-1.0.0", "riscv,plic0"];
/// One priority word per source; zero means never interrupt.
const PLIC_PRIORITY: u64 = 0x0;
/// One bit per source, a block of words per context.
const PLIC_ENABLE: u64 = 0x2000;
const PLIC_ENABLE_STRIDE: u64 = 0x80;
/// Per context: the priority threshold, then the claim/complete word.
const PLIC_CONTEXT: u64 = 0x20_0000;
const PLIC_CONTEXT_STRIDE: u64 = 0x1000;
const PLIC_CLAIM: u64 = 4;
/// The controller's architectural limit; source 0 does not exist.
const PLIC_SOURCES: u32 = 1024;
/// `sie` bit for supervisor external interrupts.
const SEIE: usize = 1 << 9;

struct Plic
{
    base:    u64,
    /// The supervisor context of the hart that takes device interrupts.
    context: u64
}
static ADMITTED: spin::Once<Plic> = spin::Once::new();

fn read(address: u64) -> u32
{
    // SAFETY: the address is inside the controller's window, which the
    // machine's tree states and the kernel root maps as device memory.
    unsafe { core::ptr::read_volatile(address as *const u32) }
}

fn write(address: u64, value: u32)
{
    // SAFETY: as `read`; every register here is word-wide.
    unsafe { core::ptr::write_volatile(address as *mut u32, value) };
}

/// Brings up the platform-level interrupt controller for this hart, with every
/// source disabled.
///
/// A PLIC has one context per hart and privilege mode, in the order its
/// `interrupts-extended` lists them. On the machines this kernel runs on that
/// order is machine then supervisor for each hart in turn, so this hart's
/// supervisor context is `2 * hart + 1`. That is read off the layout, not out
/// of the property, and a machine that orders its contexts differently needs
/// the property read here.
pub(crate) fn admit(info: &mac_abi::BootInfo) -> Result<(), Refusal>
{
    // SAFETY: the boot stage states the tree's extent in the handoff.
    let Ok(blob) = (unsafe {
        crate::machine::memory::primary_slice(info, info.device_tree, info.device_tree_bytes)
    })
    else
    {
        return Ok(());
    };
    let Ok(tree) = mac_devicetree::Tree::new(blob)
    else
    {
        return Ok(());
    };
    let Ok(Some(node)) = tree.find(|node| node.enabled() && PLIC.iter().any(|name| node.names(name)))
    else
    {
        crate::machine::emergency::serial(format_args!("INTERRUPTS_READY: controller=none; the timer \
                                                        needs none, and no device line can be \
                                                        taken\n"));
        return Ok(());
    };
    let Ok(Some((base, bytes))) = tree.cpu_window(&node)
    else
    {
        return Ok(());
    };
    let context = 2 * (super::hart_id() as u64) + 1;
    let control = base + PLIC_CONTEXT + PLIC_CONTEXT_STRIDE * context;
    if control + 8 > base + bytes
    {
        return Ok(());
    }
    for word in 0..u64::from(PLIC_SOURCES / 32)
    {
        write(base + PLIC_ENABLE + PLIC_ENABLE_STRIDE * context + 4 * word, 0);
    }
    write(control, 0);
    ADMITTED.call_once(|| Plic { base,
                                 context });
    // SAFETY: setting only this hart's supervisor external enable.
    unsafe { asm!("csrs sie, {mask}", mask = in(reg) SEIE, options(nostack)) };
    crate::machine::emergency::serial(format_args!("INTERRUPTS_READY: controller=plic \
                                                    base={base:#x} context={context} \
                                                    threshold={:#x} sources_enabled=0\n",
                                                   read(control)));
    Ok(())
}

fn enable_word(plic: &Plic, line: u32) -> u64
{
    plic.base + PLIC_ENABLE + PLIC_ENABLE_STRIDE * plic.context + 4 * u64::from(line / 32)
}

fn device_line(line: u32) -> Option<&'static Plic>
{
    ADMITTED.get().filter(|_| line != 0 && line < PLIC_SOURCES)
}

/// The external interrupt's source: claimed, disabled for this context, and
/// completed. Completing it at once is safe because it is disabled first: a
/// level the device still holds becomes pending again and waits there until
/// its driver has run and the line is enabled again.
pub(crate) fn service() -> crate::machine::traps::Interrupt
{
    use crate::machine::traps::Interrupt;
    let Some(plic) = ADMITTED.get()
    else
    {
        return Interrupt::Unhandled { number: 0 };
    };
    let claim = plic.base + PLIC_CONTEXT + PLIC_CONTEXT_STRIDE * plic.context + PLIC_CLAIM;
    let line = read(claim);
    if line == 0 || line >= PLIC_SOURCES
    {
        return Interrupt::Unhandled { number: line };
    }
    let word = enable_word(plic, line);
    write(word, read(word) & !(1 << (line % 32)));
    write(claim, line);
    Interrupt::Device { line }
}

/// Gives a device's line a priority above the threshold and enables it.
pub(crate) fn enable_line(line: u32) -> bool
{
    let Some(plic) = device_line(line)
    else
    {
        return false;
    };
    write(plic.base + PLIC_PRIORITY + 4 * u64::from(line), 1);
    unmask_line(line);
    true
}

pub(crate) fn mask_line(line: u32)
{
    if let Some(plic) = device_line(line)
    {
        // The enable word is shared with the trap path, which clears bits in
        // it; the update is made with this hart's interrupts off.
        with_masked(|| {
            let word = enable_word(plic, line);
            write(word, read(word) & !(1 << (line % 32)));
        });
    }
}

pub(crate) fn unmask_line(line: u32)
{
    if let Some(plic) = device_line(line)
    {
        with_masked(|| {
            let word = enable_word(plic, line);
            write(word, read(word) | 1 << (line % 32));
        });
    }
}
