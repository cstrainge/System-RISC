//! Positioned, bounded resource inspection over the filesystem class interface.
//! The flight owns all state across waits; caller buffers are used only on delivery.
#![forbid(unsafe_code)]
use mac_abi::resources::{FileResourceCall, ResourceInfo};
use mac_formats::resource::{index::{Index, MapLayout},
                            stream::Budget};

use super::*;
use crate::toolbox::resources::inspect::{Need, format_status, prepare};

const CHUNK: usize = 64 * 1024;

pub(super) struct Response
{
    key:      ResourceKey,
    index:    u32,
    info:     ResourceInfo,
    has_name: u32,
    chunks:   Vec<Vec<u8>>
}
#[derive(Clone, Copy)]
enum Stage
{
    Header,
    Map,
    Length(usize),
    Data
    {
        next:      u64,
        remaining: usize
    }
}
pub(super) struct State
{
    handle:       u32,
    volume:       u32,
    fork:         u32,
    saved_offset: u64,
    length:       u64,
    operation:    u32,
    offset:       usize,
    capacity:     usize,
    index:        Option<Index>,
    layout:       Option<MapLayout>,
    map:          Vec<u8>,
    stage:        Stage,
    expected:     usize,
    response:     Box<Response>
}
impl State
{
    fn query(&mut self, name: &str) -> Result<Option<(u64, usize)>, Status>
    {
        let mut call = FileResourceCall { operation: self.operation,
                                          key: self.response.key,
                                          index: self.response.index,
                                          offset: self.offset,
                                          ..Default::default() };
        let need = prepare(self.index.as_ref().ok_or(BAD_ABI)?, &mut call, name, self.capacity)?;
        match need
        {
            Need::Length { number,
                           offset } =>
            {
                self.stage = Stage::Length(number);
                Ok(Some((offset, 4)))
            }
            Need::Done | Need::Data { .. } =>
            {
                self.response.key = call.key;
                self.response.index = call.index;
                self.response.info = call.info;
                self.response.has_name = call.has_name;
                if let Need::Data { offset,
                                    length } = need
                {
                    self.response
                        .chunks
                        .try_reserve_exact(length.div_ceil(CHUNK))
                        .map_err(|_| NO_MEMORY)?;
                    let count = length.min(CHUNK);
                    self.stage = Stage::Data { next:      offset + count as u64,
                                               remaining: length - count };
                    Ok(Some((offset, count)))
                }
                else
                {
                    Ok(None)
                }
            }
        }
    }

    fn received(&mut self, bytes: Vec<u8>, name: &str) -> Result<Option<(u64, usize)>, Status>
    {
        if bytes.len() != self.expected
        {
            return Err(IO_ERROR);
        }
        match self.stage
        {
            Stage::Header =>
            {
                let header: &[u8; 16] = bytes.as_slice().try_into().map_err(|_| BAD_ABI)?;
                let layout = MapLayout::decode(header, self.length, Budget::default())
                    .map_err(format_status)?;
                self.map.try_reserve_exact(layout.map_length()).map_err(|_| NO_MEMORY)?;
                self.layout = Some(layout);
                self.stage = Stage::Map;
            }
            Stage::Map => self.map.extend_from_slice(&bytes),
            Stage::Length(number) =>
            {
                let bytes: [u8; 4] = bytes.as_slice().try_into().map_err(|_| BAD_ABI)?;
                self.index
                    .as_mut()
                    .ok_or(BAD_ABI)?
                    .record_length(number, u32::from_be_bytes(bytes))
                    .map_err(format_status)?;
                return self.query(name);
            }
            Stage::Data { next,
                          remaining } =>
            {
                self.response.chunks.push(bytes);
                if remaining == 0
                {
                    return Ok(None);
                }
                let count = remaining.min(CHUNK);
                self.stage = Stage::Data { next:      next + count as u64,
                                           remaining: remaining - count };
                return Ok(Some((next, count)));
            }
        }
        let layout = self.layout.ok_or(BAD_ABI)?;
        if self.map.len() < layout.map_length()
        {
            return Ok(Some((layout.map_offset() + self.map.len() as u64,
                            (layout.map_length() - self.map.len()).min(CHUNK))));
        }
        self.index =
            Some(Index::decode(layout, core::mem::take(&mut self.map)).map_err(format_status)?);
        self.query(name)
    }
}
fn read(provider: u32, state: &mut State, offset: u64, count: usize) -> Result<OwnedJob, Status>
{
    if offset.checked_add(count as u64).is_none_or(|end| end > state.length)
    {
        return Err(BAD_ARGUMENT);
    }
    state.expected = count;
    submit_args(provider,
                Operation::Read,
                Arguments { volume: state.volume,
                            token: state.handle,
                            flags: state.fork,
                            offset,
                            length: count as u64,
                            ..Default::default() })
}
fn finish(owner: u64, mut state: Box<State>, result: Result<(), Status>) -> Progress
{
    let released = files(|manager| {
        manager.end_resource_inspection(owner, state.handle, state.saved_offset, state.index.take())
    });
    Progress::Done(released.and(result).map(|()| Reply::Resource(state.response)))
}
pub(super) fn advance(mut flight: Flight, result: Result<Vec<u8>, Status>) -> Progress
{
    let Step::Resource(mut state) = flight.step
    else
    {
        return Progress::Done(Err(BAD_ABI));
    };
    let next = if flight.abandoned
    {
        Err(OPERATION_CANCELLED)
    }
    else
    {
        result.and_then(|bytes| state.received(bytes, &flight.input))
    };
    match next
    {
        Ok(Some((offset, count))) =>
        {
            match read(flight.provider, &mut state, offset, count)
            {
                Ok(job) =>
                {
                    flight.job = job;
                    flight.step = Step::Resource(state);
                    Progress::Pending(flight)
                }
                Err(error) => finish(flight.owner.label(), state, Err(error))
            }
        }
        Ok(None) => finish(flight.owner.label(), state, Ok(())),
        Err(error) => finish(flight.owner.label(), state, Err(error))
    }
}
fn deliver(call: &mut FileResourceCall, output: &mut [u8], progress: Progress) -> Status
{
    match progress
    {
        Progress::Pending(flight) => waiting(flight),
        Progress::Done(Err(error)) => error,
        Progress::Done(Ok(Reply::Resource(response))) =>
        {
            let length = response.chunks.iter().map(Vec::len).sum::<usize>();
            if length > output.len()
            {
                return BAD_ABI;
            }
            let mut at = 0;
            for bytes in &response.chunks
            {
                output[at..at + bytes.len()].copy_from_slice(bytes);
                at += bytes.len();
            }
            call.key = response.key;
            call.index = response.index;
            call.info = response.info;
            call.has_name = response.has_name;
            call.transferred = length;
            OK
        }
        Progress::Done(_) => BAD_ABI
    }
}
pub(crate) fn request(owner: u64, call: &mut FileResourceCall, name: &str, output: &mut [u8])
                      -> Status
{
    let thread = crate::context::current_thread().0;
    if thread <= 1
    {
        return WRONG_THREAD;
    }
    let key = Key { record:    Record::Resource,
                    operation: call.operation,
                    volume:    0,
                    index:     call.index,
                    cnid:      0,
                    handle:    call.handle,
                    fork:      0,
                    capacity:  output.len(),
                    cursor:    DirectoryScanCursor::default(),
                    resource:  Some((call.key, call.offset)),
                    metadata:  None,
                    when:      0 };
    match claim(owner, thread, key, name)
    {
        Ok(Some(flight)) => return deliver(call, output, super::advance(flight)),
        Err(error) => return error,
        Ok(None) => ()
    }
    let prepared = (|| {
        let mut input = String::new();
        input.try_reserve_exact(name.len()).map_err(|_| NO_MEMORY)?;
        input.push_str(name);
        let response = Box::try_new(Response { key:      call.key,
                                               index:    call.index,
                                               info:     ResourceInfo::default(),
                                               has_name: 0,
                                               chunks:   Vec::new() }).map_err(|_| NO_MEMORY)?;
        let mut state = Box::try_new(State { handle: call.handle,
                                             volume: 0,
                                             fork: 0,
                                             saved_offset: 0,
                                             length: 0,
                                             operation: call.operation,
                                             offset: call.offset,
                                             capacity: output.len(),
                                             index: None,
                                             layout: None,
                                             map: Vec::new(),
                                             stage: Stage::Header,
                                             expected: 0,
                                             response }).map_err(|_| NO_MEMORY)?;
        let inspection = files(|manager| manager.begin_resource_inspection(owner, call.handle))?;
        state.volume = inspection.volume;
        state.fork = inspection.fork;
        state.saved_offset = inspection.offset;
        state.length = inspection.length;
        state.index = inspection.index;
        Ok::<_, Status>((inspection.provider, state, input))
    })();
    let (provider, mut state, input) = match prepared
    {
        Ok(p) => p,
        Err(e) => return e
    };
    let next = if state.index.is_some()
    {
        state.query(name)
    }
    else if state.length == 0
    {
        state.index = Some(Index::empty());
        state.query(name)
    }
    else
    {
        Ok(Some((0, 16)))
    };
    let job = match next
    {
        Ok(Some((offset, count))) => read(provider, &mut state, offset, count),
        Ok(None) => return deliver(call, output, finish(owner, state, Ok(()))),
        Err(error) => return deliver(call, output, finish(owner, state, Err(error)))
    };
    match job
    {
        Ok(job) =>
        {
            waiting(Flight { thread,
                             owner: owner.into(),
                             key,
                             provider,
                             step: Step::Resource(state),
                             job,
                             abandoned: false,
                             input,
                             path: None })
        }
        Err(error) => deliver(call, output, finish(owner, state, Err(error)))
    }
}
