//! Affine, authenticated RETURN storage for fixed prepaid callback work.
//! This is not a completion observer or author-execution authority.
use super::*;
use crate::machine::paging::user::ReaderStorage;

pub(super) enum ReturnStorage
{
    Ordinary,
    Prepaid(Option<Stock>)
}
pub(super) struct Stock
{
    bytes:   OwnedBytes,
    readers: ReaderStorage
}
impl ReturnStorage
{
    pub(super) fn prepare(bytes: usize) -> Result<Self, Status>
    {
        Ok(Self::Prepaid(Some(Stock { bytes:   OwnedBytes::prepare_callback(bytes)?,
                                      readers: ReaderStorage::prepare(bytes)? })))
    }
}
/// Only checkout of the actual inflight Job creates a loan. The immutable Job
/// identity, same bytes allocation and same reader metadata survive refusal.
pub(crate) struct ReturnLoan
{
    job:     Arc<Job>,
    storage: OwnedBytes,
    readers: Option<ReaderStorage>,
    active:  bool
}
impl ReturnLoan
{
    pub(crate) fn storage(&self) -> &OwnedBytes
    {
        &self.storage
    }

    pub(crate) fn storage_mut(&mut self) -> &mut OwnedBytes
    {
        &mut self.storage
    }

    pub(crate) fn take_reader_storage(&mut self) -> Result<ReaderStorage, Status>
    {
        self.readers.take().ok_or(SERVICE_BUSY)
    }

    pub(crate) fn restore_reader_storage(&mut self, storage: ReaderStorage)
                                         -> Result<(), (Status, ReaderStorage)>
    {
        if !self.active || self.readers.is_some() || !storage.is_empty()
        {
            return Err((BAD_ARGUMENT, storage));
        }
        self.readers = Some(storage);
        Ok(())
    }
}
pub(crate) fn checkout_prepaid_return(queue: &Arc<Queue>, execution: &CallbackExecution,
                                      bytes: usize)
                                      -> Result<Option<ReturnLoan>, Status>
{
    authentic(queue, execution)?;
    InvocationGate::run(|| {
        let state = queue.state.lock();
        let job = state.running.as_ref().ok_or(BAD_ARGUMENT)?;
        let mut cell = job.state.lock();
        if state.stopped || cell.phase != Phase::Running
        {
            return Err(SERVICE_BUSY);
        }
        if job.identity.owner != execution.owner().owner()
           || job.identity.image_id != execution.image_id()
        {
            return Err(ACCESS_DENIED);
        }
        if matches!(cell.return_storage, ReturnStorage::Ordinary)
        {
            return Ok(None);
        }
        let expected = cell.packet.as_ref().ok_or(BAD_ARGUMENT)?.bytes().len();
        if bytes != expected
        {
            return Err(BAD_ARGUMENT);
        }
        let ReturnStorage::Prepaid(stock) = &mut cell.return_storage
        else
        {
            return Err(BAD_ARGUMENT);
        };
        // Missing prepaid stock means an actual loan, never ordinary fallback.
        let stock = stock.as_ref().ok_or(SERVICE_BUSY)?;
        if stock.bytes.len() != expected || !stock.readers.is_empty()
        {
            return Err(BAD_ARGUMENT);
        }
        let ReturnStorage::Prepaid(stock) = &mut cell.return_storage
        else
        {
            return Err(BAD_ARGUMENT);
        };
        let stock = stock.take().ok_or(SERVICE_BUSY)?;
        Ok(Some(ReturnLoan { job:     job.clone(),
                             storage: stock.bytes,
                             readers: Some(stock.readers),
                             active:  true }))
    })
}
pub(crate) fn returned_prepaid(queue: &Arc<Queue>, execution: &CallbackExecution,
                               mut loan: ReturnLoan, result: i64)
                               -> Result<(), (Status, ReturnLoan)>
{
    if !loan.active || !loan.readers.as_ref().is_some_and(ReaderStorage::is_empty)
    {
        return Err((BAD_ARGUMENT, loan));
    }
    let bytes = loan.storage.take();
    match returned_for(queue, execution, bytes, result, Some(&loan.job))
    {
        Ok(()) =>
        {
            loan.active = false;
            Ok(())
        }
        Err((status, bytes)) =>
        {
            loan.storage = bytes;
            Err((status, loan))
        }
    }
}
impl Drop for ReturnLoan
{
    fn drop(&mut self)
    {
        if !self.active
        {
            return;
        }
        let Some(readers) = self.readers.take()
        else
        {
            return;
        };
        if !readers.is_empty()
        {
            return;
        }
        let stock = Stock { bytes: self.storage.take(),
                            readers };
        // Only a refused capture/commit returns the stock. No callback result,
        // observer, resource ACK or scheduler wake is invented by this Drop.
        let retired = InvocationGate::run(|| {
            let mut state = self.job.state.lock();
            match &mut state.return_storage
            {
                ReturnStorage::Prepaid(slot) if slot.is_none() =>
                {
                    *slot = Some(stock);
                    None
                }
                _ => Some(stock)
            }
        });
        drop(retired);
    }
}
