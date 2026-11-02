//! Pre-effect canonical FFRK admission. Capacity and the real wire identity are
//! admitted before storage changes. A late owner stop retains the original
//! permit, description and actual committed outcome until terminal cleanup.
use mac_object_table::{CapacityError, projected::PreparedError, store};

use super::{super::objects::prepared::{Error, Permit},
            *};

pub(super) struct Open
{
    permit:                 Option<Permit>,
    value:                  Option<(Permit, OpenFile)>,
    pub(super) description: Arc<OpenDescription>,
    pub(super) published:   Option<u32>,
    pub(super) entry:       Option<CatalogInfo>
}
fn files<R>(f: impl FnOnce(&mut Manager) -> Result<R, Error>) -> Result<R, Error>
{
    crate::kernel::with_files(f).map_err(|error| Error::Index(error.status()))?
}
impl Open
{
    pub(super) fn prepare(owner: &OwnerRef, subject: &Subject, call: &Call)
                          -> Result<Self, Status>
    {
        crate::context::with_allocation(subject.allocation().clone(), || {
            let kinds = crate::toolbox::handles::file_fork_kinds()?;
            files(|manager| {
                manager.validate_file_authority(owner.owner().incarnation(), owner)
                       .map_err(Error::Index)?;
                manager.prepare_forks(kinds).map_err(Error::Index)
            }).map_err(Error::status)?;
            let description = OpenDescription::new(FileFork::Opening, call.fork, call.open_flags)?;
            let mut permit = 'plan: loop
            {
                let plan =
                    files(|manager| manager.handles.plan_fork(owner)).map_err(Error::status)?;
                let mut preparation =
                    Some(plan.prepare().map_err(|cause| Error::Facility(cause).status())?);
                loop
                {
                    let mut retired = None;
                    let result =
                        files(|manager| manager.handles.admit_fork(&mut preparation, &mut retired));
                    if let Some(retired) = retired
                    {
                        retired.dispose();
                    }
                    match result {
                        Ok(permit)=>break 'plan permit,
                        Err(Error::Facility(PreparedError::Native(store::PreparedError::CapacityChanged)))=>{drop(preparation.take());continue 'plan;},
                        Err(Error::Facility(PreparedError::Projection(CapacityError::CapacityChanged)))=>{
                            let prepared=preparation.as_mut().ok_or(BAD_ABI)?;
                            let mut capacity=prepared.prepare_capacity().map_err(|cause|Error::Facility(cause).status())?;
                            prepared.replace_capacity(&mut capacity).map_err(|cause|Error::Facility(cause).status())?;
                            drop(capacity);
                        },
                        Err(error)=>return Err(error.status())
                    }
                }
            };
            loop
            {
                match files(|manager|manager.handles.admit_fork_wire(&mut permit)) {
                    Ok(_actual_wire)=>break,
                    Err(Error::Facility(PreparedError::Projection(CapacityError::CapacityChanged)))=>{
                        let mut capacity=permit.prepare_capacity().map_err(|cause|Error::Facility(cause).status())?;
                        files(|manager|manager.handles.admit_fork_capacity(&permit,&mut capacity)).map_err(Error::status)?;
                        drop(capacity);
                    },
                    Err(error)=>return Err(error.status())
                }
            }
            Ok(Self { permit: Some(permit),
                      value: None,
                      description,
                      published: None,
                      entry: None })
        })
    }

    /// Called only after the actual fork is ready; no late description
    /// allocation. Refusal returns with the same permit/value inside self.
    pub(super) fn publish(&mut self, owner: u64, authority: &OwnerRef, volume: u32,
                          entry: CatalogInfo)
                          -> Result<u32, Status>
    {
        self.entry = Some(entry);
        if let Some(id) = self.published
        {
            return Ok(id);
        }
        if self.value.is_none()
        {
            let permit = self.permit.take().ok_or(BAD_ABI)?;
            self.value =
                Some((permit,
                      OpenFile { owner,
                                 id: entry.id,
                                 volume,
                                 description: self.description.clone(),
                                 retired: core::sync::atomic::AtomicBool::new(false),
                                 last_close: core::sync::atomic::AtomicBool::new(false),
                                 operations: core::sync::atomic::AtomicU32::new(0),
                                 authority: subject::Authority::Client(authority.clone()),
                                 io_subject: None,
                                 resource_index: spinning_top::Spinlock::new(None) }));
        }
        let mut retired = None;
        let result = files(|manager| manager.handles.publish_fork(&mut self.value, &mut retired));
        if let Some(retired) = retired
        {
            retired.dispose();
        }
        match result
        {
            Ok(id) =>
            {
                self.published = Some(id);
                Ok(id)
            }
            Err(error) => Err(error.status())
        }
    }
}
