//! Exercise the actual clipboard policy Engine against a private X11 peer.
//! The immutable pins below belong only to this fixture; they are not resident
//! Scrap pins/grants and do not qualify the production integration or worker.
use mac_abi::{BAD_ABI, IO_ERROR};
use mac_byte_channel::{self as wire, ByteChannel};
use mac_driver::{Error, Result};
use mac_host_clipboard::{Config, Content, Effect, Engine, Phase, Snapshot};
use mac_vdagent_protocol::{Decoder, Format, Limits, Message, Selection};

use crate::{call, codec, log, machine};
const HOST: &str = "Private host café 🦀\nsecond line";
const GUEST: &str = "Private guest 雪 🦀\nsecond line";
struct Pin
{
    revision: u64,
    text:     Option<&'static str>
}
impl Snapshot for Pin
{
    fn revision(&self) -> u64
    {
        self.revision
    }

    fn content(&self) -> Content<'_>
    {
        self.text.map_or(Content::Empty, Content::Text)
    }
}
fn checked<T>(result: core::result::Result<T, mac_host_clipboard::Error>) -> Result<T>
{
    result.map_err(|cause| {
              log(format_args!("VDAGENT_NATIVE_ENGINE_ERROR detail={}\n", cause));
              Error(BAD_ABI)
          })
}
fn drain(channel: &ByteChannel, engine: &mut Engine<Pin>, deadline: u64) -> Result<()>
{
    while let Some(view) = engine.output()
    {
        let token = view.token;
        let Some(segment) = view.segments.into_iter().find(|segment| !segment.is_empty())
        else
        {
            return Err(Error(BAD_ABI));
        };
        let mut bytes = [0; 4096];
        let length = segment.len();
        if length > bytes.len()
        {
            return Err(Error(BAD_ABI));
        }
        bytes[..length].copy_from_slice(segment);
        let mut reply = [0; 4];
        let received = call(channel, wire::WRITE, &bytes[..length], &mut reply, deadline)?;
        let accepted = wire::accepted(&reply[..received], length)?;
        checked(engine.accepted(token, accepted))?;
        if accepted == 0
        {
            machine::pause();
        }
    }
    Ok(())
}
pub fn run(channel: &ByteChannel, deadline: u64, generation: u64) -> Result<()>
{
    let limits = codec(Limits::new(4096))?;
    let config = checked(Config::new(limits, 1024))?;
    let mut engine = Engine::new(config);
    let epoch = checked(engine.open_after_transport_reset(Pin { revision: 1,
                                                                text:     None }))?;
    let mut decoder = Decoder::new(limits);
    let mut bytes = [0; 4096];
    let mut negotiated = false;
    let mut offer_deadline = None;
    let mut host_received = false;
    let mut guest_requested = false;
    log(format_args!("VDAGENT_NATIVE_POLICY engine=mac-host-clipboard \
                      snapshots=private-fixture-only resident_grants=false\n"));
    loop
    {
        checked(engine.poll(epoch))?;
        drain(channel, &mut engine, deadline)?;
        if engine.phase() == Phase::Ready && !negotiated
        {
            negotiated = true;
            offer_deadline = machine::now().checked_add(3_000_000);
            log(format_args!("VDAGENT_NATIVE_CAPABILITIES_PASS generation={} protocol=1 \
                              message=6 negotiated={} engine=mac-host-clipboard \
                              scope=bootstrap-transport-only\n",
                             generation,
                             engine.capabilities().bits()));
        }
        if guest_requested && engine.queued_messages() == 0
        {
            log(format_args!("VDAGENT_NATIVE_PRIVATE_GUEST_TEXT_SENT bytes={} \
                              engine=mac-host-clipboard scope=fresh-private-X11\n",
                             GUEST.len()));
            return Ok(());
        }
        if !host_received && offer_deadline.is_some_and(|limit| machine::now() >= limit)
        {
            log(format_args!("VDAGENT_NATIVE_PRIVATE_HOST_PEER_UNAVAILABLE detail=no private \
                              X11 offer arrived after negotiation; enable a working QEMU GTK \
                              clipboard peer and retry; guest clipboard remains local\n"));
            return Ok(());
        }
        let length = call(channel, wire::READ, &[], &mut bytes, deadline)?;
        let mut consumed = 0;
        while consumed < length
        {
            let feed=decoder.feed(&bytes[consumed..length]).map_err(|error| {
                log(format_args!("VDAGENT_NATIVE_CODEC_ERROR detail={}\n",error.cause));
                Error(BAD_ABI)
            })?;
            if feed.consumed == 0
            {
                return Err(Error(BAD_ABI));
            }
            consumed += feed.consumed;
            let Some(frame) = feed.frame
            else
            {
                continue;
            };
            if let Message::Request { selection: Selection::Clipboard,
                                      format: Format::UTF8_TEXT } =
                codec(frame.message(engine.capabilities()))?
            {
                guest_requested = host_received;
            }
            match checked(engine.receive(epoch, &frame))?
            {
                Effect::Import(import) =>
                {
                    if import.text() != HOST
                    {
                        log(format_args!("VDAGENT_NATIVE_PRIVATE_TEXT_FAIL detail=isolated host \
                                          literal differs; received text is intentionally not \
                                          logged\n"));
                        return Err(Error(BAD_ABI));
                    }
                    engine.complete_import::<()>(import,
                                                 Ok(Pin { revision: 2,
                                                          text:     Some(HOST) }))
                          .map_err(|cause| {
                              log(format_args!("VDAGENT_NATIVE_ENGINE_IMPORT_ERROR detail={:?}\n",
                                               cause));
                              Error(BAD_ABI)
                          })?;
                    host_received = true;
                    log(format_args!("VDAGENT_NATIVE_PRIVATE_HOST_TO_GUEST_PASS bytes={} \
                                      utf8=passed multiline=passed emoji=passed \
                                      engine=mac-host-clipboard\n",
                                     HOST.len()));
                    checked(engine.observe(Pin { revision: 3,
                                                 text:     Some(GUEST) }))?;
                }
                Effect::Unavailable => return Err(Error(IO_ERROR)),
                _ => ()
            }
            // Preserve every coalesced frame: drain required protocol effects
            // before feeding the remainder of this same receive buffer.
            checked(engine.poll(epoch))?;
            drain(channel, &mut engine, deadline)?;
        }
        machine::pause();
    }
}
