//! One readonly descriptor and genuine runtime entries shared only by threads
//! of the same Process. The last real THRD retires its mapping before disposal.
#![forbid(unsafe_code)]
use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, Ordering};

use mac_abi::{ACCESS_DENIED, BAD_ABI, NO_MEMORY, SERVICE_BUSY, Status};
use mac_formats::native::runtime::Entries;
use mac_object_table::domain::DomainId;
use spinning_top::Spinlock;

use crate::{images::Binding,
            machine::paging::{process,
                              user::{Error, Mapping}},
            toolbox::handles::OwnerRef};

pub(crate) struct Runtime
{
    owner:         DomainId,
    image:         Binding,
    entries:       Entries,
    descriptor:    usize,
    mapping:       Spinlock<MappingSlot>,
    // One shared Process runtime preserves chronological first-fault custody
    // across all genuine main, child and callback threads.
    fault_claimed: AtomicBool
}

struct MappingSlot
{
    mapping: Option<Mapping>,
    busy:    bool,
    waiting: bool
}

impl Runtime
{
    pub(in crate::machine::riscv64::scheduler) fn prepare(owner: &OwnerRef,
                                                 image: &crate::kernel::LoadedImage,
                                                 space: &process::Pin)
                                                 -> Result<Arc<Self>, Status>
    {
        if !space.matches(owner, image.binding()) || !owner.accepting()
        {
            return Err(ACCESS_DENIED);
        }
        let entries = image.runtime_entries()?;
        let mapping = Mapping::descriptor(space, &crate::kernel::user_service_descriptor())?;
        let descriptor = mapping.user_base();
        if descriptor == 0
        {
            return Err(BAD_ABI);
        }
        Arc::try_new(Self { owner: owner.owner(),
                            image: image.binding(),
                            entries,
                            descriptor,
                            mapping: Spinlock::new(MappingSlot { mapping: Some(mapping),
                                                                 busy:    false,
                                                                 waiting: false }),
                            fault_claimed: AtomicBool::new(false) }).map_err(|_| NO_MEMORY)
    }

    /// Called in the same canonical scheduler turn that installs the original
    /// prepaid fault record and parks siblings. It is never reset or reused.
    pub(in crate::machine::riscv64::scheduler) fn claim_fault(&self) -> bool
    {
        !self.fault_claimed.swap(true, Ordering::AcqRel)
    }

    pub(in crate::machine::riscv64::scheduler) fn matches(&self, owner: &OwnerRef, image: Binding) -> bool
    {
        self.owner == owner.owner() && self.image == image
    }

    pub(in crate::machine::riscv64::scheduler) fn descriptor(&self) -> usize
    {
        self.descriptor
    }

    pub(in crate::machine::riscv64::scheduler) fn callback_loop(&self) -> usize
    {
        self.entries.callback_loop as usize
    }

    pub(in crate::machine::riscv64::scheduler) fn thread_return(&self) -> usize
    {
        self.entries.thread_return as usize
    }

    fn mapping_turn(&self, run: impl FnOnce(&mut Mapping) -> Result<(), Error>)
                    -> Result<(), super::retirement::Failure>
    {
        let mapping = {
            let mut slot = self.mapping.lock();
            if slot.busy
            {
                slot.waiting = true;
                return Err(super::retirement::Failure::rejected(SERVICE_BUSY));
            }
            let mapping = slot.mapping.take();
            if mapping.is_some()
            {
                slot.busy = true;
            }
            mapping
        };
        let Some(mut mapping) = mapping
        else
        {
            return Err(super::retirement::Failure::rejected(BAD_ABI));
        };
        // No descriptor, backing or scheduler guard spans PTE/monitor work.
        let result =
            run(&mut mapping).map_err(|cause| super::retirement::Failure::mapping(cause, &mapping));
        let waiting = {
            let mut slot = self.mapping.lock();
            slot.mapping = Some(mapping);
            slot.busy = false;
            core::mem::take(&mut slot.waiting)
        };
        if waiting
        {
            crate::machine::scheduler::context_released();
        }
        result
    }

    pub(in crate::machine::riscv64::scheduler) fn publish(&self) -> Result<(), super::retirement::Failure>
    {
        self.mapping_turn(|mapping| mapping.publish())
    }

    pub(in crate::machine::riscv64::scheduler) fn retire_if_last(runtime: &Arc<Self>)
                                                        -> Result<(), super::retirement::Failure>
    {
        // New threads clone only a live canonical Process thread's binding.
        // Retired threads admit no new clones. This test concerns the shared
        // runtime owner; Mapping::retire separately drains every real span pin.
        if Arc::strong_count(runtime) != 1
        {
            return Ok(());
        }
        runtime.mapping_turn(|mapping| mapping.retire())
    }
}
