use core::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error
{
    Configuration,
    Closed,
    ResetRequired,
    StaleEpoch,
    Exhausted,
    HandshakeRequired,
    UnsupportedPeer,
    CapabilitiesChanged,
    QueueFull,
    ResponsePending,
    ImportPending,
    StaleCompletion,
    InvalidCompletion,
    InvalidProgress,
    RevisionOrder,
    UnexpectedData,
    Timeout,
    EmbeddedNul
    {
        offset: usize
    },
    TextLimit
    {
        bytes:   usize,
        maximum: usize
    },
    UnsupportedFormat
    {
        format: u32
    },
    Protocol(mac_vdagent_protocol::Error),
    Output(mac_vdagent_protocol::Error)
}
impl Error
{
    /// These receive refusals retain the caller's frame for retry after output
    /// drains or an earlier import completes. Other outcomes consume the frame
    /// or require reset; never replay them as a new clipboard operation.
    pub fn retry_frame(self) -> bool
    {
        matches!(self,
                 Self::QueueFull
                 | Self::ResponsePending
                 | Self::ImportPending
                 | Self::Output(mac_vdagent_protocol::Error::NoMemory { .. }))
    }
}
impl fmt::Display for Error
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        f.write_str("Host Clipboard ")?;
        match self
        {
            Self::Configuration => f.write_str("has invalid local framing or text limits")?,
            Self::Closed => f.write_str("channel is closed; retain local Scrap and reconnect")?,
            Self::ResetRequired => f.write_str("requires a channel reset before another transfer")?,
            Self::StaleEpoch => f.write_str("refused work from a previous channel session")?,
            Self::Exhausted =>
            {
                f.write_str("exhausted its nonreusable labels; retain local Scrap and restart \
                             System")?
            }
            Self::HandshakeRequired =>
            {
                f.write_str("requires the host capability reply and accepted guest advertisement \
                             before clipboard traffic")?
            }
            Self::UnsupportedPeer =>
            {
                f.write_str("host does not support by-demand text; keep using local Scrap")?
            }
            Self::CapabilitiesChanged =>
            {
                f.write_str("host changed live message layouts; reset and negotiate again")?
            }
            Self::QueueFull =>
            {
                f.write_str("output queue is full; drain accepted bytes and retry pending work")?
            }
            Self::ResponsePending =>
            {
                f.write_str("already has a queued data response; drain it and retry the retained \
                             request")?
            }
            Self::ImportPending =>
            {
                f.write_str("is awaiting atomic Scrap import completion; finish it before \
                             reading another frame")?
            }
            Self::StaleCompletion =>
            {
                f.write_str("discarded a stale transfer completion; retain the newer Scrap")?
            }
            Self::InvalidCompletion =>
            {
                f.write_str("import did not report a newer committed Scrap revision; preserve \
                             the current Scrap")?
            }
            Self::InvalidProgress =>
            {
                f.write_str("refused stale or excessive output acknowledgement; retain the \
                             queued owner and retry its current slice")?
            }
            Self::RevisionOrder =>
            {
                f.write_str("received a decreasing Scrap revision; retain the current snapshot")?
            }
            Self::UnexpectedData =>
            {
                f.write_str("received unrequested unlabelled text; retain Scrap and reset the \
                             channel")?
            }
            Self::Timeout =>
            {
                f.write_str("fetch timed out with an unlabelled response outstanding; retain \
                             Scrap and reset the channel before retrying")?
            }
            Self::EmbeddedNul { offset } =>
            {
                write!(f,
                       "text contains an embedded NUL at byte {offset}; retain Scrap and copy \
                        text without NUL because native clipboard consumers may truncate it")?
            }
            Self::TextLimit { bytes,
                              maximum } =>
            {
                write!(f,
                       "text needs {bytes} bytes but the local bound is {maximum}; retain Scrap \
                        and copy a smaller selection")?
            }
            Self::UnsupportedFormat { format } =>
            {
                write!(f,
                       "received format {format} instead of requested UTF-8 text; retain the \
                        current Scrap")?
            }
            Self::Protocol(cause) => return cause.fmt(f),
            Self::Output(mac_vdagent_protocol::Error::NoMemory { bytes }) =>
            {
                return write!(f,
                              "could not reserve {bytes} queued output bytes; retain the \
                               snapshot or receive frame, close unused applications and retry");
            }
            Self::Output(cause) =>
            {
                return write!(f,
                              "could not prepare queued output; retain Scrap and reset a failed \
                               channel before retrying: {cause}");
            }
        }
        Ok(())
    }
}
impl core::error::Error for Error
{
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)>
    {
        match self
        {
            Self::Protocol(cause) | Self::Output(cause) => Some(cause),
            _ => None
        }
    }
}
impl From<mac_vdagent_protocol::Error> for Error
{
    fn from(cause: mac_vdagent_protocol::Error) -> Self
    {
        Self::Output(cause)
    }
}
#[derive(Debug)]
pub enum CompletionError<E>
{
    Engine(Error),
    Import(E)
}
impl<E: fmt::Display> fmt::Display for CompletionError<E>
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::Engine(cause) => cause.fmt(f),
            Self::Import(cause) =>
            {
                write!(f,
                       "Host Clipboard could not commit Scrap; retain its current content and \
                        retry: {cause}")
            }
        }
    }
}
impl<E: core::error::Error + 'static> core::error::Error for CompletionError<E>
{
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)>
    {
        match self
        {
            Self::Engine(cause) => Some(cause),
            Self::Import(cause) => Some(cause)
        }
    }
}
