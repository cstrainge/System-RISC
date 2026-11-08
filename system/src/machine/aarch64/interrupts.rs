//! Interrupt delivery admission, interrupt masking and the resident invocation
//! gate.
//!
//! AArch64 keeps the interrupt mask in PSTATE.DAIF: bit 7 masks IRQ, bit 6 FIQ,
//! bit 8 SError and bit 9 debug. Only the IRQ bit is touched here, and only the
//! caller's previous value is restored, so a nested mask cannot leave interrupts
//! enabled that an outer scope had disabled.
//!
//! Delivery is a second, separate question, and it is this module's other half:
//! masking says whether this CPU may take an interrupt, and [`admit`] says
//! whether the machine has anywhere to send one from. Both have to be true
//! before a programmed deadline becomes a tick, and until this module existed
//! only the first was ever established.
use core::arch::asm;

use mac_abi::BootInfo;
use mac_devicetree::{Node, Tree};

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
/// Why the machine's interrupt controller could not be brought up.
///
/// Each cause is a different defect and a different fix, so they are kept apart
/// rather than flattened into one message: a machine that describes no
/// controller is not a kernel that cannot drive the one it was given, and a
/// controller outside the window the kernel root maps is neither.
pub(crate) enum Refusal
{
    /// The machine supplied no tree this kernel could read.
    Tree,
    /// No node in the tree declares itself an interrupt controller.
    NoController,
    /// The controller is one this resident path does not drive. The identity is
    /// carried so the refusal can name it rather than say "unsupported".
    Unsupported
    {
        compatible: [u8; 16]
    },
    /// The controller's register blocks are not inside the window the platform
    /// record declares, so the kernel root maps neither of them.
    OutsideWindow
    {
        distributor:   u64,
        cpu_interface: u64
    },
    /// The machine states no timer interrupt for the controller to deliver, so
    /// there is nothing to enable.
    NoTimerNode,
    /// The timer node's interrupt is written in a layout this reader cannot
    /// decode, so reading it would be a guess.
    NoTimerSpecifier,
    /// The timer's interrupt is not a private peripheral interrupt.
    TimerNotPrivate
    {
        kind:   u32,
        number: u32
    }
}
impl core::fmt::Display for Refusal
{
    fn fmt(&self, out: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        match self
        {
            Self::Tree =>
            {
                out.write_str("the machine supplied no readable device tree")
            }
            Self::NoController =>
            {
                out.write_str("the machine's device tree declares no interrupt controller")
            }
            Self::Unsupported { compatible } =>
            {
                let end = compatible.iter().position(|&byte| byte == 0).unwrap_or(compatible.len());
                write!(out,
                       "the machine's interrupt controller `{}` is not a GICv2 this resident \
                        path can program",
                       core::str::from_utf8(&compatible[..end]).unwrap_or("<unprintable>"))
            }
            Self::OutsideWindow { distributor,
                                  cpu_interface } =>
            {
                write!(out,
                       "the controller's register blocks ({distributor:#x}, {cpu_interface:#x}) \
                        are outside the window the platform record declares \
                        ({:#x}..{:#x}), which is the extent the kernel root maps",
                       crate::machine::memory::INTERRUPT_CONTROLLER_BASE,
                       crate::machine::memory::INTERRUPT_CONTROLLER_BASE
                       + crate::machine::memory::INTERRUPT_CONTROLLER_BYTES)
            }
            Self::NoTimerNode =>
            {
                out.write_str("the machine's device tree describes no `arm,armv8-timer`")
            }
            Self::NoTimerSpecifier =>
            {
                out.write_str("the machine's timer declares no interrupt this kernel can read \
                               against the controller's specifier width")
            }
            Self::TimerNotPrivate { kind,
                                     number } =>
            {
                write!(out,
                       "the machine's timer interrupt is not a private peripheral interrupt \
                        (type {kind}, number {number})")
            }
        }
    }
}

/// The compatible identities this module drives.
///
/// GICv2 is the only controller whose register map is implemented here, and the
/// two names are the same part: `arm,cortex-a15-gic` is how a machine describes
/// the integrated controller and `arm,gic-400` is the discrete equivalent.
const GICV2: &[&str] = &["arm,cortex-a15-gic", "arm,gic-400"];

/// Brings up the machine's interrupt controller, so a programmed deadline can
/// become a delivered interrupt.
///
/// This is machine bring-up and not a driver: it runs before any module can
/// load, and nothing binds it to a device. The controller, its two register
/// blocks and the timer's interrupt number all come from the machine's own tree.
/// The platform record contributes exactly one fact - the window the kernel root
/// maps - and that fact is checked against the tree rather than used as an
/// address, because a declaration nobody compares is how a wrong address
/// reaches a boot.
pub(crate) fn admit(info: &BootInfo) -> Result<(), Refusal>
{
    // SAFETY: the admitted firmware root maps primary RAM in the permanent
    // physmap and the blob stays pinned for the System lifetime, which is the
    // same contract the peripheral inventory is admitted under.
    let blob = unsafe {
        crate::machine::memory::primary_slice(info, info.device_tree, info.device_tree_bytes)
    }.map_err(|_| Refusal::Tree)?;
    let tree = Tree::new(blob).map_err(|_| Refusal::Tree)?;
    let controller = tree.find(|node| node.enabled() && node.interrupt_controller)
                          .map_err(|_| Refusal::Tree)?
                          .ok_or(Refusal::NoController)?;
    let mut compatible = [0u8; 16];
    let identity = controller.compatible.unwrap_or("<unnamed>");
    for (slot, byte) in compatible.iter_mut().zip(identity.as_bytes())
    {
        *slot = *byte;
    }
    if !GICV2.iter().any(|name| controller.names(name))
    {
        return Err(Refusal::Unsupported { compatible });
    }
    // A GICv2 states its distributor and its CPU interface as two `reg`
    // regions. Reading the second from the first is the guess this project has
    // already paid for once; the machine states both, so both are read.
    let (distributor, distributor_bytes) =
        region(&controller, 0).ok_or(Refusal::NoController)?;
    let (cpu_interface, cpu_interface_bytes) =
        region(&controller, 1).ok_or(Refusal::NoController)?;
    let base = crate::machine::memory::INTERRUPT_CONTROLLER_BASE;
    let bytes = crate::machine::memory::INTERRUPT_CONTROLLER_BYTES;
    let inside = |address: u64, length: u64| {
        address >= base && address.checked_add(length).is_some_and(|end| end <= base + bytes)
    };
    if !inside(distributor, distributor_bytes) || !inside(cpu_interface, cpu_interface_bytes)
    {
        return Err(Refusal::OutsideWindow { distributor,
                                            cpu_interface });
    }
    let timer = timer_of(&tree, &controller)?;
    super::gic::admit(distributor, cpu_interface, timer);
    Ok(())
}

/// The `reg` entry at `index`, as an address and a length.
///
/// The entry's width is the pair of cell counts the node inherits from its
/// parent, so a machine that states 32-bit addresses is read as one rather than
/// assumed to be 64-bit. A width this reader does not handle is `None`, which the
/// caller turns into a refusal: reading a `reg` by the wrong stride is exactly
/// how a device window ends up at an address the machine never stated.
fn region(node: &Node<'_>, index: usize) -> Option<(u64, u64)>
{
    let reg = node.reg?;
    let address_cells = node.address_cells? as usize;
    let size_cells = node.size_cells? as usize;
    if !(1..=2).contains(&address_cells) || !(1..=2).contains(&size_cells)
    {
        return None;
    }
    let cells = address_cells + size_cells;
    let entry = reg.get(index * cells * 4..(index + 1) * cells * 4)?;
    let cell = |offset: usize| -> u64 {
        u32::from_be_bytes(entry[offset..offset + 4].try_into().unwrap()) as u64
    };
    let mut address = 0;
    for index in 0..address_cells
    {
        address = (address << 32) | cell(index * 4);
    }
    let mut length = 0;
    for index in 0..size_cells
    {
        length = (length << 32) | cell((address_cells + index) * 4);
    }
    Some((address, length))
}

/// The interrupt the machine states for its non-secure physical timer.
///
/// The timer node's binding states four interrupts in a fixed order - secure
/// physical, non-secure physical, virtual, hypervisor - and `CNTP_*`, which is
/// the timer the scheduler arms, is the non-secure physical one. Each specifier
/// is as wide as the controller's `#interrupt-cells`, so the width is read from
/// the controller rather than assumed.
fn timer_of(tree: &Tree<'_>, controller: &Node<'_>) -> Result<u32, Refusal>
{
    let cells = controller.interrupt_cells.ok_or(Refusal::NoTimerSpecifier)? as usize;
    // A GICv2 specifier is type, number, flags. A narrower one is a different
    // controller's encoding and cannot be read with this layout.
    if cells != 3
    {
        return Err(Refusal::NoTimerSpecifier);
    }
    let node = tree.find(|node| node.enabled() && node.names("arm,armv8-timer"))
                   .map_err(|_| Refusal::Tree)?;
    let specifiers =
        node.ok_or(Refusal::NoTimerNode)?.interrupts.ok_or(Refusal::NoTimerSpecifier)?;
    // The binding states four specifiers in a fixed order - secure physical,
    // non-secure physical, virtual, hypervisor - and `CNTP_*`, which is the
    // timer the scheduler arms, is the non-secure physical one.
    let entry = specifiers.get(cells * 4..cells * 8).ok_or(Refusal::NoTimerSpecifier)?;
    let kind = u32::from_be_bytes(entry[0..4].try_into().unwrap());
    let number = u32::from_be_bytes(entry[4..8].try_into().unwrap());
    // Type 1 is a private peripheral interrupt, which is what a timer is on this
    // controller, and its number is an index inside that bank rather than the
    // number the distributor uses. A shared interrupt here would mean the
    // specifier order is not the binding's, and vectoring it would be a guess.
    if kind != 1 || number >= super::gic::PPI_BASE
    {
        return Err(Refusal::TimerNotPrivate { kind,
                                              number });
    }
    Ok(super::gic::PPI_BASE + number)
}
