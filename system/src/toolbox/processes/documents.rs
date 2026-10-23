//! Atomic document-set delivery. Resource I/O runs before taking Desktop; the
//! queue owns stable file references and volume pins until its receiver finishes.
#![forbid(unsafe_code)]
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};
static CLEANUP: AtomicBool = AtomicBool::new(false);

use mac_abi::{files::{CatalogInfo, FILE, ROOT_ID},
              processes::*,
              *};

use crate::toolbox::files::application;
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
    owner:       u64,
    application: (u32, u32),
    drag:        Option<u32>,
    types:       Vec<[u8; 4]>,
    documents:   Vec<CatalogInfo>,
    phase:       Phase,
    pending:     Option<Pending>,
    abandoned:   bool
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
fn owned<'a>(manager: &'a mut super::Manager, owner: u64, handle: u32)
             -> Result<&'a mut Batch, Status>
{
    let batch = manager.documents.get_mut(handle).ok_or(NOT_FOUND)?;
    if batch.owner != owner
    {
        return Err(ACCESS_DENIED);
    }
    if batch.abandoned
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
fn activate_existing(d: &mut crate::toolbox::runtime::Desktop, owner: u64, handle: u32,
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
fn clear_pending(owner: u64, handle: u32)
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
fn unpin(handle: u32) -> Result<(), Status>
{
    crate::kernel::with_files(|files|files.unpin_documents(handle)).map_err(crate::state::AccessError::status)
}
fn dispose(manager: &mut super::Manager, owner: u64, handle: u32, preparing: bool)
           -> Result<(), Status>
{
    let batch = manager.documents.get_mut(handle).ok_or(NOT_FOUND)?;
    if batch.owner != owner
    {
        return Err(ACCESS_DENIED);
    }
    if matches!(batch.phase, Phase::Probing | Phase::Preparing) != preparing
    {
        return Err(BAD_ARGUMENT);
    }
    if batch.pending.is_some()
    {
        batch.abandoned = true;
        CLEANUP.store(true, Ordering::Release);
        return Ok(());
    }
    batch.abandoned = true;
    CLEANUP.store(true, Ordering::Release);
    unpin(handle)?;
    manager.documents.remove(handle).ok_or(NOT_FOUND)?;
    Ok(())
}
pub(crate) fn request(owner: u64, call: &mut DocumentCall) -> Status
{
    if call.reserved != [0; 2] || call.operation > DOCUMENT_ACCEPTS_DRAG
    {
        return BAD_ARGUMENT;
    }
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
                         .insert(Batch { sequence: 0,
                                         owner,
                                         application: (call.application_volume,
                                                       call.application_cnid),
                                         drag: (call.drag != 0).then_some(call.drag),
                                         types: Vec::new(),
                                         documents: Vec::new(),
                                         phase: Phase::Probing,
                                         pending: Some(pending),
                                         abandoned: false })
                         .map_err(|_| NO_MEMORY)
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
                crate::kernel::with_files(|files| {
                    files.pin_document(owner, call.handle, call.application_volume,
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
                let pinned = crate::kernel::with_files(|files| {
                                 files.pin_document(owner, call.handle, call.volume, call.cnid)
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
                    d.processes.document_sequence =
                        d.processes.document_sequence.checked_add(1).ok_or(NO_MEMORY)?;
                    let sequence = d.processes.document_sequence;
                    let batch = owned(&mut d.processes, owner, call.handle)?;
                    batch.sequence = sequence;
                    batch.owner = launch.info.id;
                    batch.phase = Phase::Queued;
                    batch.drag = None;
                    batch.pending = None;
                    if let Err(error) = d.notify_document_batch(launch.info.id, call.handle)
                    {
                        let batch = d.processes.documents.get_mut(call.handle).ok_or(BAD_ABI)?;
                        batch.owner = owner;
                        batch.phase = Phase::Preparing;
                        return Err(error);
                    }
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
                                    batch.owner == owner
                                    && batch.phase == Phase::Queued
                                    && !batch.abandoned
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
pub(in crate::toolbox) fn close_owner(manager: &mut super::Manager, owner: u64)
{
    // Exit already abandons the owner's provider flights. Pins are RAM-only;
    // remove the borrowed batch IDs before retiring their process generation.
    loop
    {
        let Some(handle) =
            manager.documents.iter().find_map(|(id, batch)| (batch.owner == owner).then_some(id))
        else
        {
            break;
        };
        if unpin(handle).is_err()
        {
            if let Some(batch) = manager.documents.get_mut(handle)
            {
                batch.abandoned = true;
                CLEANUP.store(true, Ordering::Release);
            }
            break;
        }
        manager.documents.remove(handle);
    }
}

/// Abandonment is independent of the dying sender's event loop. Matching lower
/// continuations drain their own I/O; a later unrelated trap is never cancelled.
pub(crate) fn pump()
{
    if !CLEANUP.load(Ordering::Acquire)
    {
        return;
    }
    let mark = desktop(|d| {
        let mark = d.processes.documents.iter().find_map(|(id, batch)| {
                                                   batch.abandoned.then_some((id,
                                                                              batch.owner,
                                                                              batch.pending,
                                                                              batch.application))
                                               });
        if mark.is_none()
        {
            CLEANUP.store(false, Ordering::Release);
        }
        Ok(mark)
    });
    let Ok(Some((handle, owner, pending, application))) = mark
    else
    {
        return;
    };
    if let Some(pending) = pending
    {
        let (volume, cnid) = if matches!(pending.operation, DOCUMENT_BEGIN | DOCUMENT_COMMIT)
        {
            application
        }
        else
        {
            (pending.volume, pending.cnid)
        };
        if crate::toolbox::files::abandon_document_io(owner,
                                                      pending.thread,
                                                      volume,
                                                      cnid,
                                                      matches!(pending.operation,
                                                               DOCUMENT_BEGIN | DOCUMENT_COMMIT))
        {
            return;
        }
    }
    if unpin(handle).is_err()
    {
        return;
    }
    let _ = desktop(|d| {
        if d.processes
            .documents
            .get(handle)
            .is_some_and(|batch| batch.abandoned && batch.owner == owner)
        {
            d.processes.documents.remove(handle);
        }
        Ok(())
    });
}

pub(in crate::toolbox) fn cancel_drag(manager: &mut super::Manager, drag: u32)
{
    for batch in manager.documents.values_mut()
    {
        if batch.drag == Some(drag) && matches!(batch.phase, Phase::Probing | Phase::Preparing)
        {
            batch.abandoned = true;
            CLEANUP.store(true, Ordering::Release);
        }
    }
}
