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
    pub interrupt:      u32,
    pub flags:          u32
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
    pub interrupt:      u32
}

/// Why a declared peripheral could not be resolved.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Cause
{
    /// The machine supplied no readable device tree.
    Tree,
    /// Ordinary memory could not retain the admitted peripheral inventory.
    Allocation,
    /// The machine's tree does not describe this peripheral.
    Missing,
    /// The node is present but does not describe a usable register window.
    NoWindow,
    /// The node declares interrupts that cannot be attributed to a controller.
    NoInterruptParent,
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
            Cause::NoWindow =>
            {
                write!(out, "`{}` describes no usable register window", self.name())
            }
            Cause::NoInterruptParent =>
            {
                write!(out,
                       "`{}` declares interrupts but no interrupt controller can be resolved for \
                        it",
                       self.name())
            }
            Cause::Unpublished =>
            {
                write!(out, "a peripheral admission should have enumerated was not there")
            }
        }
    }
}

/// The pinned firmware description is used while boot is still allocation-free.
/// Once ordinary heap storage exists, `retain_inventory` publishes immutable
/// resolved descriptors. Paging and device queries then read that inventory
/// without reparsing the tree or resolving interrupt ancestry on every request.
static TREE_BLOB: spin::Once<&'static [u8]> = spin::Once::new();

struct Inventory
{
    devices:    Vec<Device>,
    transports: Vec<Transport>
}
static INVENTORY: spin::Once<Inventory> = spin::Once::new();

/// Retain the machine-derived inventory after heap initialization, before
/// services or application roots can use it. There is no fixed device limit;
/// ordinary memory admission bounds storage. Failure publishes nothing and
/// refuses service startup, rather than dropping peripherals from the view.
pub(crate) fn retain_inventory() -> Result<(), Refusal>
{
    if INVENTORY.get().is_some()
    {
        return Ok(());
    }
    let tree = admitted_tree().ok_or(Refusal { key:   NO_KEY,
                                               cause: Cause::Tree })?;
    let mut inventory = Inventory { devices:    Vec::new(),
                                    transports: Vec::new() };
    let mut allocation_failed = false;
    let mut retain = |node: &Node<'_>| {
        if allocation_failed
        {
            return;
        }
        if let Some(device) = device_of(&tree, node)
        {
            if inventory.devices.try_reserve(1).is_err()
            {
                allocation_failed = true;
                return;
            }
            inventory.devices.push(device);
        }
    };
    tree.for_each_node(&mut retain).map_err(|_| {
                                        Refusal { key:   NO_KEY,
                                                  cause: Cause::Tree }
                                    })?;
    tree.for_each_compatible(VIRTIO_MMIO, |node| {
            if allocation_failed
            {
                return;
            }
            if let Some(transport) = transport_of(&tree, &node)
            {
                if inventory.transports.try_reserve(1).is_err()
                {
                    allocation_failed = true;
                    return;
                }
                inventory.transports.push(transport);
            }
        })
        .map_err(|_| {
            Refusal { key:   NO_KEY,
                      cause: Cause::Tree }
        })?;
    if allocation_failed
    {
        return Err(Refusal { key:   NO_KEY,
                             cause: Cause::Allocation });
    }
    INVENTORY.call_once(|| inventory);
    Ok(())
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
    if TREE_BLOB.get().is_some()
    {
        return Ok(());
    }
    let blob = unsafe {
                   super::memory::primary_slice(info, info.device_tree, info.device_tree_bytes)
               }.map_err(|_| {
                    Refusal { key:   NO_KEY,
                              cause: Cause::Tree }
                })?;
    Tree::new(blob).map_err(|_| {
                       Refusal { key:   NO_KEY,
                                 cause: Cause::Tree }
                   })?;
    TREE_BLOB.call_once(|| blob);
    Ok(())
}

/// The admitted tree, or `None` before admission.
fn admitted_tree() -> Option<Tree<'static>>
{
    Tree::new(TREE_BLOB.get().copied()?).ok()
}

/// Visit every peripheral the machine describes, in tree order.
///
/// A peripheral is an enabled node that names a part and states a window. Which
/// of them a driver binds is the driver's declaration, matched by bus and key,
/// not a platform list, so a machine with parts this kernel has never seen needs
/// no change here.
fn for_each_device(mut visit: impl FnMut(Device))
{
    if let Some(inventory) = INVENTORY.get()
    {
        inventory.devices.iter().copied().for_each(visit);
        return;
    }
    let Some(tree) = admitted_tree()
    else
    {
        return;
    };
    let _ = tree.for_each_node(|node| {
                    if let Some(device) = device_of(&tree, &node)
                    {
                        visit(device);
                    }
                });
}

/// The peripheral a node describes, or `None` when it describes none.
fn device_of(tree: &Tree<'_>, node: &Node<'_>) -> Option<Device>
{
    if !node.enabled() || node.compatible.is_none() || node.reg.is_none()
    {
        return None;
    }
    let (registers, register_bytes) = node.window()?;
    if register_bytes == 0 || registers == 0
    {
        return None;
    }
    let compatible = node.compatible?;
    let mut key = [0u8; 16];
    for (slot, byte) in key.iter_mut().zip(compatible.as_bytes())
    {
        *slot = *byte;
    }
    let interrupt = interrupt_of(tree, node).unwrap_or(0);
    let mut flags = mac_device_interface::MMIO;
    if node.dma_coherent
    {
        flags |= mac_device_interface::COHERENT_DMA;
    }
    Some(Device { bus: *b"MMIO",
                  key,
                  registers,
                  register_bytes,
                  interrupt,
                  flags })
}

/// How many peripherals the machine described.
pub(crate) fn count() -> usize
{
    if let Some(inventory) = INVENTORY.get()
    {
        return inventory.devices.len();
    }
    let mut count = 0;
    for_each_device(|_| count += 1);
    count
}

/// The transport a node describes, or `None` when it describes none.
///
/// Only the node's own `reg` states where a transport is. Nothing here derives a
/// window from an index: a machine that places its transports unevenly, or
/// leaves the positions between them empty, is described exactly as it is.
fn transport_of(tree: &Tree<'_>, node: &Node<'_>) -> Option<Transport>
{
    let (registers, register_bytes) = node.window()?;
    if registers == 0
       || register_bytes < VIRTIO_MMIO_REGISTERS
       || registers.checked_add(register_bytes).is_none()
    {
        return None;
    }
    Some(Transport { registers,
                     register_bytes,
                     interrupt: interrupt_of(tree, node).unwrap_or(0) })
}

/// Visit every virtio-MMIO transport the machine describes, in tree order, and
/// report how many there were.
///
/// This is a filtered view of the same walk `for_each_device` uses, not a second
/// reading of the tree: a transport's window and interrupt are resolved exactly
/// as any other peripheral's are.
pub(crate) fn for_each_transport(mut visit: impl FnMut(Transport)) -> usize
{
    if let Some(inventory) = INVENTORY.get()
    {
        inventory.transports.iter().copied().for_each(visit);
        return inventory.transports.len();
    }
    let Some(tree) = admitted_tree()
    else
    {
        return 0;
    };
    let mut count = 0;
    let _ = tree.for_each_compatible(VIRTIO_MMIO, |node| {
                    if let Some(transport) = transport_of(&tree, node)
                    {
                        visit(transport);
                        count += 1;
                    }
                });
    count
}

/// How many virtio-MMIO transports the machine describes.
pub(crate) fn transport_count() -> usize
{
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

/// The enumerated peripheral at `index`, or `None` before admission or past the
/// number the machine described.
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

/// The interrupt number a node's first specifier carries.
///
/// The width of a specifier is the interrupt controller's `#interrupt-cells`, so
/// the controller has to be resolved rather than assumed: reading the property
/// by length alone would take a two-cell specifier as a number plus a flag. A
/// node that declares no `interrupts` raises none, which is a fact rather than a
/// default.
fn interrupt_of<'a>(tree: &Tree<'a>, node: &Node<'a>) -> Result<u32, ()>
{
    let Some(specifier) = node.interrupts
    else
    {
        return Ok(0);
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
    if cells == 0 || specifier.len() != cells * 4
    {
        return Err(());
    }
    // The last cell is the trigger type and flags, and a one-cell controller
    // carries only the number itself.
    let start = cells.saturating_sub(2) * 4;
    Ok(u32::from_be_bytes(specifier[start..start + 4].try_into().map_err(|_| ())?))
}
