//! The machine's own peripheral inventory.
//!
//! The platform record declares *which* peripherals a platform carries. The
//! machine's device tree is the authority for *where* they are: their register
//! windows, sizes, interrupts and bus-mastering behaviour. This module resolves
//! each declared identity through the tree the machine supplied, once, at
//! admission, and publishes the result.
//!
//! Two things follow, and both are deliberate. The kernel is not compiled with
//! any device address, so it cannot read a claim where it should read the
//! hardware. And a declared peripheral the machine's tree does not describe is a
//! refusal that names it - never a default, never a skip, and never a fallback
//! to a compiled-in window.

use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use mac_abi::BootInfo;
use mac_devicetree::{Node, Tree};

/// How far an `interrupt-parent` may be inherited. A real tree states it on the
/// bus or the root, so a longer chain is a malformed tree rather than a deeper
/// machine, and walking it would be unbounded work at admission.
const MAX_ANCESTRY: usize = 16;

/// A peripheral as the machine describes it.
#[derive(Clone, Copy)]
pub(crate) struct Device
{
    pub bus:            [u8; 4],
    pub key:            [u8; 16],
    pub registers:      u64,
    pub register_bytes: u64,
    /// The interrupt the machine states, or `None` when the node declares none.
    /// A driver that needs an interrupt refuses the device rather than vectoring
    /// a number the machine never stated.
    pub interrupt:      Option<u32>,
    pub flags:          u32,
    /// How the device addresses memory, from its bus's `dma-ranges`. A driver
    /// is handed device addresses through this, never a CPU address to adjust.
    pub dma:            mac_devicetree::DmaView
}

/// The compatible string a virtio-MMIO transport declares.
const VIRTIO_MMIO: &str = "virtio,mmio";

/// The virtio-MMIO register file: the transport's own registers occupy the
/// first `0x100` bytes of its window, and device-specific configuration follows.
/// A window shorter than that cannot carry the protocol the transport is
/// written in, so a node that declares one is not a transport.
const VIRTIO_MMIO_REGISTERS: u64 = 0x100;

/// A virtio-MMIO transport as the machine places it.
///
/// The window and the interrupt are the machine's facts, read from the node that
/// declares them. A transport is not identified by its position in this list:
/// the machine places windows where it places them, and the order is only the
/// tree's order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Transport
{
    pub registers:      u64,
    pub register_bytes: u64,
    /// The interrupt the machine states for this transport, or `None` when the
    /// node declares none.
    pub interrupt:      Option<u32>
}

/// Why a declared peripheral could not be resolved.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Cause
{
    /// The machine supplied no readable device tree, or the walk over it did not
    /// reach its end. Both mean the same thing to a caller: the tree could not
    /// be read, so a short list is a failure rather than a smaller machine.
    Tree,
    /// Ordinary memory could not retain the admitted peripheral inventory.
    Allocation,
    /// The machine's tree does not describe this peripheral.
    Missing,
    /// The node declares interrupts that cannot be attributed to a controller.
    NoInterruptParent,
    /// The node sits under a bus whose `dma-ranges` this kernel cannot read as
    /// one view of memory.
    DmaView,
    /// A window the machine describes as a transport is also enumerated as a
    /// board peripheral, so one device would be offered to a driver twice.
    Overlap,
    /// A peripheral admission should have enumerated was not there. Admission
    /// walks the machine's tree, so this is an internal inconsistency rather
    /// than a machine fact, and it is still reported rather than skipped.
    Unpublished
}

/// A refusal that names the peripheral it is about.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Refusal
{
    key:   [u8; 16],
    cause: Cause
}

/// The key of a refusal about the blob itself rather than about one peripheral.
const NO_KEY: [u8; 16] = [0; 16];

impl Refusal
{
    /// The declared identity, as the compatible string it was written as.
    pub(crate) fn name(&self) -> &str
    {
        let end = self.key.iter().position(|&byte| byte == 0).unwrap_or(self.key.len());
        core::str::from_utf8(&self.key[..end]).unwrap_or("<unprintable>")
    }

    pub(crate) fn cause(&self) -> Cause
    {
        self.cause
    }

    /// The refusal for a peripheral the enumeration was expected to find and
    /// did not. It names no identity, because the walk that would have supplied
    /// one is the thing that came up short.
    pub(crate) fn unresolved() -> Self
    {
        Refusal { key:   NO_KEY,
                  cause: Cause::Unpublished }
    }
}

/// The refusal for a tree this kernel could not read to its end. It names no
/// identity: the defect is the blob, not a peripheral in it.
fn unreadable_tree() -> Refusal
{
    Refusal { key:   NO_KEY,
              cause: Cause::Tree }
}

impl core::fmt::Display for Refusal
{
    fn fmt(&self, out: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        match self.cause
        {
            Cause::Tree => out.write_str("the machine supplied no readable device tree"),
            Cause::Allocation => out.write_str("no memory for the admitted peripheral inventory"),
            Cause::Missing =>
            {
                write!(out, "the machine's device tree does not describe `{}`", self.name())
            }
            Cause::NoInterruptParent =>
            {
                write!(out,
                       "`{}` declares interrupts but no interrupt controller can be resolved for \
                        it",
                       self.name())
            }
            Cause::DmaView =>
            {
                write!(out,
                       "`{}` sits under a bus whose dma-ranges cannot be read as one view of \
                        memory",
                       self.name())
            }
            Cause::Overlap =>
            {
                write!(out,
                       "`{}` is enumerated as both a virtio-MMIO transport and a board peripheral",
                       self.name())
            }
            Cause::Unpublished =>
            {
                write!(out, "a peripheral admission should have enumerated was not there")
            }
        }
    }
}

/// The machine's authoritative description, validated and pinned at admission.
/// Early boot walks it without allocation. Once heap storage exists, retain the
/// classified descriptors so allocator and device queries do not walk it again.
/// The inventory is derived from this tree, never a compiled peripheral list.
static TREE: spin::Once<Tree<'static>> = spin::Once::new();

struct Inventory
{
    devices:    Vec<Device>,
    transports: Vec<Transport>
}
static INVENTORY: spin::Once<Inventory> = spin::Once::new();

/// Retain the already validated, disjoint classification after heap startup.
/// Storage grows under ordinary memory admission, with no fixed device limit.
/// Allocation failure publishes nothing and refuses service startup.
pub(crate) fn retain_inventory() -> Result<(), Refusal>
{
    if INVENTORY.get().is_some()
    {
        return Ok(());
    }
    let mut inventory = Inventory { devices:    Vec::new(),
                                    transports: Vec::new() };
    let mut allocation_failed = false;
    walk(|classified| {
        if allocation_failed
        {
            return;
        }
        match classified
        {
            Classified::Peripheral(device) =>
            {
                if inventory.devices.try_reserve(1).is_err()
                {
                    allocation_failed = true;
                }
                else
                {
                    inventory.devices.push(device);
                }
            }
            Classified::Transport(transport) =>
            {
                if inventory.transports.try_reserve(1).is_err()
                {
                    allocation_failed = true;
                }
                else
                {
                    inventory.transports.push(transport);
                }
            }
        }
    });
    if allocation_failed
    {
        return Err(Refusal { key:   NO_KEY,
                             cause: Cause::Allocation });
    }
    INVENTORY.call_once(|| inventory);
    Ok(())
}

/// The firmware power conduit the bootstrap read from the machine's own tree,
/// as [`mac_abi::boot::conduit`] encodes it.
///
/// This is a boot fact rather than an enumeration. A `/psci` node describes a
/// function and states no register window, so it is not a device a driver can
/// match or claim; the resident relays what the machine declared and the power
/// driver decides whether it implements that conduit.
static POWER_CONDUIT: AtomicU64 = AtomicU64::new(mac_abi::boot::conduit::NONE);

/// The declared conduit. An encoding this build does not know is reported as
/// [`mac_abi::boot::conduit::OTHER`] so a driver refuses it by name instead of
/// treating it as the absence of a conduit.
pub(crate) fn power_conduit() -> u64
{
    let declared = POWER_CONDUIT.load(Ordering::Acquire);
    if declared > mac_abi::boot::conduit::OTHER
    {
        return mac_abi::boot::conduit::OTHER;
    }
    declared
}

/// Check that the machine supplied a tree this kernel can read, and pin it.
///
/// Runs before anything that needs a device window, so a tree that does not
/// parse is refused before a window is used rather than after.
///
/// # Safety
/// The admitted firmware root maps primary RAM in the permanent physmap and the
/// blob stays pinned for the System lifetime, which is what lets the borrowed
/// view live as long as the tree is read.
pub(crate) unsafe fn admit(info: &BootInfo) -> Result<(), Refusal>
{
    if TREE.get().is_some()
    {
        return Ok(());
    }
    let blob = unsafe {
                   super::memory::primary_slice(info, info.device_tree, info.device_tree_bytes)
               }.map_err(|_| unreadable_tree())?;
    let tree = Tree::new(blob).map_err(|_| unreadable_tree())?;
    verify(&tree)?;
    POWER_CONDUIT.store(info.power_conduit, Ordering::Release);
    TREE.call_once(|| tree);
    Ok(())
}

/// Read the machine's tree to its end, once, and refuse what a later walk would
/// otherwise discover one node at a time.
///
/// Three facts are established here, and every enumeration below relies on them:
/// the structure block is readable to its end, so a walk cannot come up short; a
/// node that states interrupts states ones this kernel can attribute, so no
/// device is published with an interrupt the machine never stated; and no node
/// is enumerated as both a transport and a board peripheral, so a window is
/// offered to a driver once.
///
/// This is the one place a walk over the tree can fail, and it is the place that
/// can still say so by name: a boot that reaches enumeration has a tree whose
/// every node was already read.
fn verify(tree: &Tree<'_>) -> Result<(), Refusal>
{
    let mut refusal = None;
    tree.for_each_node(|node| {
            if refusal.is_some()
            {
                return;
            }
            refusal = match classify(tree, &node)
            {
                // A transport is a bus, and the device on it is identified by
                // the transport's ID register, so the node that declares the
                // window is a transport and never a board peripheral. A node
                // that was both would be offered to a driver twice, under two
                // keys and with two different flag sets.
                Ok(Some(Classified::Peripheral(device))) if node.names(VIRTIO_MMIO) =>
                {
                    Some(Refusal { key:   device.key,
                                   cause: Cause::Overlap })
                }
                Ok(_) => None,
                Err(cause) => Some(cause)
            };
        })
        .map_err(|_| unreadable_tree())?;
    match refusal
    {
        Some(refusal) => Err(refusal),
        None => Ok(())
    }
}

/// What one node of the machine's tree describes.
///
/// A node yields at most one of these, which is what keeps the two views this
/// module publishes disjoint: the classification is one predicate, not a filter
/// in each view that could drift from the other.
enum Classified
{
    Transport(Transport),
    Peripheral(Device)
}

/// The single classification both walks are filtered from.
fn classify(tree: &Tree<'_>, node: &Node<'_>) -> Result<Option<Classified>, Refusal>
{
    if !node.enabled() || node.compatible.is_none() || node.reg.is_none()
    {
        return Ok(None);
    }
    if node.names(VIRTIO_MMIO)
    {
        return transport_of(tree, node).map(|transport| transport.map(Classified::Transport));
    }
    device_of(tree, node).map(|device| device.map(Classified::Peripheral))
}

/// The admitted tree.
///
/// Admission pins it before anything enumerates it, so a caller that finds it
/// missing ran before admission rather than found a machine with no peripherals.
/// The message names that boot-order invariant rather than answering an empty
/// list, which would be indistinguishable from a machine that describes nothing.
fn tree() -> &'static Tree<'static>
{
    TREE.get().expect("admission pinned the machine's tree before enumeration")
}

/// Visit every classified node of the admitted tree.
///
/// The walk cannot fail here: admission read this blob to its end and resolved
/// every node in it, which is what the two messages name. Answering with a
/// shorter list instead would be the defect they exist to prevent.
fn walk(mut take: impl FnMut(Classified))
{
    let tree = tree();
    tree.for_each_node(|node| {
            if let Some(classified) =
                classify(tree, &node).expect("admission resolved every node in the tree")
            {
                take(classified);
            }
        })
        .expect("admission read the machine's tree to its end");
}

/// Visit every peripheral the machine describes, in tree order, and report how
/// many there were.
///
/// A peripheral is an enabled node that names a part and states a window, and is
/// not a transport. Which of them a driver binds is the driver's declaration,
/// matched by bus and key, not a platform list, so a machine with parts this
/// kernel has never seen needs no change here.
fn for_each_device(mut visit: impl FnMut(Device)) -> usize
{
    if let Some(inventory) = INVENTORY.get()
    {
        inventory.devices.iter().copied().for_each(visit);
        return inventory.devices.len();
    }
    let mut count = 0;
    walk(|classified| {
        if let Classified::Peripheral(device) = classified
        {
            visit(device);
            count += 1;
        }
    });
    count
}

/// Visit every virtio-MMIO transport the machine describes, in tree order, and
/// report how many there were.
///
/// This is a filtered view of the same walk `for_each_device` uses, not a second
/// reading of the tree: a transport's window and interrupt are resolved exactly
/// as any other peripheral's are, and the filter is the classification, so a
/// window is in one view or the other and never in both.
pub(crate) fn for_each_transport(mut visit: impl FnMut(Transport)) -> usize
{
    if let Some(inventory) = INVENTORY.get()
    {
        inventory.transports.iter().copied().for_each(visit);
        return inventory.transports.len();
    }
    let mut count = 0;
    walk(|classified| {
        if let Classified::Transport(transport) = classified
        {
            visit(transport);
            count += 1;
        }
    });
    count
}

/// How many peripherals the machine described.
pub(crate) fn count() -> usize
{
    if let Some(inventory) = INVENTORY.get()
    {
        return inventory.devices.len();
    }
    for_each_device(|_| ())
}

/// The transport a node describes, or `None` when it describes none.
///
/// Only the node's own `reg` states where a transport is. Nothing here derives a
/// window from an index: a machine that places its transports unevenly, or
/// leaves the positions between them empty, is described exactly as it is.
fn transport_of(tree: &Tree<'_>, node: &Node<'_>) -> Result<Option<Transport>, Refusal>
{
    // Where the CPU finds the block, not the number the node wrote: under a
    // remapping bus those differ, and a node on a bus with no mapping at all -
    // a USB port, an I2C device - has no address to publish.
    let Some((registers, register_bytes)) =
        tree.cpu_window(node).map_err(|_| unreadable_tree())?
    else
    {
        return Ok(None);
    };
    if registers == 0
       || register_bytes < VIRTIO_MMIO_REGISTERS
       || registers.checked_add(register_bytes).is_none()
    {
        return Ok(None);
    }
    let key = node.compatible.map_or(NO_KEY, key_of);
    let interrupt = interrupt_of(tree, node).map_err(|()| {
                                                Refusal { key,
                                                          cause: Cause::NoInterruptParent }
                                            })?;
    Ok(Some(Transport { registers,
                        register_bytes,
                        interrupt }))
}

/// How many virtio-MMIO transports the machine describes.
pub(crate) fn transport_count() -> usize
{
    if let Some(inventory) = INVENTORY.get()
    {
        return inventory.transports.len();
    }
    for_each_transport(|_| ())
}

/// The transport at `index` in tree order, or `None` when the machine describes
/// fewer transports than that.
pub(crate) fn transport(index: usize) -> Option<Transport>
{
    if let Some(inventory) = INVENTORY.get()
    {
        return inventory.transports.get(index).copied();
    }
    let mut found = None;
    let mut seen = 0usize;
    for_each_transport(|transport| {
        if seen == index && found.is_none()
        {
            found = Some(transport);
        }
        seen += 1;
    });
    found
}

/// The peripheral a node describes, or `None` when it describes none.
fn device_of(tree: &Tree<'_>, node: &Node<'_>) -> Result<Option<Device>, Refusal>
{
    // Where the CPU finds the block, not the number the node wrote: under a
    // remapping bus those differ, and a node on a bus with no mapping at all -
    // a USB port, an I2C device - has no address to publish.
    let Some((registers, register_bytes)) =
        tree.cpu_window(node).map_err(|_| unreadable_tree())?
    else
    {
        return Ok(None);
    };
    if register_bytes == 0 || registers == 0
    {
        return Ok(None);
    }
    let Some(compatible) = node.compatible
    else
    {
        return Ok(None);
    };
    let key = key_of(compatible);
    // A node that states interrupts this kernel cannot attribute is refused by
    // the identity the machine wrote, rather than published as a device whose
    // interrupt is unknown: a driver that vectors it would vector a number the
    // machine never stated.
    let interrupt = interrupt_of(tree, node).map_err(|()| {
                                                Refusal { key,
                                                          cause: Cause::NoInterruptParent }
                                            })?;
    let mut flags = mac_device_interface::MMIO;
    if node.dma_coherent
    {
        flags |= mac_device_interface::COHERENT_DMA;
    }
    // A bus whose view of memory this kernel cannot read is not guessed at:
    // the device is refused by the identity the machine wrote, because a
    // driver given the wrong device address has the device write somewhere
    // else.
    let dma = tree.dma_view(node).map_err(|_| {
                                     Refusal { key,
                                               cause: Cause::DmaView }
                                 })?;
    Ok(Some(Device { bus: *b"MMIO",
                     key,
                     registers,
                     register_bytes,
                     interrupt,
                     flags,
                     dma }))
}

/// The declared identity, as the fixed-width key the wire carries it in.
fn key_of(compatible: &str) -> [u8; 16]
{
    let mut key = [0u8; 16];
    for (slot, byte) in key.iter_mut().zip(compatible.as_bytes())
    {
        *slot = *byte;
    }
    key
}

/// The enumerated peripheral at `index`, or `None` past the number the machine
/// described.
pub(crate) fn get(index: usize) -> Option<Device>
{
    if let Some(inventory) = INVENTORY.get()
    {
        return inventory.devices.get(index).copied();
    }
    let mut found = None;
    let mut seen = 0usize;
    for_each_device(|device| {
        if seen == index && found.is_none()
        {
            found = Some(device);
        }
        seen += 1;
    });
    found
}

/// The interrupt number a node's first specifier carries, or `None` when the
/// node declares none.
///
/// The width of a specifier is the interrupt controller's `#interrupt-cells`, so
/// the controller has to be resolved rather than assumed: reading the property
/// by length alone would take a two-cell specifier as a number plus a flag. A
/// node that declares no `interrupts` raises none, which is a fact rather than a
/// default, and a node that declares one this kernel cannot attribute is an
/// error rather than a number the machine never stated.
fn interrupt_of<'a>(tree: &Tree<'a>, node: &Node<'a>) -> Result<Option<u32>, ()>
{
    let Some(specifier) = node.interrupts
    else
    {
        return Ok(None);
    };
    let mut current = *node;
    let mut phandle = None;
    for _ in 0..MAX_ANCESTRY
    {
        if let Some(found) = current.interrupt_parent
        {
            phandle = Some(found);
            break;
        }
        let Some(index) = current.parent_index
        else
        {
            break;
        };
        current = tree.find(|candidate| candidate.index == index).map_err(|_| ())?.ok_or(())?;
    }
    let phandle = phandle.ok_or(())?;
    let controller = tree.find(|candidate| {
                             candidate.interrupt_controller
                             && (candidate.phandle == Some(phandle)
                                 || candidate.linux_phandle == Some(phandle))
                         })
                         .map_err(|_| ())?
                         .ok_or(())?;
    let cells = controller.interrupt_cells.ok_or(())? as usize;
    // A node may state several interrupts, each one specifier wide; a pin
    // controller with four banks states four. The first is the one reported, as
    // the summary above says, so the property must hold a whole number of
    // specifiers rather than exactly one.
    if cells == 0 || specifier.is_empty() || specifier.len() % (cells * 4) != 0
    {
        return Err(());
    }
    // The last cell is the trigger type and flags, and a one-cell controller
    // carries only the number itself.
    let start = cells.saturating_sub(2) * 4;
    Ok(Some(u32::from_be_bytes(specifier[start..start + 4].try_into().map_err(|_| ())?)))
}
