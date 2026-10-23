//! Whole-batch transfers are resident-owned; applications service ordinary
//! events between bounded steps. Drop requests cancellation and owned cleanup.
pub use mac_traps::types::{FileTransferId, TransferSource};

use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransferMode
{
    Copy,
    Move
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransferDestination
{
    Folder
    {
        volume: VolumeId, parent: Cnid
    },
    /// Each source uses its own volume's Trash. This always means move,
    /// independently of keyboard modifiers or the supplied transfer mode.
    Trash
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TransferSpec
{
    pub mode:        TransferMode,
    pub destination: TransferDestination
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransferPhase
{
    Preparing,
    Copying,
    Reading,
    Writing,
    Verifying,
    Committing,
    RollingBack,
    CleaningUp,
    Committed,
    Cancelled
}
#[derive(Clone, Copy, Debug)]
pub struct TransferProgress
{
    pub phase:           TransferPhase,
    pub completed_items: u32,
    pub total_items:     u32,
    pub completed_bytes: u64,
    pub total_bytes:     u64,
    pub current:         Option<CatalogInfo>,
    pub recovery:        Option<TransferRecovery>
}
impl TransferProgress
{
    pub const fn finished(&self) -> bool
    {
        matches!(self.phase, TransferPhase::Committed | TransferPhase::Cancelled)
    }
}
/// A recoverable cleanup failure. The owner/leases remain resident and retry;
/// the committed flag distinguishes cleanup from precommit rollback.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TransferRecovery
{
    pub original:  Option<Error>,
    pub error:     Error,
    pub committed: bool,
    pub current:   Option<CatalogInfo>
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TransferFailure
{
    pub cause:   Error,
    pub cleanup: Option<Error>,
    pub current: Option<CatalogInfo>
}
impl core::fmt::Display for TransferFailure
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        if let Some(item) = &self.current
        {
            write!(f, "Could not transfer \"{}\": {}", item.name(), self.cause)?;
        }
        else
        {
            write!(f, "Could not complete file transfer: {}", self.cause)?;
        }
        if let Some(error) = self.cleanup
        {
            write!(f, "; cleanup failed: {error}")?;
        }
        Ok(())
    }
}
impl core::error::Error for TransferFailure
{
}
#[must_use = "keep the owner and service events until the transfer finishes; Drop requests \
              cancellation"]
pub struct FileTransfer<'a>
{
    client: Client<'a>,
    id:     FileTransferId
}
impl<'a> Files<'a>
{
    pub fn begin_transfer(self, spec: TransferSpec, sources: &[TransferSource])
                          -> Result<FileTransfer<'a>>
    {
        let (mode, volume, parent) = match spec.destination
        {
            TransferDestination::Trash => (TRANSFER_TRASH, 0, 0),
            TransferDestination::Folder { volume,
                                          parent } =>
            {
                (match spec.mode
                 {
                     TransferMode::Copy => TRANSFER_COPY,
                     TransferMode::Move => TRANSFER_MOVE
                 },
                 volume.0,
                 parent.0)
            }
        };
        let mut call = FileTransferCall { operation: BEGIN_TRANSFER,
                                          mode,
                                          destination_volume: volume,
                                          destination_parent: parent,
                                          when: self.client
                                                    .now()?
                                                    .mac_microseconds()
                                                    .ok_or(Error::BadArgument)?,
                                          ..Default::default() };
        self.client.file_transfer(&mut call, sources)?;
        if call.handle == 0
        {
            return Err(Error::BadAbi);
        }
        Ok(FileTransfer { client: self.client,
                          id:     FileTransferId(call.handle) })
    }
}
impl FileTransfer<'_>
{
    pub const fn is_finished(&self) -> bool
    {
        self.id.0 == 0
    }

    pub const fn id(&self) -> FileTransferId
    {
        self.id
    }

    /// Advance at most one bounded storage step. Pending I/O does not park the
    /// application's OS thread; keep handling normal events between calls.
    pub fn step(&mut self) -> core::result::Result<TransferProgress, TransferFailure>
    {
        self.advance(STEP_TRANSFER)
    }

    /// Cancellation remains owned until rollback acknowledges, or reports the
    /// actual committed outcome if the irreversible barrier already won.
    pub fn cancel_step(&mut self) -> core::result::Result<TransferProgress, TransferFailure>
    {
        self.advance(CANCEL_TRANSFER)
    }

    fn advance(&mut self, operation: u32)
               -> core::result::Result<TransferProgress, TransferFailure>
    {
        let failure = |cause| {
            TransferFailure { cause,
                              cleanup: None,
                              current: None }
        };
        if self.id.0 == 0
        {
            return Err(failure(Error::NotFound));
        }
        let mut call = FileTransferCall { operation,
                                          handle: self.id.0,
                                          ..Default::default() };
        self.client.file_transfer(&mut call, &[]).map_err(failure)?;
        if call.commit_decided > 1
           || call.error > 0
           || call.cleanup_error > 0
           || call.phase == TRANSFER_COMMITTED
              && (call.commit_decided != 1 || call.error != 0 || call.cleanup_error != 0)
           || call.phase == TRANSFER_CANCELLED && call.commit_decided != 0
           || call.phase == TRANSFER_FAILED && call.commit_decided != 0
        {
            return Err(failure(Error::BadAbi));
        }
        let current = if call.current.id == 0
        {
            None
        }
        else
        {
            Some(call.current.try_into().map_err(failure)?)
        };
        if call.phase == TRANSFER_FAILED
        {
            if call.error >= 0 || call.cleanup_error > 0
            {
                return Err(failure(Error::BadAbi));
            }
            self.id = FileTransferId(0);
            return Err(TransferFailure { cause: Error::from_status(call.error),
                                         cleanup: if call.cleanup_error < 0
                                         {
                                             Some(Error::from_status(call.cleanup_error))
                                         }
                                         else
                                         {
                                             None
                                         },
                                         current });
        }
        let phase = match call.phase
        {
            TRANSFER_PREPARING => TransferPhase::Preparing,
            TRANSFER_COPYING => TransferPhase::Copying,
            TRANSFER_READING => TransferPhase::Reading,
            TRANSFER_WRITING => TransferPhase::Writing,
            TRANSFER_VERIFYING => TransferPhase::Verifying,
            TRANSFER_COMMITTING => TransferPhase::Committing,
            TRANSFER_ROLLING_BACK => TransferPhase::RollingBack,
            TRANSFER_CLEANING_UP => TransferPhase::CleaningUp,
            TRANSFER_COMMITTED => TransferPhase::Committed,
            TRANSFER_CANCELLED => TransferPhase::Cancelled,
            _ => return Err(failure(Error::BadAbi))
        };
        if call.completed_items > call.total_items || call.completed_bytes > call.total_bytes
        {
            return Err(failure(Error::BadAbi));
        }
        let recovery = if call.cleanup_error < 0
        {
            Some(TransferRecovery { original: (call.error < 0).then(|| {
                                                                  Error::from_status(call.error)
                                                              }),
                                    error: Error::from_status(call.cleanup_error),
                                    committed: call.commit_decided != 0,
                                    current })
        }
        else
        {
            None
        };
        let progress = TransferProgress { phase,
                                          recovery,
                                          current,
                                          completed_items: call.completed_items,
                                          total_items: call.total_items,
                                          completed_bytes: call.completed_bytes,
                                          total_bytes: call.total_bytes };
        if progress.finished()
        {
            self.id = FileTransferId(0);
        }
        Ok(progress)
    }
}
impl Drop for FileTransfer<'_>
{
    fn drop(&mut self)
    {
        if self.id.0 != 0
        {
            let mut call = FileTransferCall { operation: ABANDON_TRANSFER,
                                              handle: self.id.0,
                                              ..Default::default() };
            let _ = self.client.file_transfer(&mut call, &[]);
            self.id = FileTransferId(0);
        }
    }
}
