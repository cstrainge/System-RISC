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

use mac_abi::BootInfo;
use mac_devicetree::{Node, Tree};

use super::power::platform::DECLARED_DEVICES;

/// How many declared identities the platform has. A capacity, not a machine
/// fact: the record states it and the tree supplies the contents.
pub(crate) const SLOTS: usize = DECLARED_DEVICES.len();

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

/// Why a declared peripheral could not be resolved.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Cause
{
    /// The machine supplied no readable device tree.
    Tree,
    /// The machine's tree does not describe this peripheral.
    Missing,
    /// The node is present but does not describe a usable register window.
    NoWindow,
    /// The node declares interrupts that cannot be attributed to a controller.
    NoInterruptParent,
    /// A slot admission should have published but did not. Admission publishes
    /// every slot or none, so this is an internal inconsistency rather than a
    /// machine fact, and it is still reported rather than skipped.
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

    /// The refusal for a declared slot the inventory did not publish. The
    /// declared identity is still named, so the report points at a peripheral
    /// rather than at an index.
    pub(crate) fn unresolved(index: usize) -> Self
    {
        let key = DECLARED_DEVICES.get(index).map(|&(_, key)| key).unwrap_or(NO_KEY);
        Refusal { key,
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
                       "`{}` declares interrupts but no interrupt controller can be resolved \
                        for it",
                       self.name())
            }
            Cause::Unpublished =>
            {
                write!(out, "`{}` was not resolved by admission", self.name())
            }
        }
    }
}

struct Inventory
{
    devices: [Option<Device>; SLOTS]
}

static INVENTORY: spin::Once<Inventory> = spin::Once::new();

/// Resolve every declared peripheral through the machine's tree.
///
/// Runs before anything that needs a device window, and publishes nothing if any
/// identity fails, so a partially resolved inventory is never observable.
///
/// # Safety
/// The admitted firmware root maps primary RAM in the permanent physmap and the
/// blob stays pinned for the System lifetime, which is what lets the borrowed
/// view live as long as the tree is read.
pub(crate) unsafe fn admit(info: &BootInfo) -> Result<(), Refusal>
{
    if INVENTORY.get().is_some()
    {
        return Ok(());
    }
    let blob = unsafe {
        super::memory::primary_slice(info, info.device_tree, info.device_tree_bytes)
    }.map_err(|_| Refusal { key: NO_KEY,
                             cause: Cause::Tree })?;
    let tree = Tree::new(blob).map_err(|_| Refusal { key: NO_KEY,
                                                     cause: Cause::Tree })?;
    let mut devices: [Option<Device>; SLOTS] = [None; SLOTS];
    for (slot, &(bus, key)) in devices.iter_mut().zip(DECLARED_DEVICES)
    {
        *slot = Some(resolve(&tree, bus, key)?);
    }
    INVENTORY.call_once(|| Inventory { devices });
    Ok(())
}

/// The resolved peripheral at `index`, or `None` before admission or past the
/// declared count.
pub(crate) fn get(index: usize) -> Option<Device>
{
    INVENTORY.get()?.devices.get(index).copied().flatten()
}

fn resolve(tree: &Tree<'_>, bus: [u8; 4], key: [u8; 16]) -> Result<Device, Refusal>
{
    let refuse = |cause| Refusal { key, cause };
    // Borrowed from the key rather than copied into a String: admission runs
    // before the heap exists, and the name is only needed for the match below.
    let end = key.iter().position(|&byte| byte == 0).unwrap_or(key.len());
    let name = core::str::from_utf8(&key[..end]).map_err(|_| refuse(Cause::Missing))?;
    let node = tree.find_compatible(name)
                   .map_err(|_| refuse(Cause::Tree))?
                   .ok_or_else(|| refuse(Cause::Missing))?;
    let (registers, register_bytes) = node.window().ok_or_else(|| refuse(Cause::NoWindow))?;
    if register_bytes == 0 || registers == 0
    {
        return Err(refuse(Cause::NoWindow));
    }
    let interrupt = interrupt_of(tree, &node).map_err(|_| refuse(Cause::NoInterruptParent))?;
    let mut flags = mac_device_interface::MMIO;
    if node.dma_coherent
    {
        flags |= mac_device_interface::COHERENT_DMA;
    }
    Ok(Device { bus,
                key,
                registers,
                register_bytes,
                interrupt,
                flags })
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
