//! Atomic document-set delivery. Resource I/O runs before taking Desktop; the
//! queue owns stable file references and volume pins until its receiver finishes.
#![forbid(unsafe_code)]
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
static CLEANUP: AtomicBool = AtomicBool::new(false);
static WAITING: AtomicBool = AtomicBool::new(false);
static CLEANUP_REVISION: AtomicU64 = AtomicU64::new(1);
fn request_cleanup()
{
    CLEANUP_REVISION.fetch_add(1, Ordering::AcqRel);
    CLEANUP.store(true, Ordering::Release);
}
/// A matching lower-flight retirement is a change, not a physical input event.
pub(crate) fn io_changed()
{
    if WAITING.load(Ordering::Acquire)
    {
        request_cleanup();
    }
}

use mac_abi::{files::{CatalogInfo, FILE, ROOT_ID},
              processes::*,
              *};

use crate::toolbox::{files::{application, pins::ScopeOwner},
                     handles::OwnerRef};
pub(crate) mod storage;
#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase
{
    Probing,
    Preparing,
    Queued,
    Receiving
}
#[derive(Clone, Copy, PartialEq, Eq)]
struct Pending
{
    thread:    u64,
    operation: u32,
    volume:    u32,
    cnid:      u32
}
pub(super) struct Batch
{
    sequence:    u64,
    id:          u64,
    authority:   OwnerRef,
    scope:       ScopeOwner,
    application: (u32, u32),
    drag:        Option<u32>,
    types:       Vec<[u8; 4]>,
    documents:   Vec<CatalogInfo>,
    phase:       Phase,
    pending:     Option<Pending>,
    abandoned:   AtomicBool
}
impl Batch
{
    fn new(authority: OwnerRef, application: (u32, u32), drag: Option<u32>,
           pending: Option<Pending>)
           -> Result<Self, Status>
    {
        let scope = ScopeOwner::new(&authority)?;
        Ok(Self { id: 0,
                  sequence: 0,
                  authority,
                  scope,
                  application,
                  drag,
                  types: Vec::new(),
                  documents: Vec::new(),
                  phase: Phase::Probing,
                  pending,
                  abandoned: AtomicBool::new(false) })
    }

    fn owner(&self) -> u64
    {
        self.authority.owner().incarnation()
    }

    fn abandon(&self)
    {
        if !self.abandoned.swap(true, Ordering::AcqRel)
        {
            request_cleanup();
        }
    }

    fn abandoned(&self) -> bool
    {
        self.abandoned.load(Ordering::Acquire)
    }

    fn receiver(&self, owner: OwnerRef, sequence: u64) -> Result<Self, Status>
    {
        let mut result = Self::new(owner, self.application, None, None)?;
        result.types.try_reserve_exact(self.types.len()).map_err(|_| NO_MEMORY)?;
        result.documents.try_reserve_exact(self.documents.len()).map_err(|_| NO_MEMORY)?;
        result.types.extend_from_slice(&self.types);
        result.documents.extend_from_slice(&self.documents);
        result.phase = Phase::Queued;
        result.sequence = sequence;
        Ok(result)
    }
}
fn desktop<T>(action: impl FnOnce(&mut crate::toolbox::runtime::Desktop) -> Result<T, Status>)
              -> Result<T, Status>
{
    crate::kernel::with_desktop(action).map_err(crate::state::AccessError::status)?
}
fn declarations(creator: [u8; 4], resources: &[mac_formats::resource::Resource])
                -> Result<Vec<[u8; 4]>, Status>
{
    let mut types = Vec::new();
    for resource in resources.iter().filter(|r| r.kind == *b"open")
    {
        let declaration =
            mac_formats::open_types::OpenTypes::decode(&resource.data).map_err(|_| CORRUPT_DATA)?;
        if declaration.signature() != creator
        {
            return Err(CORRUPT_DATA);
        }
        for &kind in declaration.types()
        {
            match types.binary_search(&kind)
            {
                Ok(_) => (),
                Err(at) =>
                {
                    if types.len() == 256
                    {
                        return Err(NO_MEMORY);
                    }
                    types.try_reserve(1).map_err(|_| NO_MEMORY)?;
                    types.insert(at, kind);
                }
            }
        }
    }
    if types.is_empty()
    {
        return Err(NOT_SUPPORTED);
    }
    Ok(types)
}
fn owned<'a>(manager: &'a mut super::Manager, owner: u64, handle: u64)
             -> Result<&'a mut Batch, Status>
{
    let batch = manager.documents.get_mut(handle).ok_or(NOT_FOUND)?;
    if batch.owner() != owner
    {
        return Err(ACCESS_DENIED);
    }
    if batch.abandoned()
    {
        return Err(OPERATION_CANCELLED);
    }
    Ok(batch)
}
fn prepare(manager: &mut super::Manager, owner: u64, call: &DocumentCall)
           -> Result<(u32, u32), Status>
{
    let pending = Pending { thread:    crate::context::current_thread().0,
                            operation: call.operation,
                            volume:    call.volume,
                            cnid:      call.cnid };
    let batch = owned(manager, owner, call.handle)?;
    if batch.phase != Phase::Preparing
    {
        return Err(BAD_ARGUMENT);
    }
    if let Some(previous) = batch.pending
    {
        if previous != pending
        {
            return Err(SERVICE_BUSY);
        }
    }
    if call.operation == DOCUMENT_PUSH
    {
        if batch.documents.len() == MAX_DOCUMENTS
        {
            return Err(NO_MEMORY);
        }
        batch.documents.try_reserve(1).map_err(|_| NO_MEMORY)?;
    }
    else if batch.documents.is_empty()
    {
        return Err(BAD_ARGUMENT);
    }
    batch.pending = Some(pending);
    Ok(batch.application)
}
fn activate_existing(d: &mut crate::toolbox::runtime::Desktop, owner: u64, handle: u64,
                     launch: &mut ProcessCall)
                     -> Result<bool, Status>
{
    let image =
        d.processes
         .iter()
         .find(|process| process.info.volume == launch.volume && process.info.cnid == launch.cnid)
         .map(|process| process.image);
    let Some(image) = image
    else
    {
        return Ok(false);
    };
    let image = crate::kernel::image(image).ok_or(BAD_ABI)?;
    let actual = declarations(image.creator, &image.resources)?;
    let batch = owned(&mut d.processes, owner, handle)?;
    if batch.documents.iter().any(|document| actual.binary_search(&document.file_type).is_err())
    {
        return Err(NOT_SUPPORTED);
    }
    d.activate_existing_application(launch)
}
fn clear_pending(owner: u64, handle: u64)
{
    let _ = desktop(|d| {
        if let Ok(batch) = owned(&mut d.processes, owner, handle)
        {
            batch.pending = None;
        }
        Ok(())
    });
}
fn pending_io() -> bool
{
    crate::toolbox::files::current_io_pending()
}
fn dispose(manager: &mut super::Manager, owner: u64, handle: u64, preparing: bool)
           -> Result<(), Status>
{
    let batch = manager.documents.get(handle).ok_or(NOT_FOUND)?;
    if batch.owner() != owner
    {
        return Err(ACCESS_DENIED);
    }
    if matches!(batch.phase, Phase::Probing | Phase::Preparing) != preparing
    {
        return Err(BAD_ARGUMENT);
    }
    manager.documents.abandon(handle)
}
pub(crate) fn request(owner: u64, call: &mut DocumentCall) -> Status
{
    if call.reserved != [0; 2] || call.operation > DOCUMENT_ACCEPTS_DRAG
    {
        return BAD_ARGUMENT;
    }
    let authority = match crate::context::current_owner()
    {
        Ok(authority) if authority.owner().incarnation() == owner => authority,
        Ok(_) => return ACCESS_DENIED,
        Err(status) => return status
    };
    let result = (|| {
        match call.operation
        {
            DOCUMENT_BEGIN =>
            {
                if call.application_volume == 0 || call.application_cnid <= ROOT_ID
                {
                    return Err(BAD_ARGUMENT);
                }
                let pending = Pending { thread:    crate::context::current_thread().0,
                                        operation: DOCUMENT_BEGIN,
                                        volume:    call.application_volume,
                                        cnid:      call.application_cnid };
                if call.handle == 0
                {
                    call.handle = desktop(|d| {
                        if call.drag != 0
                        {
                            d.drag.offer_live(owner, call.drag)?;
                        }
                        d.processes
                         .documents
                         .prepare(crate::toolbox::handles::document_batch_kinds()?)?;
                        let batch = Batch::new(authority.clone(),
                                               (call.application_volume, call.application_cnid),
                                               (call.drag != 0).then_some(call.drag),
                                               Some(pending))?;
                        d.processes.documents.insert(batch)
                    })?;
                }
                else
                {
                    desktop(|d| {
                        let batch = owned(&mut d.processes, owner, call.handle)?;
                        if batch.phase != Phase::Probing
                           || batch.pending != Some(pending)
                           || batch.application != (call.application_volume, call.application_cnid)
                        {
                            return Err(BAD_ABI);
                        }
                        Ok(())
                    })?;
                }
                let scope =
                    desktop(|d| Ok(owned(&mut d.processes, owner, call.handle)?.scope.lease()))?;
                crate::kernel::with_files(|files| {
                    files.pin_document(&authority, call.handle, scope, call.application_volume,
                                       call.application_cnid)
                }).map_err(crate::state::AccessError::status)??;
                let cached = if application::pending()
                {
                    None
                }
                else
                {
                    desktop(|d| {
                        Ok(d.processes
                            .iter()
                            .find(|process| {
                                process.info.volume == call.application_volume
                                && process.info.cnid == call.application_cnid
                            })
                            .map(|process| process.image))
                    })?
                };
                let types = if let Some(image) = cached.and_then(crate::kernel::image)
                {
                    declarations(image.creator, &image.resources)?
                }
                else
                {
                    // A contract probe reads map + declarations, never CODE.
                    let loaded = application::read_selected(owner,
                                                            call.application_volume,
                                                            call.application_cnid,
                                                            application::Selection::OpenTypes)?;
                    declarations(loaded.catalog.creator, &loaded.resources)?
                };
                desktop(|d| {
                    let batch = owned(&mut d.processes, owner, call.handle)?;
                    call.count = types.len() as u32;
                    batch.types = types;
                    batch.phase = Phase::Preparing;
                    batch.pending = None;
                    Ok(())
                })?;
            }

            DOCUMENT_PUSH =>
            {
                if call.volume == 0 || call.cnid <= ROOT_ID
                {
                    return Err(BAD_ARGUMENT);
                }
                desktop(|d| prepare(&mut d.processes, owner, call))?;
                let catalog = match application::catalog(owner, call.volume, call.cnid)
                {
                    Ok(catalog) => catalog,
                    Err(error) =>
                    {
                        if !pending_io()
                        {
                            clear_pending(owner, call.handle);
                        }
                        return Err(error);
                    }
                };
                let accepted = desktop(|d| {
                    let batch = owned(&mut d.processes, owner, call.handle)?;
                    if catalog.kind != FILE
                       || batch.types.binary_search(&catalog.file_type).is_err()
                    {
                        return Err(NOT_SUPPORTED);
                    }
                    if batch.documents
                            .iter()
                            .any(|doc| doc.volume == catalog.volume && doc.id == catalog.id)
                    {
                        return Err(BAD_ARGUMENT);
                    }
                    Ok(())
                });
                if let Err(error) = accepted
                {
                    clear_pending(owner, call.handle);
                    return Err(error);
                }
                let scope =
                    desktop(|d| Ok(owned(&mut d.processes, owner, call.handle)?.scope.lease()))?;
                let pinned = crate::kernel::with_files(|files| {
                                 files.pin_document(&authority, call.handle, scope, call.volume,
                                                    call.cnid)
                             }).map_err(crate::state::AccessError::status)?;
                if let Err(error) = pinned
                {
                    clear_pending(owner, call.handle);
                    return Err(error);
                }
                desktop(|d| {
                    let batch = owned(&mut d.processes, owner, call.handle)?;
                    batch.documents.push(catalog);
                    batch.pending = None;
                    call.count = batch.documents.len() as u32;
                    Ok(())
                })?;
            }
            DOCUMENT_ACCEPTS_DRAG =>
            {
                call.present = desktop(|d| {
                    let batch = owned(&mut d.processes, owner, call.handle)?;
                    if batch.phase != Phase::Preparing
                    {
                        return Err(BAD_ARGUMENT);
                    }
                    d.drag.accepts_documents(owner, call.drag, &batch.types).map(u32::from)
                })?;
            }
            DOCUMENT_ACCEPTS =>
            {
                call.present = desktop(|d| {
                    let batch = owned(&mut d.processes, owner, call.handle)?;
                    if batch.phase != Phase::Preparing
                    {
                        return Err(BAD_ARGUMENT);
                    }
                    Ok(u32::from(batch.types.binary_search(&call.file_type).is_ok()))
                })?;
            }
            DOCUMENT_COMMIT =>
            {
                let (volume, cnid) = desktop(|d| prepare(&mut d.processes, owner, call))?;
                let mut launch = ProcessCall { operation: LAUNCH,
                                               volume,
                                               cnid,
                                               ..Default::default() };
                // Finish any existing file read before deciding whether a
                // concurrent caller has already launched this exact file.
                let existing = if application::pending()
                {
                    false
                }
                else
                {
                    desktop(|d| activate_existing(d, owner, call.handle, &mut launch))?
                };
                if !existing
                {
                    let loaded = match application::read(owner, volume, cnid)
                    {
                        Ok(loaded) => loaded,
                        Err(error) =>
                        {
                            if !pending_io()
                            {
                                clear_pending(owner, call.handle);
                            }
                            return Err(error);
                        }
                    };
                    let actual = declarations(loaded.catalog.creator, &loaded.resources)?;
                    desktop(|d| {
                        if activate_existing(d, owner, call.handle, &mut launch)?
                        {
                            return Ok(());
                        }
                        let batch = owned(&mut d.processes, owner, call.handle)?;
                        if batch.documents
                                .iter()
                                .any(|document| actual.binary_search(&document.file_type).is_err())
                        {
                            return Err(NOT_SUPPORTED);
                        }
                        d.launch_application(&mut launch, *loaded)
                    })?;
                }
                desktop(|d| {
                    d.processes.get(launch.info.id)?;
                    let recipient = d.processes.ledger(launch.info.id)?;
                    let sequence = d.processes.document_sequence.checked_add(1).ok_or(NO_MEMORY)?;
                    let batch =
                        owned(&mut d.processes, owner, call.handle)?.receiver(recipient.clone(),
                                                                              sequence)?;
                    let scope = batch.scope.lease();
                    let receiver = d.processes.documents.insert(batch)?;
                    let sent = crate::kernel::with_files(|files| {
                                   files.handoff_documents(&authority,
                                                           call.handle,
                                                           &recipient,
                                                           receiver,
                                                           scope,
                                                           || {
                                                               d.notify_document_batch(launch.info
                                                                                             .id,
                                                                                       receiver)
                                                           })
                               }).map_err(crate::state::AccessError::status)
                                 .and_then(|result| result);
                    if let Err(error) = sent
                    {
                        if d.processes.documents.abandon(receiver).is_err()
                        {
                            if let Some(batch) = d.processes.documents.get(receiver)
                            {
                                batch.abandon();
                            }
                        }
                        return Err(error);
                    }
                    d.processes.document_sequence = sequence;
                    d.processes.documents.finish(call.handle);
                    call.handle = 0;
                    call.process = launch.info;
                    Ok(())
                })?;
            }

            DOCUMENT_ABORT => desktop(|d| dispose(&mut d.processes, owner, call.handle, true))?,
            DOCUMENT_NEXT =>
            {
                if call.handle != 0
                {
                    return Err(BAD_ARGUMENT);
                }
                desktop(|d| {
                    let next = d.processes
                                .documents
                                .iter()
                                .filter(|(_, batch)| {
                                    batch.owner() == owner
                                    && batch.phase == Phase::Queued
                                    && !batch.abandoned()
                                })
                                .min_by_key(|(_, batch)| batch.sequence)
                                .map(|(handle, _)| handle);
                    call.present = 0;
                    call.handle = 0;
                    call.count = 0;
                    if let Some(handle) = next
                    {
                        let batch = d.processes.documents.get_mut(handle).ok_or(NOT_FOUND)?;
                        batch.phase = Phase::Receiving;
                        call.handle = handle;
                        call.count = batch.documents.len() as u32;
                        call.present = 1;
                    }
                    Ok(())
                })?;
            }
            DOCUMENT_AT =>
            {
                desktop(|d| {
                    let batch = owned(&mut d.processes, owner, call.handle)?;
                    if batch.phase != Phase::Receiving
                    {
                        return Err(BAD_ARGUMENT);
                    }
                    let doc = batch.documents.get(call.index as usize).ok_or(NOT_FOUND)?;
                    call.volume = doc.volume;
                    call.cnid = doc.id;
                    call.entry = *doc;
                    Ok(())
                })?
            }
            DOCUMENT_FINISH => desktop(|d| dispose(&mut d.processes, owner, call.handle, false))?,
            _ => return Err(BAD_ARGUMENT)
        }
        Ok(())
    })();
    if result.is_err()
       && matches!(call.operation, DOCUMENT_BEGIN | DOCUMENT_PUSH | DOCUMENT_COMMIT)
       && !pending_io()
    {
        clear_pending(owner, call.handle);
    }
    if result.is_err() && call.operation == DOCUMENT_BEGIN && !pending_io() && call.handle != 0
    {
        let _ = desktop(|d| dispose(&mut d.processes, owner, call.handle, true));
    }
    result.err().unwrap_or(OK)
}
/// Abandonment is independent of the dying sender's event loop. Matching lower
/// continuations drain their own I/O; a later unrelated trap is never cancelled.
pub(crate) fn pump()
{
    if !CLEANUP.swap(false, Ordering::AcqRel)
    {
        return;
    }
    let mark = desktop(|d| {
        let revision = CLEANUP_REVISION.load(Ordering::Acquire);
        let batches = &mut d.processes.documents;
        if batches.cleanup_revision != revision
        {
            batches.cleanup_revision = revision;
            batches.cleanup_after = 0;
        }
        let mark =
            batches.retained()
                   .filter(|(id, batch)| {
                       *id > batches.cleanup_after && batch.abandoned() && !batch.scope.closed()
                   })
                   .min_by_key(|(id, _)| *id)
                   .map(|(id, batch)| (id, batch.owner(), batch.pending, batch.application));
        if let Some((id, _, _, _)) = mark
        {
            batches.cleanup_after = id;
        }
        else
        {
            WAITING.store(batches.retained()
                                 .any(|(_, batch)| batch.abandoned() && !batch.scope.closed()),
                          Ordering::Release);
        }
        Ok(mark)
    });
    let (handle, owner, pending, application) = match mark
    {
        Ok(Some(mark)) => mark,
        Ok(None) => return,
        Err(_) =>
        {
            request_cleanup();
            return;
        }
    };
    // Continue this change's bounded pass over the other pending batches.
    // A waiting flight is visited once, then only after a completion wake.
    CLEANUP.store(true, Ordering::Release);
    if let Some(pending) = pending
    {
        let is_application = matches!(pending.operation, DOCUMENT_BEGIN | DOCUMENT_COMMIT);
        let (volume, cnid) =
            if is_application { application } else { (pending.volume, pending.cnid) };
        WAITING.store(true, Ordering::Release);
        if crate::toolbox::files::abandon_document_io(owner, pending.thread, volume, cnid,
                                                      is_application)
        {
            return;
        }
    }
    let cleaned = desktop(|d| {
        let batch = d.processes.documents.completing(handle).ok_or(NOT_FOUND)?;
        if !batch.abandoned() || batch.owner() != owner
        {
            return Err(BAD_ABI);
        }
        // No provider callback/I/O here: close the private scope and retire
        // live aliases only after the real Files service admits this cleanup.
        crate::kernel::with_files(|files| {
            batch.scope.close();
            files.unpin_documents(handle);
        }).map_err(crate::state::AccessError::status)?;
        d.processes.documents.finish(handle);
        Ok(())
    });
    if cleaned.is_err()
    {
        request_cleanup();
    }
}

pub(in crate::toolbox) fn cancel_drag(manager: &mut super::Manager, drag: u32)
{
    loop
    {
        let next = manager.documents.iter().find_map(|(id, batch)| {
                                               (batch.drag == Some(drag)
                                                && matches!(batch.phase,
                                                            Phase::Probing | Phase::Preparing)
                                                && !batch.abandoned()).then_some(id)
                                           });
        let Some(id) = next
        else
        {
            break;
        };
        if manager.documents.abandon(id).is_err()
        {
            if let Some(batch) = manager.documents.get(id)
            {
                batch.abandon();
            }
            break;
        }
    }
}
