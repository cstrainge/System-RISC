use mac_vdagent_protocol::{Capabilities, Capability, ClipboardData, Format, Frame, Limits,
                           Message, MessageType, OwnedMessage, Selection};

use crate::{CompletionError, Error, Output, OutputToken, Sent, Settings,
            output::{Purpose, Queue, SLOTS, Transmission}};

/// An immutable resident pin supplied by the integration layer. `revision`
/// changes monotonically on every committed Scrap mutation. The content of one
/// snapshot must never change. This trait does not represent a write grant.
pub trait Snapshot
{
    fn revision(&self) -> u64;
    fn content(&self) -> Content<'_>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Content<'a>
{
    Empty,
    Text(&'a str),
    Unsupported
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Epoch(u64);
impl Epoch
{
    pub fn get(self) -> u64
    {
        self.0
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FetchToken
{
    epoch: Epoch,
    id:    u64
}
impl FetchToken
{
    pub fn epoch(self) -> Epoch
    {
        self.epoch
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImportToken
{
    fetch:    FetchToken,
    expected: u64,
    offer:    u64
}
impl ImportToken
{
    pub fn epoch(self) -> Epoch
    {
        self.fetch.epoch
    }

    pub fn expected_revision(self) -> u64
    {
        self.expected
    }
}
/// Borrowed decoded text survives until the caller completes this effect. The
/// caller must validate its separate resident grant and atomically compare
/// `expected_revision` before replacing Scrap, then report the actual pin.
#[derive(Clone, Copy, Debug)]
pub struct Import<'a>
{
    token: ImportToken,
    text:  &'a str
}
impl<'a> Import<'a>
{
    pub fn token(self) -> ImportToken
    {
        self.token
    }

    pub fn text(self) -> &'a str
    {
        self.text
    }
}
#[derive(Clone, Copy, Debug)]
pub enum Effect<'a>
{
    Idle,
    Negotiated,
    Ignored,
    Discarded,
    Unavailable,
    Import(Import<'a>)
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase
{
    Closed,
    Negotiating,
    Ready,
    ResetRequired
}
#[derive(Clone, Copy, Debug)]
pub struct Config
{
    limits:       Limits,
    maximum_text: usize,
    settings:     Settings
}
impl Config
{
    pub fn new(limits: Limits, maximum_text: usize) -> Result<Self, Error>
    {
        // Announcements, selected requests and serial-bearing grabs all fit.
        if limits.maximum_message_bytes() < 32
           || maximum_text > limits.maximum_text_bytes(local_capabilities())
        {
            return Err(Error::Configuration);
        }
        Ok(Self { limits,
                  maximum_text,
                  settings: Settings::default() })
    }

    pub fn qemu() -> Self
    {
        let limits = Limits::qemu();
        Self { limits,
               maximum_text: limits.maximum_text_bytes(local_capabilities()),
               settings: Settings::default() }
    }

    pub fn maximum_text_bytes(self) -> usize
    {
        self.maximum_text
    }

    pub fn limits(self) -> Limits
    {
        self.limits
    }

    /// Select policy before constructing an engine. Replacing policy on a live
    /// connection requires close, real grant revocation, transport reset and a
    /// new engine/session; there is no in-place policy/authority mutation.
    pub fn with_settings(mut self, settings: Settings) -> Self
    {
        self.settings = settings;
        self
    }

    pub fn settings(self) -> Settings
    {
        self.settings
    }
}
const fn local_capabilities() -> Capabilities
{
    Capabilities::empty().with(Capability::ClipboardByDemand)
                         .with(Capability::ClipboardSelection)
                         .with(Capability::ClipboardGrabSerial)
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Owner
{
    None,
    Local,
    Host
}
#[derive(Clone, Copy)]
struct Offer
{
    id:       u64,
    expected: u64
}
#[derive(Clone, Copy)]
enum FetchState
{
    Queued,
    Waiting,
    Importing
}
struct Fetch
{
    token: ImportToken,
    stale: bool,
    state: FetchState
}
struct Handshake
{
    peer:       Option<Capabilities>,
    advertised: bool,
    replied:    bool
}
impl Handshake
{
    fn new() -> Self
    {
        Self { peer:       None,
               advertised: false,
               replied:    false }
    }
}

/// One outstanding unlabelled host fetch, one immutable local snapshot, and four
/// bounded outbound message owners. Only one queued text response is admitted.
/// Scheduling, deadlines, subscriptions and authorization belong to the caller.
pub struct Engine<S: Snapshot>
{
    config:       Config,
    phase:        Phase,
    epoch:        Epoch,
    counter:      u64,
    capabilities: Capabilities,
    handshake:    Handshake,
    local:        Option<S>,
    local_error:  Option<Error>,
    dirty:        bool,
    owner:        Owner,
    serial:       u32,
    offer:        u64,
    desired:      Option<Offer>,
    fetch:        Option<Fetch>,
    output:       Queue
}
impl<S: Snapshot> Engine<S>
{
    pub fn new(config: Config) -> Self
    {
        Self { config,
               phase: Phase::Closed,
               epoch: Epoch(0),
               counter: 0,
               capabilities: Capabilities::empty(),
               handshake: Handshake::new(),
               local: None,
               local_error: None,
               dirty: false,
               owner: Owner::None,
               serial: 0,
               offer: 0,
               desired: None,
               fetch: None,
               output: Queue::new() }
    }

    pub fn phase(&self) -> Phase
    {
        self.phase
    }

    pub fn epoch(&self) -> Epoch
    {
        self.epoch
    }

    pub fn capabilities(&self) -> Capabilities
    {
        self.capabilities
    }

    pub fn settings(&self) -> Settings
    {
        self.config.settings
    }

    pub fn queued_messages(&self) -> usize
    {
        self.output.len()
    }

    pub fn local_revision(&self) -> Option<u64>
    {
        self.local.as_ref().map(Snapshot::revision)
    }

    /// Borrow the retained immutable pin to authorize an actual compare/import.
    /// The caller cannot replace engine state or manufacture a write grant.
    pub fn local_snapshot(&self) -> Option<&S>
    {
        self.local.as_ref()
    }

    pub fn pending_fetch(&self) -> Option<FetchToken>
    {
        self.fetch.as_ref().map(|fetch| fetch.token.fetch)
    }

    pub fn fetch_is_quarantined(&self) -> bool
    {
        self.fetch.as_ref().is_some_and(|fetch| fetch.stale)
    }

    /// Call only after the actual transport was reset and its receive decoder
    /// replaced. Reopening this state alone cannot revoke old bytes in a device.
    /// The initial local pin is observed without exporting preexisting content;
    /// subsequent stable `observe` calls identify actual local copy operations.
    /// Exactly one immutable capability request is queued. Readiness additionally
    /// requires its full write acceptance and the host's request=0 reply; a
    /// crossed host request=1 never queues another guest advertisement.
    pub fn open_after_transport_reset(&mut self, snapshot: S) -> Result<Epoch, Error>
    {
        self.reset();
        self.phase = Phase::Closed;
        self.local_error = if self.config.settings.permits_export()
        {
            match snapshot.content()
            {
                Content::Text(text) => self.validate(text).err(),
                _ => None
            }
        }
        else
        {
            None
        };
        self.local = Some(snapshot);
        if !self.config.settings.enabled()
        {
            return Err(Error::Disabled);
        }
        self.epoch = Epoch(self.epoch.0.checked_add(1).ok_or(Error::Exhausted)?);
        self.serial = 0;
        self.offer = 0;
        self.capabilities = Capabilities::empty();
        let message = OwnedMessage::announce(local_capabilities(), true, self.config.limits)?;
        self.enqueue(message, Purpose::Capabilities)?;
        self.phase = Phase::Negotiating;
        Ok(self.epoch)
    }

    pub fn close(&mut self, epoch: Epoch) -> Result<(), Error>
    {
        if epoch != self.epoch
        {
            return Err(Error::StaleEpoch);
        }
        self.reset();
        self.phase = Phase::Closed;
        Ok(())
    }

    fn reset(&mut self)
    {
        self.phase = Phase::ResetRequired;
        self.output = Queue::new();
        self.fetch = None;
        self.desired = None;
        self.owner = Owner::None;
        self.dirty = false;
        self.handshake = Handshake::new();
    }

    fn finish_handshake(&mut self) -> bool
    {
        if self.phase == Phase::Negotiating && self.handshake.advertised && self.handshake.replied
        {
            if let Some(capabilities) = self.handshake.peer
            {
                self.capabilities = capabilities;
                self.phase = Phase::Ready;
                return true;
            }
        }
        false
    }

    fn check(&self, epoch: Epoch) -> Result<(), Error>
    {
        if epoch != self.epoch
        {
            return Err(Error::StaleEpoch);
        }
        match self.phase
        {
            Phase::Closed => Err(Error::Closed),
            Phase::ResetRequired => Err(Error::ResetRequired),
            _ => Ok(())
        }
    }

    fn room(&self) -> Result<(), Error>
    {
        if self.output.len() == SLOTS { Err(Error::QueueFull) } else { Ok(()) }
    }

    fn next_id(&mut self) -> Result<u64, Error>
    {
        match self.counter.checked_add(1)
        {
            Some(id) => Ok(id),
            None =>
            {
                self.reset();
                Err(Error::Exhausted)
            }
        }
    }

    fn enqueue(&mut self, message: OwnedMessage, purpose: Purpose) -> Result<u64, Error>
    {
        self.room()?;
        let id = self.next_id()?;
        self.output.push(Transmission::new(message, id, purpose)?)?;
        self.counter = id;
        Ok(id)
    }

    fn validate(&self, text: &str) -> Result<(), Error>
    {
        if text.len() > self.config.maximum_text
        {
            return Err(Error::TextLimit { bytes:   text.len(),
                                          maximum: self.config.maximum_text });
        }
        if let Some(offset) = text.as_bytes().iter().position(|byte| *byte == 0)
        {
            return Err(Error::EmbeddedNul { offset });
        }
        // All CR, LF and CRLF bytes are preserved. No conversion capability is
        // advertised, no NUL is appended, and Rust/codec validate UTF-8.
        Ok(())
    }

    fn cancel_guest(&mut self) -> Result<(), Error>
    {
        if let Err(cause) = self.output.cancel_guest()
        {
            self.reset();
            return Err(cause);
        }
        Ok(())
    }

    /// A stable snapshot selected by the caller's bounded clear/put batch rule.
    /// This consumes and records a newer local pin even when export is refused;
    /// `poll` retries queued work, never a stale or partial Scrap snapshot.
    pub fn observe(&mut self, snapshot: S) -> Result<(), Error>
    {
        if let Some(previous) = self.local_revision()
        {
            if snapshot.revision() < previous
            {
                return Err(Error::RevisionOrder);
            }
            if snapshot.revision() == previous
            {
                return Ok(());
            }
        }
        self.local_error = if self.config.settings.permits_export()
        {
            match snapshot.content()
            {
                Content::Text(text) => self.validate(text).err(),
                _ => None
            }
        }
        else
        {
            None
        };
        self.local = Some(snapshot);
        if let Some(fetch) = &mut self.fetch
        {
            fetch.stale = true;
        }
        self.desired = None;
        self.dirty = self.config.settings.permits_export();
        if !self.dirty
        {
            return Ok(());
        }
        self.cancel_guest()?;
        if self.phase == Phase::Ready
        {
            self.flush_local()
        }
        else
        {
            self.local_error.map_or(Ok(()), Err)
        }
    }

    fn flush_local(&mut self) -> Result<(), Error>
    {
        if !self.dirty
        {
            return Ok(());
        }
        let content = self.local.as_ref().map(Snapshot::content).unwrap_or(Content::Empty);
        // Snapshot contents are immutable. Cache policy admission per revision,
        // rather than rescanning large text on each demand/allocation retry.
        let validation = self.local_error.map_or(Ok(()), Err);
        let text = match content
        {
            Content::Text(text) if validation.is_ok() => Some(text),
            _ => None
        };
        if text.is_none() && self.owner != Owner::Local
        {
            self.dirty = false;
            self.owner = Owner::None;
            return validation;
        }
        self.room()?;
        let next = if text.is_some() && self.capabilities.contains(Capability::ClipboardGrabSerial)
        {
            match self.serial.checked_add(1)
            {
                Some(value) => Some(value),
                None =>
                {
                    self.reset();
                    return Err(Error::Exhausted);
                }
            }
        }
        else
        {
            None
        };
        let message = if text.is_some()
        {
            OwnedMessage::grab(Selection::Clipboard,
                               next,
                               &[Format::UTF8_TEXT],
                               self.capabilities,
                               self.config.limits)?
        }
        else
        {
            OwnedMessage::release(Selection::Clipboard, self.capabilities, self.config.limits)?
        };
        let has_text = text.is_some();
        self.enqueue(message, Purpose::Guest)?;
        if let Some(serial) = next
        {
            self.serial = serial;
        }
        self.owner = if has_text { Owner::Local } else { Owner::None };
        self.dirty = false;
        validation
    }

    /// Retry retained semantic work after output admission/OOM, or request the
    /// latest host offer after a stale response has been completely drained.
    pub fn poll(&mut self, epoch: Epoch) -> Result<(), Error>
    {
        self.check(epoch)?;
        if self.phase != Phase::Ready
        {
            return Ok(());
        }
        if self.dirty
        {
            self.flush_local()?;
        }
        if self.fetch.is_none()
        {
            if let Some(offer) = self.desired
            {
                self.request_offer(offer)?;
            }
        }
        Ok(())
    }

    fn request_offer(&mut self, offer: Offer) -> Result<(), Error>
    {
        self.room()?;
        let id = self.next_id()?;
        let fetch = FetchToken { epoch: self.epoch,
                                 id };
        let message = OwnedMessage::request(Selection::Clipboard,
                                            Format::UTF8_TEXT,
                                            self.capabilities,
                                            self.config.limits)?;
        self.enqueue(message, Purpose::Fetch(fetch))?;
        self.fetch = Some(Fetch { token: ImportToken { fetch,
                                                       expected: offer.expected,
                                                       offer: offer.id },
                                  stale: false,
                                  state: FetchState::Queued });
        self.desired = None;
        Ok(())
    }

    /// Retry `QueueFull`/allocation refusals using this same retained codec
    /// frame, after draining output. A successful effect consumes the frame.
    /// Transport framing failures must call `malformed` before any later input.
    pub fn receive<'a>(&mut self, epoch: Epoch, frame: &Frame<'a>) -> Result<Effect<'a>, Error>
    {
        self.check(epoch)?;
        if self.fetch.as_ref().is_some_and(|fetch| matches!(fetch.state, FetchState::Importing))
        {
            return Err(Error::ImportPending);
        }
        if self.phase == Phase::Negotiating
           && !matches!(frame.header().kind(),
                        MessageType::ANNOUNCE_CAPABILITIES | MessageType::CLIENT_DISCONNECTED)
        {
            return Err(Error::HandshakeRequired);
        }
        if frame.header().kind() == MessageType::CLIPBOARD && !self.config.settings.permits_import()
        {
            // This immutable direction can never have admitted a host fetch.
            // Refuse before decoding/scanning text that cannot be imported.
            return Err(Error::DirectionDenied { direction: self.config.settings.direction(),
                                                operation: "host import" });
        }
        let message = match frame.message(self.capabilities)
        {
            Ok(message) => message,
            Err(cause) =>
            {
                self.reset();
                return Err(Error::Protocol(cause));
            }
        };
        match message
        {
            Message::Capabilities(peer) =>
            {
                let capabilities = peer.negotiate(local_capabilities());
                if !capabilities.contains(Capability::ClipboardByDemand)
                {
                    self.reset();
                    return Err(Error::UnsupportedPeer);
                }
                if self.handshake.peer.is_some_and(|previous| previous != capabilities)
                {
                    self.reset();
                    return Err(Error::CapabilitiesChanged);
                }
                // This QEMU profile advertises once per actual connection.
                // Its immutable request=1 is already queued/sent and answers
                // the host's crossed initial request. Sending another response
                // makes QEMU reset clipboard serials after peer registration,
                // closing the port. Await the request=0 reply to our own request
                // instead. Unchanged later announcements do not restart it.
                self.handshake.peer = Some(capabilities);
                if !peer.requests_reply()
                {
                    self.handshake.replied = true;
                }
                if self.finish_handshake()
                {
                    Ok(Effect::Negotiated)
                }
                else if self.phase == Phase::Ready
                {
                    Ok(Effect::Ignored)
                }
                else
                {
                    Ok(Effect::Idle)
                }
            }
            Message::ClientDisconnected =>
            {
                self.close(epoch)?;
                Ok(Effect::Idle)
            }
            Message::Grab(grab) =>
            {
                if grab.selection() != Selection::Clipboard
                {
                    return Ok(Effect::Ignored);
                }
                if grab.serial().is_some_and(|serial| serial < self.serial)
                {
                    return Ok(Effect::Discarded);
                }
                let text_offered = grab.formats().contains(Format::UTF8_TEXT);
                let import_allowed = self.config.settings.permits_import();
                let supported = text_offered && import_allowed;
                let offer_id = match self.offer.checked_add(1)
                {
                    Some(id) => id,
                    None =>
                    {
                        self.reset();
                        return Err(Error::Exhausted);
                    }
                };
                let offer = Offer { id:       offer_id,
                                    expected: self.local_revision().ok_or(Error::Closed)? };
                let immediate = supported && self.fetch.is_none();
                match self.output.room_after_guest_cancel()
                {
                    Err(cause) =>
                    {
                        self.reset();
                        return Err(cause);
                    }
                    Ok(false) if immediate => return Err(Error::QueueFull),
                    _ =>
                    {}
                }
                // Stage a request before changing ownership, so OOM allows the
                // caller to retain and retry the exact incoming offer.
                let request = if immediate
                {
                    Some(OwnedMessage::request(Selection::Clipboard,
                                               Format::UTF8_TEXT,
                                               self.capabilities,
                                               self.config.limits)?)
                }
                else
                {
                    None
                };
                let id = if immediate { Some(self.next_id()?) } else { None };
                self.cancel_guest()?;
                if let Some(serial) = grab.serial()
                {
                    self.serial = serial;
                }
                self.offer = offer.id;
                self.owner = Owner::Host;
                self.dirty = false;
                self.desired = if supported { Some(offer) } else { None };
                if let Some(fetch) = &mut self.fetch
                {
                    fetch.stale = true;
                }
                if let (Some(message), Some(id)) = (request, id)
                {
                    let fetch = FetchToken { epoch: self.epoch,
                                             id };
                    self.enqueue(message, Purpose::Fetch(fetch))?;
                    self.fetch = Some(Fetch { token: ImportToken { fetch,
                                                                   expected: offer.expected,
                                                                   offer: offer.id },
                                              stale: false,
                                              state: FetchState::Queued });
                    self.desired = None;
                }
                if text_offered && !import_allowed
                {
                    Err(Error::DirectionDenied { direction: self.config.settings.direction(),
                                                 operation: "host import" })
                }
                else
                {
                    Ok(if supported { Effect::Idle } else { Effect::Ignored })
                }
            }
            Message::Release { selection } =>
            {
                if selection == Selection::Clipboard && self.owner == Owner::Host
                {
                    self.desired = None;
                    self.owner = Owner::None;
                    if let Some(fetch) = &mut self.fetch
                    {
                        fetch.stale = true;
                    }
                }
                Ok(Effect::Idle)
            }
            Message::Request { selection,
                               format } =>
            {
                if self.output.response_pending()
                {
                    return Err(Error::ResponsePending);
                }
                self.room()?;
                let text = if selection == Selection::Clipboard
                              && format == Format::UTF8_TEXT
                              && self.owner == Owner::Local
                              && self.local_error.is_none()
                              && self.config.settings.permits_export()
                {
                    match self.local.as_ref().map(Snapshot::content)
                    {
                        Some(Content::Text(text)) => Some(text),
                        _ => None
                    }
                }
                else
                {
                    None
                };
                let message = match text
                {
                    Some(text) =>
                    {
                        // A Local offer already admitted this immutable pin.
                        OwnedMessage::text(selection, text, self.capabilities, self.config.limits)?
                    }
                    None =>
                    {
                        OwnedMessage::unavailable(selection, self.capabilities, self.config.limits)?
                    }
                };
                self.enqueue(message, Purpose::Response)?;
                Ok(Effect::Idle)
            }
            Message::Data(data) => self.receive_data(data),
            Message::MaximumClipboard(_) | Message::Ignored { .. } => Ok(Effect::Ignored)
        }
    }

    fn receive_data<'a>(&mut self, data: ClipboardData<'a>) -> Result<Effect<'a>, Error>
    {
        let selection = match data
        {
            ClipboardData::Text { selection,
                                  .. }
            | ClipboardData::Unavailable { selection }
            | ClipboardData::Unsupported { selection,
                                         .. } => selection
        };
        if selection != Selection::Clipboard
        {
            return Ok(Effect::Ignored);
        }
        let Some(fetch) = self.fetch.as_ref()
        else
        {
            self.reset();
            return Err(Error::UnexpectedData);
        };
        if matches!(fetch.state, FetchState::Queued)
        {
            self.reset();
            return Err(Error::UnexpectedData);
        }
        if fetch.stale
           || Some(fetch.token.expected) != self.local_revision()
           || fetch.token.offer != self.offer
           || self.owner != Owner::Host
        {
            self.fetch = None;
            return Ok(Effect::Discarded);
        }
        match data
        {
            ClipboardData::Unavailable { .. } =>
            {
                self.fetch = None;
                Ok(Effect::Unavailable)
            }
            ClipboardData::Unsupported { format,
                                         .. } =>
            {
                self.fetch = None;
                Err(Error::UnsupportedFormat { format: format.raw() })
            }
            ClipboardData::Text { text,
                                  .. } =>
            {
                if let Err(cause) = self.validate(text)
                {
                    self.fetch = None;
                    return Err(cause);
                }
                let token = fetch.token;
                if let Some(fetch) = &mut self.fetch
                {
                    fetch.state = FetchState::Importing;
                }
                Ok(Effect::Import(Import { token,
                                           text }))
            }
        }
    }

    /// Report the real atomic Scrap outcome. The imported snapshot must contain
    /// exactly this effect's text and a newer content revision. Its pin becomes
    /// the observed local baseline, suppressing a same-revision change echo.
    /// The caller's typed resident error is retained as `CompletionError::Import`.
    pub fn complete_import<E>(&mut self, import: Import<'_>, result: Result<S, E>)
                              -> Result<(), CompletionError<E>>
    {
        let token = import.token;
        let valid = self.phase == Phase::Ready
                    && self.epoch == token.fetch.epoch
                    && self.fetch.as_ref().is_some_and(|fetch| {
                                              fetch.token == token
                                              && !fetch.stale
                                              && matches!(fetch.state, FetchState::Importing)
                                          })
                    && self.local_revision() == Some(token.expected);
        if !valid
        {
            if self.fetch
                   .as_ref()
                   .is_some_and(|fetch| {
                       fetch.token == token && matches!(fetch.state, FetchState::Importing)
                   })
            {
                self.fetch = None;
            }
            return Err(CompletionError::Engine(Error::StaleCompletion));
        }
        self.fetch = None;
        match result
        {
            Err(cause) => Err(CompletionError::Import(cause)),
            Ok(snapshot) =>
            {
                if snapshot.revision() <= token.expected
                   || !matches!(snapshot.content(),Content::Text(text) if text==import.text)
                {
                    self.reset();
                    return Err(CompletionError::Engine(Error::InvalidCompletion));
                }
                self.local = Some(snapshot);
                self.local_error = None;
                self.dirty = false;
                Ok(())
            }
        }
    }

    pub fn output(&self) -> Option<Output<'_>>
    {
        if matches!(self.phase, Phase::Closed | Phase::ResetRequired)
        {
            None
        }
        else
        {
            self.output.front().map(|tx| tx.view(self.epoch))
        }
    }

    /// `bytes` is exactly what the device accepted from the current three
    /// segments. A zero/partial acceptance leaves the same owner queued.
    pub fn accepted(&mut self, token: OutputToken, bytes: usize) -> Result<Sent, Error>
    {
        self.check(token.epoch)?;
        let tx = self.output.front_mut().ok_or(Error::InvalidProgress)?;
        let done = tx.accepted(self.epoch, token, bytes)?;
        if !done
        {
            return Ok(Sent::Progress);
        }
        let purpose = tx.purpose;
        self.output.remove(0);
        if let Purpose::Capabilities = purpose
        {
            self.handshake.advertised = true;
            if self.finish_handshake()
            {
                return Ok(Sent::Negotiated);
            }
        }
        if let Purpose::Fetch(token) = purpose
        {
            if let Some(fetch) = &mut self.fetch
            {
                if fetch.token.fetch == token
                {
                    fetch.state = FetchState::Waiting;
                }
            }
        }
        Ok(match purpose
        {
            Purpose::Fetch(fetch) => Sent::Fetch(fetch),
            _ => Sent::Message
        })
    }

    /// Arm this deadline when `accepted` reports `Sent::Fetch`. Even a stale
    /// tombstone must be drained or reset: DATA has no response identifier.
    pub fn expire_fetch(&mut self, token: FetchToken) -> Result<(), Error>
    {
        self.check(token.epoch)?;
        if !self.fetch
                .as_ref()
                .is_some_and(|fetch| {
                    fetch.token.fetch == token && matches!(fetch.state, FetchState::Waiting)
                })
        {
            return Err(Error::StaleCompletion);
        }
        self.reset();
        Err(Error::Timeout)
    }

    pub fn malformed(&mut self, epoch: Epoch, cause: mac_vdagent_protocol::Error)
                     -> Result<(), Error>
    {
        self.check(epoch)?;
        self.reset();
        Err(Error::Protocol(cause))
    }
}

#[cfg(test)]
mod tests
{
    use super::*;
    struct Pin(u64);
    impl Snapshot for Pin
    {
        fn revision(&self) -> u64
        {
            self.0
        }

        fn content(&self) -> Content<'_>
        {
            Content::Text("pinned")
        }
    }
    #[test]
    fn label_exhaustion_retires_protocol_state_without_reusing_tokens_or_dropping_local_pin()
    {
        let mut engine = Engine::new(Config::qemu());
        engine.counter = u64::MAX - 1;
        let epoch = engine.open_after_transport_reset(Pin(1)).unwrap();
        let output = engine.output().unwrap();
        let token = output.token;
        let count = output.remaining_bytes();
        engine.accepted(token, count).unwrap();
        engine.phase = Phase::Ready;
        engine.capabilities = local_capabilities();
        assert_eq!(engine.observe(Pin(2)), Err(Error::Exhausted));
        assert_eq!(engine.phase(), Phase::ResetRequired);
        assert_eq!(engine.local_revision(), Some(2));
        assert!(engine.output().is_none());
        assert_eq!(engine.accepted(token, 1), Err(Error::ResetRequired));
        assert_eq!(engine.epoch(), epoch);
        engine.epoch = Epoch(u64::MAX);
        assert_eq!(engine.open_after_transport_reset(Pin(3)), Err(Error::Exhausted));
        assert_eq!(engine.epoch().get(), u64::MAX);
        assert_eq!(engine.local_revision(), Some(3));
        assert_eq!(engine.phase(), Phase::Closed);
    }
}
