//! The machine monitor directory the kernel admits through.
//!
//! The kernel refuses the image unless this directory passes the same checks the
//! loader applies here, so it is built and validated before publication rather
//! than assembled optimistically. Each hart owns one control block and the stack
//! above it; the blocks come from the record's firmware heap, which the memory
//! account already reserves, so nothing the kernel is told about is a region it
//! could hand to a process.
use alloc::{alloc::{Layout, alloc_zeroed},
            boxed::Box,
            vec::Vec};

use core::sync::atomic::Ordering;

use mac_abi::{BootMemoryRange,
              boot::monitor::{self, BootImage, Control, Directory, Entry, Signalling}};

use crate::memory::{Memory, PAGE_BYTES};

/// One page per hart, from the record. The control block is 480 bytes and the
/// rest is the monitor's own stack.
pub const MONITOR_STACK_BYTES: u64 = 0x1000;

pub enum Error
{
    /// The machine's tree declares no interrupt controller, so this platform has
    /// no signalling route to publish.
    NoController,
    /// A monitor block could not be allocated.
    Allocation,
    /// The directory did not satisfy the kernel's own admission checks.
    Directory
}

/// Everything the directory points at. The borrow lives as long as the loader,
/// because the kernel reads these cells for System's whole lifetime.
pub struct Published
{
    pub directory: Directory,
    /// One allocation per hart, kept alive so the cells the directory names stay
    /// pinned. Dropping these would leave the kernel reading freed memory.
    _storage:      Vec<*mut u8>,
    _image:        Box<BootImage>,
    _entries:      Box<[Entry]>
}

impl Published
{
    /// The sealed image profile, written once the image is loaded.
    ///
    /// Sealing happens where the image is, not here: the directory is built
    /// before System's code is decoded, so this is the cell the handoff fills in
    /// and then seals. The kernel refuses an unsealed profile, which is why the
    /// seal is the last write.
    pub fn image(&self) -> &BootImage
    {
        &self._image
    }
}

/// Allocates page-aligned zeroed storage, or reports why it could not.
fn block(bytes: u64) -> Option<*mut u8>
{
    let layout = Layout::from_size_align(bytes as usize, PAGE_BYTES as usize).ok()?;
    // SAFETY: the layout has a nonzero size and a valid power-of-two alignment,
    // and the returned pointer is checked before use. Storage is zeroed because
    // the kernel reads these cells before the monitor writes them.
    let pointer = unsafe { alloc_zeroed(layout) };
    (!pointer.is_null()).then_some(pointer)
}

/// Builds the directory for `cpus`, publishing the platform's own signalling
/// route.
///
/// The route is a shared interrupt controller rather than a bank of per-hart
/// cells: an entry's `signal` is a target encoding this platform defines, which
/// is why the kernel can check that it was stated and is not shared, but not
/// what it means.
pub fn prepare(memory: &Memory, cpus: &[mac_abi::boot::topology::Cpu],
               controller: BootMemoryRange, timebase_hz: u64, primary: u64)
               -> Result<Published, Error>
{
    if controller.length == 0
    {
        return Err(Error::NoController);
    }
    let mut storage = Vec::new();
    storage.try_reserve_exact(cpus.len()).map_err(|_| Error::Allocation)?;
    let mut entries = Vec::new();
    entries.try_reserve_exact(cpus.len()).map_err(|_| Error::Allocation)?;
    for cpu in cpus
    {
        let base = block(MONITOR_STACK_BYTES).ok_or(Error::Allocation)?;
        // SAFETY: the block is exclusive, page aligned and at least
        // MONITOR_STACK_BYTES, which covers Control; it is published only after
        // every field is written.
        unsafe {
            (base as *mut Control).write(Control::new(cpu.hart, signal(cpu.hart)));
        }
        // The kernel requires each row to state the state it is actually in:
        // the primary has joined as primary, and every other hart is parked -
        // which is the truth here, because the peers are waiting in this
        // bootstrap's own idle loop and are not running anything. A row that
        // claimed otherwise would be an authority this bootstrap cannot confer.
        unsafe {
            let control = base as *mut Control;
            let joined = if cpu.hart == primary { monitor::JOIN_PRIMARY } else { monitor::JOIN_PARKED };
            (*control).joined.store(joined, Ordering::Release);
        }
        storage.push(base);
        entries.push(Entry { hart:    cpu.hart,
                             signal:  signal(cpu.hart),
                             storage: BootMemoryRange { base:   base as u64,
                                                        length: MONITOR_STACK_BYTES } });
    }
    let image = Box::new(BootImage::new());
    let entries: Box<[Entry]> = entries.into_boxed_slice();
    let directory = Directory { entries:     entries.as_ptr(),
                                count:       entries.len(),
                                stack_bytes: MONITOR_STACK_BYTES,
                                timebase_hz,
                                signalling:  Signalling::Controller(controller),
                                image:       &*image as *const BootImage };
    let firmware = BootMemoryRange { base:   crate::FW_HEAP_BASE,
                                     length: crate::FW_HEAP_BYTES };
    directory.checked(&entries, cpus, memory.checked(PAGE_BYTES).map_err(|_| Error::Directory)?,
                      MONITOR_STACK_BYTES, firmware)
             .map_err(|_| Error::Directory)?;
    Ok(Published { directory,
                   _storage: storage,
                   _image: image,
                   _entries: entries })
}

/// The target this hart is signalled through.
///
/// On a controller platform this is the platform's own encoding, not an
/// address: bit N names hart N in a target list. It is never zero, because zero
/// means "no hart", and the kernel rejects a zero target as an undeclared route.
fn signal(hart: u64) -> u64
{
    1u64 << (hart & 31)
}
