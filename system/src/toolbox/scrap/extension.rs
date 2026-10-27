//! Authenticated, resident Scrap observer/import service. Change notification
//! mutates admitted RAM only; no independently loaded code runs under Scrap.
#![forbid(unsafe_code)]

use mac_abi::{ACCESS_DENIED, BAD_ARGUMENT, NOT_FOUND, NOT_SUPPORTED, SERVICE_BUSY, Status};
use mac_module::{Offer, PackageKind};
use mac_object_table::{domain::{CompletionError, PoolError, ReleasedResource},
                       registry::{Release, Source, Specification}};
pub use mac_scrap_extension_interface::{CLASS, VERSION};

use super::{ChangeError, Manager, Revision, Snapshot, SnapshotContent};
use crate::toolbox::handles::{CapacityPolicy, KindCode, KindLease, OwnerRef, Store,
                              cleanup::{Failure, context::Context}};
pub const MAX_READ: usize = mac_scrap_extension_interface::MAX_REPLY;
pub const MAX_IMPORT: usize = mac_scrap_extension_interface::MAX_TEXT;
pub const OFFER: Offer = Offer { class:          CLASS,
                                 version:        VERSION,
                                 request_schema: 1,
                                 reply_schema:   1,
                                 max_request:    mac_scrap_extension_interface::MAX_REQUEST as u32,
                                 max_reply:      mac_scrap_extension_interface::MAX_REPLY as u32,
                                 flags:          Offer::KERNEL_ONLY | Offer::SERIALIZED,
                                 capabilities:   0 };

/// A resident authentication result, never reconstructed from a wire owner ID.
/// Quiescing stops this exact ledger, revoking even a retained authentication.
pub(crate) struct Authority
{
    owner: OwnerRef
}
impl Authority
{
    pub(crate) fn authenticate(registry: &crate::drivers::Registry, caller: u64)
                               -> Result<Self, Error>
    {
        if registry.kind(caller).map_err(Error::Registry)? != PackageKind::Extension
        {
            return Err(Error::NotExtension);
        }
        if registry.state(caller).map_err(Error::Registry)? != crate::drivers::State::Ready
        {
            return Err(Error::NotReady);
        }
        let interface = registry.resolve(caller, CLASS, VERSION).map_err(Error::Registry)?;
        if registry.interface(interface).map_err(Error::Registry)?.module != 0
        {
            return Err(Error::NotResident);
        }
        let owner = registry.module_ledger(caller).map_err(Error::Registry)?;
        let authority = Self { owner };
        authority.live()?;
        Ok(authority)
    }

    fn live(&self) -> Result<(), Error>
    {
        if self.owner.accepting() { Ok(()) } else { Err(Error::Stopped) }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Session(u64);
impl Session
{
    pub fn from_raw(id: u64) -> Self
    {
        Self(id)
    }

    pub fn raw(self) -> u64
    {
        self.0
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pin(u64);
impl Pin
{
    pub fn from_raw(id: u64) -> Self
    {
        Self(id)
    }

    pub fn raw(self) -> u64
    {
        self.0
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Content
{
    Empty,
    Text,
    Unsupported
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Read
{
    pub revision: Revision,
    pub content:  Content,
    pub total:    usize,
    pub copied:   usize
}
#[derive(Debug)]
pub enum Error
{
    Unavailable,
    Registry(crate::drivers::Error),
    NotExtension,
    NotReady,
    NotResident,
    Stopped,
    SessionMissing,
    ForeignSession,
    ReadOnly,
    PinMissing,
    ForeignPin,
    PinSession,
    Offset
    {
        offset: usize,
        length: usize
    },
    TransferLimit
    {
        bytes:   usize,
        maximum: usize
    },
    Storage(PoolError),
    Change(ChangeError)
}
impl core::fmt::Display for Error
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        match self
        {
            Self::Unavailable =>
            {
                f.write_str("Scrap extension service is unavailable; admit its registered \
                             resident stores before starting extensions")
            }
            Self::Registry(cause) =>
            {
                write!(f,
                       "Scrap extension authentication refused: {cause}; declare the resident \
                        SCEX v1 service and use the current admitted extension caller")
            }
            Self::NotExtension =>
            {
                f.write_str("Scrap extension service requires an admitted INIT module; \
                             application and driver identities cannot grant clipboard access")
            }
            Self::NotReady =>
            {
                f.write_str("Scrap extension module has not committed successful initialization; \
                             publish no clipboard hooks and retry only after real activation")
            }
            Self::NotResident =>
            {
                f.write_str("SCEX must resolve to the resident Scrap service; rebuild the service \
                             declaration and System")
            }
            Self::Stopped =>
            {
                f.write_str("Scrap extension owner is stopping; its session authority is revoked \
                             and delayed clipboard work must be discarded")
            }
            Self::SessionMissing =>
            {
                f.write_str("Scrap extension session is absent or disconnected; discard delayed \
                             work and open a fresh authenticated session")
            }
            Self::ForeignSession =>
            {
                f.write_str("Scrap extension session belongs to another module; use this caller's \
                             own authenticated session")
            }
            Self::ReadOnly =>
            {
                f.write_str("Scrap extension session grants observation only; acquire an \
                             explicitly writable session before importing text")
            }
            Self::PinMissing =>
            {
                f.write_str("Scrap extension snapshot pin is absent or retired; retain the \
                             original live pin or take a fresh snapshot")
            }
            Self::ForeignPin =>
            {
                f.write_str("Scrap extension snapshot belongs to another module; use a pin \
                             admitted for this authenticated caller")
            }
            Self::PinSession =>
            {
                f.write_str("Scrap extension snapshot belongs to a different connection session; \
                             discard the old transfer and snapshot this session")
            }
            Self::Offset { offset,
                           length } =>
            {
                write!(f,
                       "Scrap extension snapshot read offset {offset} exceeds its {length} bytes; \
                        retry within the pinned payload")
            }
            Self::TransferLimit { bytes,
                                  maximum } =>
            {
                write!(f,
                       "Scrap extension transfer needs {bytes} bytes but this operation permits \
                        {maximum}; retain the current clipboard and use bounded transfers")
            }
            Self::Storage(cause) =>
            {
                write!(f,
                       "Scrap extension lifetime storage admission failed: {cause}; retain the \
                        current clipboard and retry after freeing memory")
            }
            Self::Change(cause) => cause.fmt(f)
        }
    }
}
impl core::error::Error for Error
{
}
impl Error
{
    pub fn status(&self) -> Status
    {
        match self
        {
            Self::Unavailable | Self::NotResident => NOT_SUPPORTED,
            Self::Registry(cause) => crate::drivers::status(*cause),
            Self::NotExtension | Self::ForeignSession | Self::ForeignPin
            | Self::ReadOnly | Self::PinSession => ACCESS_DENIED,
            Self::NotReady | Self::Stopped => SERVICE_BUSY,
            Self::SessionMissing | Self::PinMissing => NOT_FOUND,
            Self::Offset { .. } | Self::TransferLimit { .. } => BAD_ARGUMENT,
            Self::Storage(cause) => crate::toolbox::handles::projection_status(mac_object_table::projected::Error::Pool(*cause)),
            Self::Change(cause) => cause.status()
        }
    }
}

struct SessionState
{
    writable:     bool,
    acknowledged: Option<Revision>
}
struct Pinned
{
    session:  Session,
    snapshot: Snapshot
}
pub(crate) struct Service
{
    sessions:  Store<SessionState>,
    snapshots: Store<Pinned>,
    latest:    Revision
}
impl Service
{
    pub(crate) fn new(kinds: [KindLease; 4], revision: Revision) -> Result<Self, Error>
    {
        let [sessions, session_storage, snapshots, snapshot_storage] = kinds;
        Ok(Self { sessions:  Store::owned(sessions, session_storage).map_err(Error::Storage)?,
                  snapshots: Store::owned(snapshots, snapshot_storage).map_err(Error::Storage)?,
                  latest:    revision })
    }

    fn session(&self, authority: &Authority, session: Session) -> Result<&SessionState, Error>
    {
        authority.live()?;
        let (owner, state) =
            self.sessions.get_with_owner(session.0).ok_or(Error::SessionMissing)?;
        if owner != authority.owner.owner()
        {
            return Err(Error::ForeignSession);
        }
        Ok(state)
    }

    fn pin(&self, authority: &Authority, session: Session, pin: Pin) -> Result<&Pinned, Error>
    {
        self.session(authority, session)?;
        let (owner, state) = self.snapshots.get_with_owner(pin.0).ok_or(Error::PinMissing)?;
        if owner != authority.owner.owner()
        {
            return Err(Error::ForeignPin);
        }
        if state.session != session
        {
            return Err(Error::PinSession);
        }
        Ok(state)
    }

    /// One latest committed version plus each session's acknowledgement forms
    /// coalesced wake state. A commit never walks sessions or grows a queue.
    fn changed(&mut self, revision: Revision)
    {
        self.latest = revision;
    }

    fn release(&mut self, ticket: ReleasedResource, registration: Registration)
               -> Result<(), CompletionError>
    {
        match registration
        {
            Registration::Sessions => drop(self.sessions.release(ticket)?),
            Registration::SessionStorage => self.sessions.release_storage(ticket)?,
            Registration::Snapshots => drop(self.snapshots.release(ticket)?),
            Registration::SnapshotStorage => self.snapshots.release_storage(ticket)?
        }
        Ok(())
    }
}

impl Manager
{
    pub(crate) fn prepare_extensions(&mut self, kinds: [KindLease; 4]) -> Result<(), Error>
    {
        if self.extensions.is_none()
        {
            self.extensions = Some(Service::new(kinds, self.revision)?);
        }
        Ok(())
    }

    fn extension_service(&self) -> Result<&Service, Error>
    {
        self.extensions.as_ref().ok_or(Error::Unavailable)
    }

    fn extension_service_mut(&mut self) -> Result<&mut Service, Error>
    {
        self.extensions.as_mut().ok_or(Error::Unavailable)
    }

    /// Called only after a real successful mutation; every allocation and
    /// validation already finished before this infallible state update.
    pub(super) fn extension_changed(&mut self)
    {
        if let Some(service) = self.extensions.as_mut()
        {
            service.changed(self.revision);
        }
    }

    pub(crate) fn extension_open(&mut self, authority: &Authority, writable: bool)
                                 -> Result<Session, Error>
    {
        authority.live()?;
        self.extension_service_mut()?
            .sessions
            .insert(&authority.owner,
                    SessionState { writable,
                                   acknowledged: None })
            .map(Session)
            .map_err(Error::Storage)
    }

    /// Read-only observation. A wake remains pending until snapshot admission
    /// succeeds; storage refusal cannot silently consume the notification.
    pub(crate) fn extension_notice(&self, authority: &Authority, session: Session)
                                   -> Result<Option<Revision>, Error>
    {
        let service = self.extension_service()?;
        Ok((service.session(authority, session)?.acknowledged != Some(service.latest))
            .then_some(service.latest))
    }

    pub(crate) fn extension_snapshot(&mut self, authority: &Authority, session: Session)
                                     -> Result<Pin, Error>
    {
        self.extension_service()?.session(authority, session)?;
        let snapshot = self.snapshot();
        let revision = snapshot.revision();
        let service = self.extension_service_mut()?;
        let pin = service.snapshots
                         .insert(&authority.owner,
                                 Pinned { session,
                                          snapshot })
                         .map(Pin)
                         .map_err(Error::Storage)?;
        // A successful insert cannot invalidate this admitted session; no
        // callback or scheduler boundary occurs inside the Scrap operation.
        if let Some(state) = service.sessions.get_mut(session.0)
        {
            state.acknowledged = Some(revision);
        }
        Ok(pin)
    }

    pub(crate) fn extension_read(&self, authority: &Authority, session: Session, pin: Pin,
                                 offset: usize, output: &mut [u8])
                                 -> Result<Read, Error>
    {
        let snapshot = &self.extension_service()?.pin(authority, session, pin)?.snapshot;
        if output.len() > MAX_READ
        {
            return Err(Error::TransferLimit { bytes:   output.len(),
                                              maximum: MAX_READ });
        }
        let (content, bytes): (_, &[u8]) = match snapshot.content()
        {
            SnapshotContent::Empty => (Content::Empty, &[]),
            SnapshotContent::Text(text) => (Content::Text, text.as_bytes()),
            SnapshotContent::Unsupported => (Content::Unsupported, &[])
        };
        let remainder = bytes.get(offset..).ok_or(Error::Offset { offset,
                                                                   length: bytes.len() })?;
        let copied = output.len().min(remainder.len());
        output[..copied].copy_from_slice(&remainder[..copied]);
        Ok(Read { revision: snapshot.revision(),
                  content,
                  total: bytes.len(),
                  copied })
    }

    pub(crate) fn extension_info(&self, authority: &Authority, session: Session, pin: Pin)
                                 -> Result<Read, Error>
    {
        self.extension_read(authority, session, pin, 0, &mut [])
    }

    pub(crate) fn extension_import(&mut self, authority: &Authority, session: Session, pin: Pin,
                                   text: &str)
                                   -> Result<Revision, Error>
    {
        let service = self.extension_service()?;
        if !service.session(authority, session)?.writable
        {
            return Err(Error::ReadOnly);
        }
        let expected = service.pin(authority, session, pin)?.snapshot.revision();
        if text.len() > MAX_IMPORT
        {
            return Err(Error::TransferLimit { bytes:   text.len(),
                                              maximum: MAX_IMPORT });
        }
        let committed = self.compare_replace_text(expected, text).map_err(Error::Change)?;
        // The normal commit recorded the latest observable revision. Only this
        // authenticated origin session acknowledges it, suppressing echo.
        if let Some(state) = self.extension_service_mut()?.sessions.get_mut(session.0)
        {
            state.acknowledged = Some(committed);
        }
        Ok(committed)
    }

    pub(crate) fn extension_close_pin(&mut self, authority: &Authority, session: Session, pin: Pin)
                                      -> Result<(), Error>
    {
        self.extension_service()?.pin(authority, session, pin)?;
        drop(self.extension_service_mut()?.snapshots.remove(pin.0).ok_or(Error::PinMissing)?);
        Ok(())
    }

    pub(crate) fn extension_disconnect(&mut self, authority: &Authority, session: Session)
                                       -> Result<(), Error>
    {
        self.extension_service()?.session(authority, session)?;
        let service = self.extension_service_mut()?;
        service.snapshots.remove_where(|owner, pin| {
                             owner == authority.owner.owner() && pin.session == session
                         });
        drop(service.sessions.remove(session.0).ok_or(Error::SessionMissing)?);
        Ok(())
    }

    pub(crate) fn extension_release(&mut self, ticket: ReleasedResource,
                                    registration: Registration)
                                    -> Result<(), Failure>
    {
        match self.extensions.as_mut()
        {
            Some(service) => service.release(ticket, registration).map_err(Failure::from),
            None => Err(Failure::context_unavailable(ticket, "Scrap extension stores"))
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Registration
{
    Sessions,
    SessionStorage,
    Snapshots,
    SnapshotStorage
}
impl Registration
{
    pub(crate) const ALL: [Self; 4] =
        [Self::Sessions, Self::SessionStorage, Self::Snapshots, Self::SnapshotStorage];

    pub(crate) fn specification(self) -> Specification<'static>
    {
        let (kind, label, storage) = match self
        {
            Self::Sessions => (*b"SCRS", "Scrap extension sessions", false),
            Self::SessionStorage => (*b"SCRB", "Scrap extension session backing", true),
            Self::Snapshots => (*b"SRCP", "Scrap extension snapshot pins", false),
            Self::SnapshotStorage => (*b"SRPB", "Scrap extension snapshot backing", true)
        };
        Specification { kind: KindCode(kind),
                        label,
                        provider: "System Scrap Manager",
                        capacity: if storage
                        {
                            CapacityPolicy::HardBound { slots:  1,
                                                        reason: "One Scrap extension store \
                                                                 backing per module domain" }
                        }
                        else
                        {
                            CapacityPolicy::GrowableUntilMemory
                        },
                        source: Source::Resident }
    }
}
impl<C: Context + ?Sized> Release<C, Failure> for Registration
{
    fn release(&self, context: &mut C, ticket: ReleasedResource) -> Result<(), Failure>
    {
        context.release_scrap_extension(ticket, *self)
    }
}
