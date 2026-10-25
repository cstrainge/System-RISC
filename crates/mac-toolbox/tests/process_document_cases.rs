//! Native-only document lifetime acceptance. Actors use public SDK services;
//! the capture script supplies a real key only after the foreign-owner probes.
#![cfg(target_arch = "riscv64")]
use core::fmt::Write;

use mac_toolbox::{Cnid, Error, System, TypeCode, VolumeId,
                  abi::{EventKind, processes::*},
                  files::DataFileSpec,
                  processes::{DocumentOpen, DocumentRef}};

use super::Failure;
type Result<T> = core::result::Result<T, Failure>;
fn require(value: bool, detail: &'static str) -> Result<()>
{
    if value { Ok(()) } else { Err(Failure::Contract(detail)) }
}
fn error_message(error: Error, expected: Error, word: &str) -> Result<()>
{
    require(error == expected, "document refusal changed typed identity")?;
    let mut detail = crate::LogBuffer::new();
    write!(detail, "{error}").map_err(|_| Failure::Contract("document diagnostic overflow"))?;
    require(detail.text().contains(word), "document refusal lost its informative message")
}
fn raw(system: &System<'_>, handle: u64, operation: u32) -> core::result::Result<(), Error>
{
    system.client().document(&mut DocumentCall { operation,
                                                 handle,
                                                 ..Default::default() })
}
fn idle(system: &System<'_>) -> Result<()>
{
    let event = system.events().wait_event(mac_toolbox::EventMask::ALL, 1)?.into_record();
    if event.kind == EventKind::Update && event.window != 0
    {
        let _update = system.windows().update(mac_toolbox::WindowId(event.window))?;
    }
    Ok(())
}
fn number(text: &str, prefix: &str) -> Option<u64>
{
    let suffix = text.strip_prefix(prefix)?;
    if suffix.is_empty()
    {
        return None;
    }
    suffix.bytes().try_fold(0u64, |value, byte| {
                      if !byte.is_ascii_digit()
                      {
                          return None;
                      }
                      value.checked_mul(10)?.checked_add(u64::from(byte - b'0'))
                  })
}
fn create(system: &System<'_>, volume: VolumeId, name: &str) -> Result<DocumentRef>
{
    let mut writer =
        system.files().create_data(DataFileSpec { volume,
                                                   parent: Cnid::ROOT,
                                                   name,
                                                   replacing: None,
                                                   file_type: TypeCode::new(*b"TEXT"),
                                                   creator: TypeCode::new(*b"PRTS") },
                                    3)?;
    writer.write_all(b"doc")?;
    let file = writer.finish()?;
    Ok(DocumentRef { volume,
                     id: file.id })
}
fn pool(system: &System<'_>, kind: [u8; 4]) -> Result<mac_toolbox::inspection::Pool>
{
    for _ in 0..128
    {
        let read = || -> mac_toolbox::inspection::Result<Option<mac_toolbox::inspection::Pool>> {
            let snapshot = system.inspection().snapshot(mac_toolbox::inspection::Scope::Own)?;
            let ledger = snapshot.ledger(0)?;
            for at in 0..ledger.len()
            {
                let row = ledger.at(at)?;
                if row.kind == kind
                {
                    return Ok(Some(row));
                }
            }
            Ok(None)
        };
        match read()
        {
            Ok(Some(row)) => return Ok(row),
            Ok(None) =>
            {
                return Err(Failure::Contract("document registration absent from actual owner \
                                              ledger"));
            }
            Err(error) if error.cause == Error::ScanStale =>
            {
                let mut detail = crate::LogBuffer::new();
                write!(detail, "{error}").map_err(|_| {
                                             Failure::Contract("inspection diagnostic overflow")
                                         })?;
                require(detail.text().contains("snapshot") || detail.text().contains("revision"),
                        "stale inspection did not explain how to obtain a fresh read")?;
                idle(system)?;
            }
            Err(error) => return Err(Failure::Service(error.cause.status()))
        }
    }
    Err(Failure::Contract("document ledger did not stabilize within bounded snapshot retries"))
}
pub fn send(system: &System<'_>, volume: VolumeId, worker: Cnid) -> Result<()>
{
    let files = system.files();
    files.rename(volume, worker, "Document Worker")?;
    let application = DocumentRef { volume,
                                    id: worker };
    let mut held: [Option<DocumentOpen<'_>>; 20] = core::array::from_fn(|_| None);
    for slot in &mut held
    {
        let batch = system.processes().begin_document_open(application)?;
        require(batch.id().0 > u64::from(u32::MAX),
                "document batch still uses a narrow projection")?;
        *slot = Some(batch);
    }
    require(pool(system, *b"DBAT")?.live == 20,
            "document delivery still has a fixed sixteen-batch cap")?;
    for slot in &mut held
    {
        let batch = slot.take().ok_or(Failure::Contract("missing held document guard"))?;
        let stale = batch.id();
        drop(batch);
        error_message(raw(system, stale.0, DOCUMENT_ABORT).unwrap_err(),
                      Error::NotFound,
                      "not found")?;
    }
    let start = system.events().uptime_ticks()?;
    while pool(system, *b"DBAT")?.pending != 0
    {
        require(system.events().uptime_ticks()?.wrapping_sub(start) < 300,
                "dropped document batches did not drain")?;
        idle(system)?;
    }
    let mut batch = system.processes().begin_document_open(application)?;
    let sender = batch.id();
    require(batch.supported_type_count() == 1
            && batch.accepts(TypeCode::new(*b"TEXT"))?
            && !batch.accepts(TypeCode::new(*b"APPL"))?,
            "resource-declared document types changed")?;
    error_message(batch.push(application).unwrap_err(), Error::NotSupported, "support")?;
    require(system.processes().count()? == 1, "refused document push launched an app")?;
    let mut name = crate::LogBuffer::new();
    write!(name, "document-sender-{}", sender.0).map_err(|_| {
                                                    Failure::Contract("document filename overflow")
                                                })?;
    let first = create(system, volume, name.text())?;
    let second = create(system, volume, "Document Two")?;
    batch.push(first)?;
    error_message(batch.push(first).unwrap_err(), Error::BadArgument, "argument")?;
    batch.push(second)?;
    system.scrap().clear()?;
    let process = batch.commit()?;
    error_message(raw(system, sender.0, DOCUMENT_ABORT).unwrap_err(),
                  Error::NotFound,
                  "not found")?;
    let start = system.events().uptime_ticks()?;
    let receiver = loop
    {
        let mut announcement = [0; 128];
        if let Ok(text) = system.scrap().text(&mut announcement)
        {
            if let Some(id) = number(text, "document-receiver:")
            {
                break id;
            }
        }
        require(system.events().uptime_ticks()?.wrapping_sub(start) < 300,
                "receiver did not publish its document identity")?;
        idle(system)?;
    };
    require(receiver != sender.0,
            "handoff reassigned the sender's object instead of issuing a receiver object")?;
    for operation in [DOCUMENT_AT, DOCUMENT_FINISH, DOCUMENT_ABORT]
    {
        error_message(raw(system, receiver, operation).unwrap_err(),
                      Error::AccessDenied,
                      "protected")?;
    }
    error_message(files.remove(volume, first.id).unwrap_err(), Error::ServiceBusy, "busy")?;
    system.log("PROCESS_DOCUMENT_READY_FOR_INPUT\n")?;
    let start = system.events().uptime_ticks()?;
    while system.processes().is_running(volume, worker)?
    {
        require(system.events().uptime_ticks()?.wrapping_sub(start) < 600,
                "receiver exit did not release document scope")?;
        idle(system)?;
    }
    loop
    {
        match files.remove(volume, first.id)
        {
            Ok(()) => break,
            Err(Error::ServiceBusy) =>
            {
                require(system.events().uptime_ticks()?.wrapping_sub(start) < 600,
                        "document pins remained after receiver exit")?;
                idle(system)?;
            }
            Err(error) => return Err(error.into())
        }
    }
    files.remove(volume, second.id)?;
    loop
    {
        match system.processes().activate(process.id)
        {
            Err(Error::NotFound) => break,
            Err(Error::ServiceBusy) =>
            {
                require(system.events().uptime_ticks()?.wrapping_sub(start) < 600,
                        "document receiver domain did not retire")?;
                idle(system)?;
            }
            _ => return Err(Failure::Contract("retired document receiver remained activatable"))
        }
    }
    files.rename(volume, worker, "ProcessWorker")?;
    system.log("PROCESS_DOCUMENT_LIFETIMES_PASS capacity=20 handoff=passed foreign=passed \
                stale=passed exit=passed\n")?;
    Ok(())
}
pub fn receive(system: &System<'_>) -> Result<bool>
{
    let Some(batch) = system.processes().next_document_batch()?
    else
    {
        return Ok(false);
    };
    require(batch.len() == 2, "receiver observed a partial document set")?;
    let mut notified = false;
    for _ in 0..32
    {
        let event = system.events().next(mac_toolbox::EventMask::ALL)?;
        if let mac_toolbox::Event::DocumentsAvailable { batch: announced,
                                                        .. } = event
        {
            require(announced == batch.id(), "document event truncated the full native batch ID")?;
            notified = true;
            break;
        }
        let raw = event.into_record();
        if raw.kind == EventKind::Update && raw.window != 0
        {
            let _update = system.windows().update(mac_toolbox::WindowId(raw.window))?;
        }
    }
    require(notified, "receiver did not get its typed document notification")?;

    let first = batch.info(0)?;
    let second = batch.info(1)?;
    require(first.file_type == TypeCode::new(*b"TEXT")
            && second.file_type == TypeCode::new(*b"TEXT"),
            "receiver snapshot has wrong document types")?;
    let sender =
        number(first.name(), "document-sender-").ok_or(Failure::Contract("missing sender \
                                                                          identity in document \
                                                                          metadata"))?;
    require(sender != batch.id().0, "receiver retained sender's native ID")?;
    error_message(raw(system, sender, DOCUMENT_AT).unwrap_err(), Error::NotFound, "not found")?;
    error_message(batch.at(2).unwrap_err(), Error::NotFound, "not found")?;
    require(batch.at(0)?.id == first.id && batch.at(1)?.id == second.id,
            "failed read damaged document batch")?;
    let mut announcement = crate::LogBuffer::new();
    write!(announcement,"document-receiver:{}",batch.id().0).map_err(|_|Failure::Contract("document announcement overflow"))?;
    system.scrap().put_text(announcement.text())?;
    let start = system.events().uptime_ticks()?;
    loop
    {
        let event = system.events().wait_event(mac_toolbox::EventMask::ALL, 1)?.into_record();
        if event.kind == EventKind::KeyDown && event.character() == b'x'
        {
            break;
        }
        require(system.events().uptime_ticks()?.wrapping_sub(start) < 600,
                "document input did not reach exact receiver")?;
    }
    require(batch.at(0)?.id == first.id, "foreign disposal released receiver's batch")?;
    require(pool(system, *b"DBAT")?.live == 1
            && pool(system, *b"DBST")?.live == 1
            && pool(system, *b"FPIN")?.live == 4,
            "receiver scope not represented by real ledger resources")?;
    let own = system.processes().current()?;
    let mut message = crate::LogBuffer::new();
    writeln!(message,"PROCESS_DOCUMENT_SCOPE_EXIT app={} batch={}",own.id.0,batch.id().0)
        .map_err(|_|Failure::Contract("document exit diagnostic overflow"))?;
    system.log(message.text())?;
    // System must drain this registered batch and its pin aliases without an
    // app-specific EXIT hook or cooperation from this returning application.
    core::mem::forget(batch);
    Ok(true)
}
