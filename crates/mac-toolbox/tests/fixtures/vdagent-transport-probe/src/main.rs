//! Genuine System RV64/0 INIT used only by private native acceptance. This
//! bounded bootstrap probe is deliberately not a production background worker.
#![no_std]
#![no_main]
#![feature(alloc_error_handler)]
#![deny(unsafe_code)]
mod machine;
#[cfg(feature = "private-text")]
mod policy_probe;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use mac_abi::{BAD_ABI, IO_ERROR, NOT_FOUND, NOT_SUPPORTED, SERVICE_BUSY};
use mac_byte_channel::{self as wire, ByteChannel, State, Status};
use mac_driver::{Client, Completion, Driver, Error, Phase, Request, Result};
#[cfg(not(feature = "private-text"))]
use mac_vdagent_protocol::{Capabilities, Capability, Decoder, Limits, Message, OwnedMessage};
static RETAINED_TICKET: AtomicU64 = AtomicU64::new(0);
static RETAINED_OPERATION: AtomicU32 = AtomicU32::new(0);
struct Probe;
mac_driver::export_driver!(Probe);
mac_driver::driver_panic_handler!();
#[alloc_error_handler]
fn allocation_failed(layout: core::alloc::Layout) -> !
{
    panic!("private probe fallible-allocation invariant failed: {layout:?}")
}
fn log(arguments: core::fmt::Arguments<'_>)
{
    machine::log(arguments);
}
fn codec<T>(result: mac_vdagent_protocol::Result<T>) -> Result<T>
{
    result.map_err(|cause| {
              log(format_args!("VDAGENT_NATIVE_CODEC_ERROR detail={cause}\n"));
              Error(BAD_ABI)
          })
}
fn cancel(channel: &ByteChannel, operation: u32, ticket: &mut u64) -> Result<()>
{
    let began = machine::now();
    while *ticket != 0 && machine::now().saturating_sub(began) < 5_000_000
    {
        match channel.invoke(ticket, Phase::Cancel, operation, &[], &mut [])
        {
            Ok(Completion::Complete(_)) if *ticket == 0 => return Ok(()),
            Err(_) if *ticket == 0 => return Ok(()),
            _ => machine::pause()
        }
    }
    if *ticket == 0
    {
        return Ok(());
    }
    RETAINED_OPERATION.store(operation, Ordering::Release);
    RETAINED_TICKET.store(*ticket, Ordering::Release);
    log(format_args!("VDAGENT_NATIVE_CANCEL_RETAINED ticket={} operation={} detail=reset or \
                      owner cleanup must acknowledge this live request\n",
                     *ticket, operation));
    Err(Error(IO_ERROR))
}
fn call(channel: &ByteChannel, operation: u32, input: &[u8], output: &mut [u8], deadline: u64)
        -> Result<usize>
{
    let mut ticket = 0;
    loop
    {
        if machine::now() >= deadline
        {
            log(format_args!("VDAGENT_NATIVE_TIMEOUT operation={} detail=the bounded service or \
                              peer progress deadline expired\n",
                             operation));
            cancel(channel, operation, &mut ticket)?;
            return Err(Error(IO_ERROR));
        }
        let phase = if ticket == 0 { Phase::Start } else { Phase::Poll };
        match channel.invoke(&mut ticket, phase, operation, input, output)
        {
            Ok(Completion::Complete(length)) if ticket == 0 => return Ok(length),
            Ok(Completion::Pending) if ticket != 0 => (),
            Err(Error(SERVICE_BUSY)) => (),
            Err(error) =>
            {
                cancel(channel, operation, &mut ticket)?;
                if operation != wire::ERROR_TEXT
                {
                    let mut text = [0; wire::MAX_ERROR_TEXT];
                    if let Ok(length) = call(channel, wire::ERROR_TEXT, &[], &mut text, deadline)
                    {
                        let detail =
                            core::str::from_utf8(&text[..length]).unwrap_or("invalid UTF-8 \
                                                                             diagnostic");
                        log(format_args!("VDAGENT_NATIVE_CALL_FAILED operation={} status={} \
                                          detail={}\n",
                                         operation, error.0, detail));
                    }
                }
                return Err(error);
            }
            _ =>
            {
                cancel(channel, operation, &mut ticket)?;
                return Err(Error(BAD_ABI));
            }
        }
        machine::pause();
    }
}
#[cfg(not(feature = "private-text"))]
fn send(channel: &ByteChannel, message: &OwnedMessage, deadline: u64) -> Result<()>
{
    let chunks = codec(message.chunks(1024))?;
    for chunk in chunks
    {
        for bytes in chunk.segments()
        {
            let mut sent = 0;
            while sent < bytes.len()
            {
                let mut output = [0; 4];
                let length = call(channel, wire::WRITE, &bytes[sent..], &mut output, deadline)?;
                sent += wire::accepted(&output[..length], bytes.len() - sent)?;
                if sent < bytes.len()
                {
                    machine::pause();
                }
            }
        }
    }
    Ok(())
}
fn run(host: Client) -> Result<()>
{
    let channel = match ByteChannel::resolve(host)
    {
        Ok(channel) => channel,
        Err(Error(NOT_FOUND | NOT_SUPPORTED)) =>
        {
            log(format_args!("VDAGENT_NATIVE_ABSENT detail=optional VBYT service was not \
                              advertised; guest clipboard stays local\n"));
            return Ok(());
        }
        Err(error) => return Err(error)
    };
    let deadline = machine::now().checked_add(15_000_000).ok_or(Error(IO_ERROR))?;
    let generation = loop
    {
        let mut output = [0; wire::STATUS_BYTES];
        let length = call(&channel, wire::STATUS, &[], &mut output, deadline)?;
        let status = Status::decode(&output[..length])?;
        if status.state == State::Open
        {
            break status.generation;
        }
        if matches!(status.state, State::Unavailable | State::Failed)
        {
            let mut text = [0; wire::MAX_ERROR_TEXT];
            let length = call(&channel, wire::ERROR_TEXT, &[], &mut text, deadline)?;
            let text = core::str::from_utf8(&text[..length]).map_err(|_| Error(BAD_ABI))?;
            log(format_args!("VDAGENT_NATIVE_REFUSAL state={} status={} detail={}\n",
                             status.state as u32, status.last_status, text));
            return Ok(());
        }
        machine::pause();
    };
    log(format_args!("VDAGENT_NATIVE_OPEN generation={} port=com.redhat.spice.0\n", generation));
    #[cfg(feature = "private-text")]
    return policy_probe::run(&channel, deadline, generation);
    #[cfg(not(feature = "private-text"))]
    {
        let limits = codec(Limits::new(4096))?;
        let local = Capabilities::empty().with(Capability::ClipboardByDemand)
                                         .with(Capability::ClipboardSelection)
                                         .with(Capability::ClipboardGrabSerial)
                                         .with(Capability::MaxClipboard)
                                         .with(Capability::GuestLineEndLf);
        let announce = codec(OwnedMessage::announce(local, true, limits))?;
        send(&channel, &announce, deadline)?;
        log(format_args!("VDAGENT_NATIVE_ANNOUNCE protocol=1 message=6 payload=8 \
                          request_reply=1 bits={}\n",
                         local.bits()));
        let mut decoder = Decoder::new(limits);
        let mut bytes = [0; 4096];
        loop
        {
            let length = call(&channel, wire::READ, &[], &mut bytes, deadline)?;
            let mut consumed = 0;
            while consumed < length
            {
                let feed = match decoder.feed(&bytes[consumed..length])
                {
                    Ok(feed) => feed,
                    Err(error) =>
                    {
                        log(format_args!("VDAGENT_NATIVE_MALFORMED detail={} consumed={} \
                                          poisoned={}\n",
                                         error.cause,
                                         error.consumed,
                                         decoder.is_poisoned()));
                        return Ok(());
                    }
                };
                if feed.consumed == 0
                {
                    return Err(Error(BAD_ABI));
                }
                consumed += feed.consumed;
                if let Some(frame) = feed.frame
                {
                    if let Message::Capabilities(peer) = codec(frame.message(local))?
                    {
                        if !peer.supports(Capability::ClipboardByDemand)
                        {
                            log(format_args!("VDAGENT_NATIVE_CAPABILITY_REFUSAL detail=host \
                                              omitted the required ClipboardByDemand \
                                              capability\n"));
                            return Err(Error(NOT_SUPPORTED));
                        }
                        let negotiated = peer.negotiate(local);
                        if peer.requests_reply()
                        {
                            // The one outstanding announce already supplies our
                            // caps. Wait for request=0 to prove QEMU processed it;
                            // redundant caps reset QEMU's clipboard serial state
                            // and deliberately close its virtserial connection.
                            log(format_args!("VDAGENT_NATIVE_CROSSED_CAPS request=1 \
                                              local_caps_already_sent=true\n"));
                            continue;
                        }
                        log(format_args!("VDAGENT_NATIVE_CAPABILITIES_PASS generation={} \
                                          protocol={} message={} words={} negotiated={} \
                                          scope=bootstrap-transport-only\n",
                                         generation,
                                         mac_vdagent_protocol::PROTOCOL,
                                         frame.header().kind().raw(),
                                         peer.word_count(),
                                         negotiated.bits()));
                        return Ok(());
                    }
                }
            }
            machine::pause();
        }
    }
}

impl Driver for Probe
{
    fn initialize(&self, host: Client) -> Result<()>
    {
        if !machine::initialize()
        {
            return Err(Error(SERVICE_BUSY));
        }
        log(format_args!("VDAGENT_NATIVE_BEGIN target=System resource=0 \
                          execution=bounded-bootstrap fixture=true\n"));
        let result = run(host);
        if let Err(Error(status)) = result
        {
            log(format_args!("VDAGENT_NATIVE_FAIL status={} detail=bounded native transport \
                              probe failed; inspect preceding protocol and service diagnostics\n",
                             status));
        }
        result
    }

    fn request(&self, _: Request<'_>, _: &mut [u8]) -> Result<Completion>
    {
        Err(Error(NOT_SUPPORTED))
    }
}
