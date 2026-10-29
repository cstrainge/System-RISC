//! One genuine broker ticket and its immutable Start input at a time. Every
//! work turn makes one bounded callback; cancellation retains the same ticket.
use mac_abi::{BAD_ABI, BAD_ARGUMENT, SERVICE_BUSY};
use mac_byte_channel::{self as wire, ByteChannel};
use mac_driver::{Completion, Error, Phase, Result};
use mac_host_clipboard::OutputToken;

#[derive(Clone, Copy, Debug)]
pub enum Action
{
    Status,
    Read,
    Write(OutputToken, usize),
    Reset,
    Diagnostic
}
impl Action
{
    fn operation(self) -> u32
    {
        match self
        {
            Self::Status => wire::STATUS,
            Self::Read => wire::READ,
            Self::Write(..) => wire::WRITE,
            Self::Reset => wire::RESET,
            Self::Diagnostic => wire::ERROR_TEXT
        }
    }
}
pub enum Finished
{
    Success(Action, usize),
    Failed(Action, Error),
    Cancelled
}
pub struct Io
{
    channel:    ByteChannel,
    ticket:     u64,
    action:     Option<Action>,
    input:      [u8; wire::MAX_TRANSFER],
    length:     usize,
    output:     [u8; wire::MAX_TRANSFER],
    cancelling: bool,
    failure:    Option<Error>
}
impl Io
{
    pub fn new(channel: ByteChannel) -> Self
    {
        Self { channel,
               ticket: 0,
               action: None,
               input: [0; wire::MAX_TRANSFER],
               length: 0,
               output: [0; wire::MAX_TRANSFER],
               cancelling: false,
               failure: None }
    }

    pub fn busy(&self) -> bool
    {
        self.action.is_some()
    }

    pub fn resetting(&self) -> bool
    {
        matches!(self.action, Some(Action::Reset))
    }

    pub fn ticket(&self) -> u64
    {
        self.ticket
    }

    pub fn output(&self, length: usize) -> &[u8]
    {
        &self.output[..length]
    }

    pub fn start(&mut self, action: Action, input: &[u8]) -> Result<()>
    {
        if self.busy()
        {
            return Err(Error(SERVICE_BUSY));
        }
        if input.len() > self.input.len()
        {
            return Err(Error(BAD_ARGUMENT));
        }
        self.input[..input.len()].copy_from_slice(input);
        self.length = input.len();
        self.action = Some(action);
        self.cancelling = false;
        self.failure = None;
        Ok(())
    }

    pub fn step(&mut self, cancel: bool) -> Option<Finished>
    {
        let action = self.action?;
        self.cancelling |= cancel;
        if self.cancelling && self.ticket == 0
        {
            self.action = None;
            return Some(Finished::Cancelled);
        }
        let phase = if self.cancelling
        {
            Phase::Cancel
        }
        else if self.ticket == 0
        {
            Phase::Start
        }
        else
        {
            Phase::Poll
        };
        let capacity = match action
        {
            Action::Status => wire::STATUS_BYTES,
            Action::Write(..) => 4,
            Action::Read => wire::MAX_TRANSFER,
            Action::Reset => 0,
            Action::Diagnostic => wire::MAX_ERROR_TEXT
        };
        let result = self.channel.invoke(&mut self.ticket,
                                         phase,
                                         action.operation(),
                                         &self.input[..self.length],
                                         &mut self.output[..capacity]);
        match result
        {
            Ok(Completion::Pending) => None,
            Ok(Completion::Complete(length)) if self.ticket == 0 =>
            {
                self.action = None;
                Some(if self.cancelling
                     {
                         self.failure
                             .map_or(Finished::Cancelled, |cause| Finished::Failed(action, cause))
                     }
                     else
                     {
                         Finished::Success(action, length)
                     })
            }
            Err(error) if error.0 == SERVICE_BUSY => None,
            Err(error) if self.ticket != 0 =>
            {
                self.failure.get_or_insert(error);
                self.cancelling = true;
                None
            }
            Err(error) =>
            {
                self.action = None;
                Some(Finished::Failed(action, error))
            }
            Ok(Completion::Complete(_)) =>
            {
                // An impossible retained terminal ticket must be cancelled,
                // never dropped or reused for an unrelated operation.
                self.cancelling = true;
                self.failure.get_or_insert(Error(BAD_ABI));
                None
            }
        }
    }
}
