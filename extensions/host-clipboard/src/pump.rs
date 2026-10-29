//! Scheduled, bounded clipboard work. No clipboard path runs on UI input or
//! beneath a module mutex. All wire storage belongs to this actual worker.
use alloc::boxed::Box;
use core::fmt::{self, Write};

use mac_abi::{BAD_ABI, CORRUPT_DATA, IO_ERROR, NO_MEMORY, NOT_SUPPORTED, OPERATION_CANCELLED,
              SERVICE_BUSY};
use mac_byte_channel::{self as channel, ByteChannel, State};
use mac_driver::{Error, Result, lifecycle::Startup, preferences::Preferences};
use mac_driver_alloc::{WorkerContext,
                       scrap::{self, CapturedContent, Scrap, Session}};
use mac_host_clipboard::{Config, Content, Effect, Engine, Epoch, FetchToken, ImportToken, Phase,
                         Sent, Settings, Snapshot};
use mac_vdagent_protocol::{Decoder, Limits};

use crate::io::{Action, Finished, Io};

const TURN: u64 = 16_000;
const BATCH: u64 = 32_000;
const RETRY: u64 = 2_000;
const PREFERENCES: u64 = 1_000_000;
const NEGOTIATE: u64 = 5_000_000;
const TRANSFER: u64 = 5_000_000;

enum Local
{
    Captured(scrap::Captured),
    RevisionOnly(scrap::Snapshot, scrap::Metadata)
}
impl Local
{
    fn pin(&self) -> &scrap::Snapshot
    {
        match self
        {
            Self::Captured(captured) => captured.pin(),
            Self::RevisionOnly(pin, _) => pin
        }
    }

    fn capture(scrap: &mut Scrap, session: &Session, export: bool) -> Result<Self>
    {
        if export
        {
            return scrap.capture(session)
                        .map(Self::Captured)
                        .map_err(|cause| Error(cause.status()));
        }
        let pin = scrap.snapshot(session).map_err(|cause| Error(cause.status()))?;
        let metadata = scrap.info(&pin).map_err(|cause| Error(cause.status()))?;
        Ok(Self::RevisionOnly(pin, metadata))
    }
}
impl Snapshot for Local
{
    fn revision(&self) -> u64
    {
        match self
        {
            Self::Captured(captured) => captured.metadata().revision,
            Self::RevisionOnly(_, metadata) => metadata.revision
        }
    }

    fn content(&self) -> Content<'_>
    {
        match self
        {
            // With export denied, only the revision/CAS authority is observed.
            // No local text bytes are read, scanned or made available to the engine.
            Self::RevisionOnly(..) => Content::Unsupported,
            Self::Captured(captured) =>
            {
                match captured.content()
                {
                    CapturedContent::Empty => Content::Empty,
                    CapturedContent::Text(text) => Content::Text(text),
                    CapturedContent::Unsupported => Content::Unsupported
                }
            }
        }
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
struct Text
{
    bytes:  [u8; 240],
    length: usize
}
impl Text
{
    fn new() -> Self
    {
        Self { bytes:  [0; 240],
               length: 0 }
    }

    fn as_str(&self) -> &str
    {
        core::str::from_utf8(&self.bytes[..self.length]).unwrap_or("Host Clipboard diagnostic \
                                                                    encoding invariant failed")
    }
}
impl Write for Text
{
    fn write_str(&mut self, text: &str) -> fmt::Result
    {
        for mut ch in text.chars()
        {
            if ch.is_control()
            {
                ch = ' ';
            }
            if self.length + ch.len_utf8() > self.bytes.len()
            {
                return Err(fmt::Error);
            }
            let end = self.length + ch.len_utf8();
            ch.encode_utf8(&mut self.bytes[self.length..end]);
            self.length = end;
        }
        Ok(())
    }
}
struct Pump
{
    startup:             Startup,
    preferences:         Preferences,
    io:                  Io,
    scrap:               Scrap,
    session:             Option<Session>,
    engine:              Option<Engine<Local>>,
    epoch:               Option<Epoch>,
    decoder:             Decoder,
    rx:                  [u8; channel::MAX_TRANSFER],
    at:                  usize,
    end:                 usize,
    frame_pending:       bool,
    decoder_retry:       bool,
    imported:            Option<(ImportToken, scrap::Snapshot)>,
    settings:            Option<Settings>,
    preference_revision: Option<u64>,
    preference_due:      u64,
    observe_due:         u64,
    batch:               Option<(u64, u64)>,
    status_due:          u64,
    read_due:            u64,
    reset_due:           Option<u64>,
    reopen_after:        u64,
    last_failure:        Option<(State, i32)>,
    announced_epoch:     Option<Epoch>,
    generation:          u64,
    negotiate_deadline:  Option<u64>,
    fetch_deadline:      Option<(FetchToken, u64)>,
    io_deadline:         Option<u64>,
    closing:             bool,
    need_reset:          bool,
    confirmed:           bool,
    message:             Option<(i32, Text)>,
    message_pending:     bool
}
impl Pump
{
    fn notice(&mut self, status: i32, cause: fmt::Arguments<'_>)
    {
        let mut text = Text::new();
        let _ = text.write_fmt(cause);
        if self.message != Some((status, text))
        {
            self.message = Some((status, text));
            self.message_pending = true;
        }
    }

    fn publish_notice(&mut self)
    {
        if self.message_pending
           && let Some((status, text)) = self.message
        {
            let result = if !self.confirmed && status < 0
            {
                self.startup.refuse(status, text.as_str())
            }
            else
            {
                self.startup.report(status, text.as_str())
            };
            if result.is_ok()
            {
                self.message_pending = false;
            }
        }
    }

    fn close(&mut self)
    {
        self.closing = true;
        self.need_reset = true;
    }

    fn policy(&mut self, now: u64)
    {
        if now < self.preference_due
        {
            return;
        }
        self.preference_due = now.saturating_add(PREFERENCES);
        let result: Result<Option<Settings>> = (|| {
            let revision = self.preferences.revision()?;
            if self.preference_revision == Some(revision)
            {
                return Ok(self.settings);
            }
            let doc = self.preferences.load()?;
            let settings = if let Some(bytes) = doc.bytes()
            {
                Settings::decode(bytes).map_err(|_| Error(CORRUPT_DATA))?
            }
            else
            {
                let settings = Settings::default();
                let mut json = [0; 256];
                let length = settings.encode(&mut json).map_err(|_| Error(CORRUPT_DATA))?;
                self.preferences.save(&doc, &json[..length])?;
                settings
            };
            self.preference_revision = Some(revision);
            Ok(Some(settings))
        })();
        let settings = match result
        {
            Ok(settings) => settings,
            Err(error) =>
            {
                self.notice(error.0,
                            format_args!("Preferences unavailable or invalid ({error:?}); \
                                          sharing stays local; repair the versioned Host \
                                          Clipboard JSON"));
                None
            }
        };
        if self.settings != settings
        {
            self.settings = settings;
            self.close();
            if settings.is_some_and(|policy| !policy.enabled())
            {
                self.notice(NOT_SUPPORTED,
                            format_args!("Sharing disabled in Host Clipboard preferences; local \
                                          Scrap is preserved"));
            }
        }
    }

    fn start(&mut self, action: Action, input: &[u8], now: u64) -> Result<()>
    {
        self.io.start(action, input)?;
        self.io_deadline = Some(now.saturating_add(TRANSFER));
        Ok(())
    }

    fn retire(&mut self) -> Result<bool>
    {
        if self.io.busy()
        {
            return Ok(false);
        }
        self.engine = None;
        self.epoch = None;
        self.imported = None;
        self.session = None;
        self.fetch_deadline = None;
        self.negotiate_deadline = None;
        self.announced_epoch = None;
        self.batch = None;
        let drain = self.scrap.drain(16);
        if let Some(ref cause) = drain.failure
        {
            self.notice(cause.status(),
                        format_args!("Scrap cleanup retains {} records: {cause}", drain.pending));
        }
        if drain.uncertain != 0
        {
            return Err(Error(BAD_ABI));
        }
        Ok(drain.is_complete())
    }

    fn open(&mut self, settings: Settings, now: u64) -> Result<()>
    {
        if self.session.is_none()
        {
            self.session = Some(self.scrap
                                    .open(settings.permits_import())
                                    .map_err(|cause| Error(cause.status()))?);
        }
        let session = self.session.as_ref().ok_or(Error(BAD_ABI))?;
        let snapshot = Local::capture(&mut self.scrap, session, settings.permits_export())?;
        let config = Config::new(Limits::qemu(), 1024 * 1024 - 32).map_err(|_| Error(BAD_ABI))?
                                                                  .with_settings(settings);
        let mut engine = Engine::new(config);
        let epoch = engine.open_after_transport_reset(snapshot).map_err(|_| Error(BAD_ABI))?;
        self.engine = Some(engine);
        self.epoch = Some(epoch);
        self.negotiate_deadline = Some(now.saturating_add(NEGOTIATE));
        self.observe_due = now.saturating_add(TURN);
        Ok(())
    }

    fn observe(&mut self, now: u64)
    {
        if now < self.observe_due || self.frame_pending || self.imported.is_some()
        {
            return;
        }
        self.observe_due = now.saturating_add(TURN);
        let Some(session) = &self.session
        else
        {
            return;
        };
        let revision = match self.scrap.notice(session)
        {
            Ok(Some(revision)) => revision,
            Ok(None) if self.batch.is_none() => return,
            Ok(None) => self.batch.map_or(0, |batch| batch.0),
            Err(cause) =>
            {
                self.notice(cause.status(), format_args!("Scrap notice refused: {cause}"));
                return;
            }
        };
        if self.batch.is_none_or(|batch| batch.0 != revision)
        {
            self.batch = Some((revision, now.saturating_add(BATCH)));
            return;
        }
        if self.batch.is_some_and(|batch| now < batch.1)
        {
            return;
        }
        match Local::capture(&mut self.scrap,
                             session,
                             self.settings.is_some_and(Settings::permits_export))
        {
            Ok(snapshot) =>
            {
                if let Some(engine) = &mut self.engine
                {
                    // observe retains the new pin even on an export refusal;
                    // engine.poll retries queued output without rereading text.
                    let result = engine.observe(snapshot);
                    self.batch = None;
                    if let Err(cause) = result
                    {
                        self.notice(IO_ERROR, format_args!("Guest text offer refused: {cause}"));
                    }
                }
            }
            Err(cause) =>
            {
                self.notice(cause.0,
                            format_args!("Scrap capture refused; latest revision remains queued \
                                          for retry: {cause:?}"))
            }
        }
    }

    fn complete(&mut self, action: Action, length: usize, now: u64) -> Result<()>
    {
        match action
        {
            Action::Reset =>
            {
                if length != 0
                {
                    return Err(Error(BAD_ABI));
                }
                self.decoder = Decoder::new(Limits::qemu());
                self.at = 0;
                self.end = 0;
                self.frame_pending = false;
                self.decoder_retry = false;
                self.need_reset = false;
                self.closing = false;
                self.status_due = now;
                self.read_due = now;
                self.reset_due = None;
            }
            Action::Diagnostic =>
            {
                let mut text = Text::new();
                let detail =
                    core::str::from_utf8(self.io.output(length)).map_err(|_| Error(BAD_ABI))?;
                let _ = write!(text, "Endpoint unavailable: {detail}; local Scrap is preserved");
                self.notice(NOT_SUPPORTED, format_args!("{}", text.as_str()));
            }
            Action::Status =>
            {
                let status = channel::Status::decode(self.io.output(length))?;
                let enabled = self.settings.is_some_and(Settings::enabled);
                self.status_due = now.saturating_add(if status.state == State::Discovering
                                                     {
                                                         RETRY
                                                     }
                                                     else if self.engine.is_some()
                                                     {
                                                         TURN
                                                     }
                                                     else
                                                     {
                                                         250_000
                                                     });
                if status.state == State::Open && enabled
                {
                    if self.engine.is_some() && status.generation != self.generation
                    {
                        self.close();
                    }
                    else if self.engine.is_none() && now >= self.reopen_after
                    {
                        self.last_failure = None;
                        self.generation = status.generation;
                        if let Some(settings) = self.settings
                        {
                            self.open(settings, now)?;
                        }
                    }
                }
                else if status.state != State::Open && self.engine.is_some()
                {
                    self.close();
                }
                else if matches!(status.state, State::Unavailable | State::Failed)
                {
                    let changed = self.last_failure != Some((status.state, status.last_status));
                    self.last_failure = Some((status.state, status.last_status));
                    if changed
                    {
                        self.notice(NOT_SUPPORTED,
                                    format_args!("com.redhat.spice.0 unavailable (state {:?}, \
                                                  status {}); require a compatible graphical \
                                                  QEMU clipboard endpoint",
                                                 status.state, status.last_status));
                    }
                    self.status_due = now.saturating_add(NEGOTIATE);
                    self.reset_due = Some(self.status_due);
                    if changed
                    {
                        self.start(Action::Diagnostic, &[], now)?;
                    }
                }
            }
            Action::Write(token, offered) =>
            {
                let accepted = channel::accepted(self.io.output(length), offered)?;
                let engine = self.engine.as_mut().ok_or(Error(BAD_ABI))?;
                match engine.accepted(token, accepted).map_err(|_| Error(BAD_ABI))?
                {
                    Sent::Fetch(token) =>
                    {
                        self.fetch_deadline = Some((token, now.saturating_add(TRANSFER)))
                    }
                    _ => ()
                }
            }
            Action::Read =>
            {
                self.rx[..length].copy_from_slice(self.io.output(length));
                self.at = 0;
                self.end = length;
                self.read_due = now.saturating_add(if length == 0 { TURN } else { 0 });
            }
        }
        Ok(())
    }

    /// Decode at most one frame per turn. Refused effects retain both decoder
    /// storage and the exact imported pin, never a reconstructed protocol ID.
    fn receive(&mut self) -> Result<bool>
    {
        if !self.frame_pending && (self.at < self.end || self.decoder_retry)
        {
            match self.decoder.feed(&self.rx[self.at..self.end])
            {
                Ok(feed) =>
                {
                    self.at += feed.consumed;
                    self.decoder_retry = false;
                    self.frame_pending = feed.frame.is_some();
                }
                Err(error) =>
                {
                    self.at += error.consumed;
                    self.decoder_retry =
                        matches!(error.cause, mac_vdagent_protocol::Error::NoMemory { .. });
                    return Err(Error(if self.decoder_retry { NO_MEMORY } else { BAD_ABI }));
                }
            }
        }
        if !self.frame_pending
        {
            return Ok(false);
        }
        let frame = self.decoder.current_frame().ok_or(Error(BAD_ABI))?;
        let epoch = self.epoch.ok_or(Error(BAD_ABI))?;
        let engine = self.engine.as_mut().ok_or(Error(BAD_ABI))?;
        let effect = match engine.receive(epoch, &frame)
        {
            Ok(effect) => effect,
            Err(cause) if cause.retry_frame() => return Err(Error(SERVICE_BUSY)),
            Err(cause) =>
            {
                self.frame_pending = false;
                if engine.phase() == Phase::ResetRequired
                {
                    return Err(Error(BAD_ABI));
                }
                self.notice(IO_ERROR,
                            format_args!("Host clipboard message refused; current Scrap \
                                          retained: {cause}"));
                return Ok(true);
            }
        };
        if let Effect::Import(import) = effect
        {
            let expected = engine.local_snapshot().ok_or(Error(BAD_ABI))?;
            let pin = if let Some((token, pin)) = self.imported.take()
            {
                if token != import.token()
                {
                    return Err(Error(BAD_ABI));
                }
                pin
            }
            else
            {
                match self.scrap.import_snapshot(expected.pin(), import.text())
                {
                    Ok(pin) => pin,
                    Err(cause) if cause.is_busy() || cause.status() == NO_MEMORY =>
                    {
                        return Err(Error(SERVICE_BUSY));
                    }
                    Err(cause) =>
                    {
                        let status = cause.status();
                        let _ = engine.complete_import::<scrap::Error>(import, Err(cause.clone()));
                        self.frame_pending = false;
                        self.fetch_deadline = None;
                        self.notice(status,
                                    format_args!("Host import refused; local Scrap retained: \
                                                  {cause}"));
                        return Ok(true);
                    }
                }
            };
            match self.scrap.capture_pin(pin)
            {
                Ok(snapshot) =>
                {
                    engine.complete_import::<scrap::Error>(import, Ok(Local::Captured(snapshot)))
                          .map_err(|_| Error(BAD_ABI))?;
                    self.fetch_deadline = None;
                    self.batch = None;
                }
                Err(failure) =>
                {
                    let status = failure.cause.status();
                    self.imported = Some((import.token(), failure.pin));
                    if status != SERVICE_BUSY && status != NO_MEMORY
                    {
                        return Err(Error(status));
                    }
                    return Err(Error(SERVICE_BUSY));
                }
            }
        }
        else if matches!(effect, Effect::Discarded | Effect::Unavailable)
        {
            self.fetch_deadline = None;
        }
        self.frame_pending = false;
        Ok(true)
    }
}

pub fn run(context: WorkerContext, startup: Startup, preferences: Preferences,
           channel: ByteChannel, scrap: Scrap)
           -> Result<()>
{
    // Allocate retained protocol storage before any broker Start. A fatal
    // context/contract refusal must not drop a live ticket's immutable input
    // with the retiring worker stack.
    let mut pump = Box::try_new(Pump { startup,
                                       preferences,
                                       io: Io::new(channel),
                                       scrap,
                                       session: None,
                                       engine: None,
                                       epoch: None,
                                       decoder: Decoder::new(Limits::qemu()),
                                       rx: [0; channel::MAX_TRANSFER],
                                       at: 0,
                                       end: 0,
                                       frame_pending: false,
                                       decoder_retry: false,
                                       imported: None,
                                       settings: None,
                                       preference_revision: None,
                                       preference_due: 0,
                                       observe_due: 0,
                                       batch: None,
                                       status_due: 0,
                                       read_due: 0,
                                       reset_due: None,
                                       reopen_after: 0,
                                       last_failure: None,
                                       announced_epoch: None,
                                       generation: 0,
                                       negotiate_deadline: None,
                                       fetch_deadline: None,
                                       io_deadline: None,
                                       closing: true,
                                       need_reset: true,
                                       confirmed: false,
                                       message: None,
                                       message_pending: false }).map_err(|_| Error(NO_MEMORY))?;
    let result = run_loop(&context, &mut pump);
    if pump.io.busy()
    {
        let ticket = pump.io.ticket();
        pump.notice(result.as_ref().err().map_or(BAD_ABI, |cause| cause.0),
                    format_args!("Worker stopped with retained channel ticket {ticket}; \
                                  original buffers and Scrap grants quarantined until reset"));
        pump.publish_notice();
        // No terminal acknowledgement occurred. Retain the exact owned input,
        // ticket and guards in their original module allocation; this is
        // quarantine, not successful disposal or automatic fault recovery.
        core::mem::forget(pump);
    }
    else if result.is_err()
    {
        // With no broker ticket, retire guards before their controller. Any
        // refusal retains the SDK's original records through its quarantine.
        let _ = pump.retire();
        pump.publish_notice();
    }
    result
}

fn run_loop(context: &WorkerContext, pump: &mut Pump) -> Result<()>
{
    let mut quitting = false;
    loop
    {
        let now = context.monotonic_microseconds()?;
        quitting |= context.cancelled()?;
        if quitting
        {
            pump.close();
        }
        else
        {
            pump.policy(now);
        }
        pump.publish_notice();
        if pump.io.busy()
        {
            let timeout = pump.io_deadline.is_some_and(|deadline| now >= deadline);
            if timeout
            {
                let ticket = pump.io.ticket();
                pump.notice(IO_ERROR,
                            format_args!("Channel deadline expired; cancelling retained ticket \
                                          {ticket} before reset"));
            }
            if let Some(finished) = pump.io.step((pump.closing && !pump.io.resetting()) || timeout)
            {
                pump.io_deadline = None;
                match finished
                {
                    Finished::Success(action, length) =>
                    {
                        if let Err(cause) = pump.complete(action, length, now)
                        {
                            pump.notice(cause.0,
                                        format_args!("Channel completion refused: {cause:?}; \
                                                      resetting before retry"));
                            pump.close();
                        }
                    }
                    Finished::Failed(action, cause) =>
                    {
                        pump.notice(cause.0,
                                    format_args!("Channel {action:?} failed: {cause:?}; \
                                                  resetting before retry"));
                        pump.close();
                        pump.reopen_after = now.saturating_add(PREFERENCES);
                    }
                    Finished::Cancelled =>
                    {
                        pump.close();
                    }
                }
            }
        }
        if pump.closing && !pump.io.busy()
        {
            match pump.retire()
            {
                Ok(true) =>
                {
                    if pump.need_reset
                    {
                        pump.start(Action::Reset, &[], now)?;
                    }
                }
                Ok(false) if quitting =>
                {
                    pump.notice(SERVICE_BUSY,
                                format_args!("Cancellation retains Scrap cleanup; original \
                                              records quarantined, no release acknowledgement \
                                              claimed"));
                    pump.publish_notice();
                    return Err(Error(SERVICE_BUSY));
                }
                Ok(false) => (),
                Err(cause) =>
                {
                    pump.publish_notice();
                    return Err(cause);
                }
            }
        }
        if quitting && !pump.closing && !pump.io.busy()
        {
            return Ok(());
        }
        if !pump.closing && !pump.io.busy() && !quitting
        {
            let mut reset = false;
            let mut became_ready = false;
            if let Some(engine) = &mut pump.engine
            {
                if engine.phase() == Phase::Ready
                {
                    pump.negotiate_deadline = None;
                    if !pump.confirmed
                    {
                        match pump.startup.confirm()
                        {
                            Ok(()) =>
                            {
                                pump.confirmed = true;
                                became_ready = true;
                            }
                            Err(cause) if cause.0 == SERVICE_BUSY => (),
                            Err(cause) => return Err(cause)
                        }
                    }
                }
                if engine.phase() == Phase::Ready && pump.announced_epoch != pump.epoch
                {
                    pump.announced_epoch = pump.epoch;
                    became_ready = true;
                }
                if engine.pending_fetch().is_none()
                {
                    pump.fetch_deadline = None;
                }
                if let Some((token, deadline)) = pump.fetch_deadline
                   && now >= deadline
                {
                    let _ = engine.expire_fetch(token);
                    reset = true;
                }
                if pump.negotiate_deadline.is_some_and(|deadline| now >= deadline)
                {
                    reset = true;
                }
                if let Some(epoch) = pump.epoch
                   && engine.poll(epoch).is_err()
                   && engine.phase() == Phase::ResetRequired
                {
                    reset = true;
                }
            }
            if became_ready
            {
                pump.notice(0,
                            format_args!("Unicode text sharing active after scheduled vdagent \
                                          negotiation and resident Scrap admission"));
            }
            if reset
            {
                pump.notice(IO_ERROR,
                            format_args!("Clipboard negotiation or transfer timed out; \
                                          resetting the actual channel before correlation reuse"));
                pump.close();
                pump.reopen_after = now.saturating_add(PREFERENCES);
            }
            else if pump.reset_due.is_some_and(|deadline| now >= deadline)
            {
                pump.close();
            }
            else if pump.settings.is_some_and(Settings::enabled) && now >= pump.status_due
            {
                pump.start(Action::Status, &[], now)?;
            }
            else if pump.engine.is_some()
            {
                pump.observe(now);
                let output = pump.engine.as_ref().and_then(Engine::output);
                if let Some(output) = output
                {
                    let mut bytes = [0; channel::MAX_TRANSFER];
                    let mut length = 0;
                    for segment in output.segments
                    {
                        let count = segment.len().min(bytes.len() - length);
                        bytes[length..length + count].copy_from_slice(&segment[..count]);
                        length += count;
                        if length == bytes.len()
                        {
                            break;
                        }
                    }
                    if length == 0
                    {
                        return Err(Error(BAD_ABI));
                    }
                    pump.start(Action::Write(output.token, length), &bytes[..length], now)?;
                }
                else
                {
                    match pump.receive()
                    {
                        Ok(true) => (),
                        Ok(false)
                            if pump.at == pump.end
                               && !pump.decoder_retry
                               && now >= pump.read_due =>
                        {
                            pump.start(Action::Read, &[], now)?;
                        }
                        Ok(false) => (),
                        Err(cause) if cause.0 == SERVICE_BUSY || cause.0 == NO_MEMORY => (),
                        Err(cause) =>
                        {
                            pump.notice(cause.0,
                                        format_args!("Clipboard input refused: {cause:?}; local \
                                                      Scrap retained"));
                            pump.close();
                        }
                    }
                }
            }
        }
        let drain = pump.scrap.drain(8);
        if let Some(cause) = drain.failure
        {
            pump.notice(cause.status(), format_args!("Scrap retirement remains pending: {cause}"));
        }
        if !quitting
        {
            let duration = if pump.io.busy() || pump.frame_pending || pump.at < pump.end
            {
                RETRY
            }
            else if pump.engine.is_none() && !pump.closing
            {
                let mut deadline = pump.preference_due;
                if pump.settings.is_some_and(Settings::enabled)
                {
                    deadline = deadline.min(pump.status_due);
                    if let Some(reset) = pump.reset_due
                    {
                        deadline = deadline.min(reset);
                    }
                }
                deadline.saturating_sub(now).max(1)
            }
            else
            {
                TURN
            };
            if let Err(cause) = context.park_for(core::time::Duration::from_micros(duration))
            {
                if cause.0 == OPERATION_CANCELLED
                {
                    quitting = true;
                }
                else
                {
                    return Err(cause);
                }
            }
        }
    }
}
