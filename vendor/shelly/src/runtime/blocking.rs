
use std::{
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicUsize, Ordering},
        mpsc::{SyncSender, sync_channel},
    },
    thread::Builder,
};

use tokio::sync::oneshot;

use crate::runtime::tasks::NativeFault;

type Job = Box<dyn FnOnce() + Send>;
static POOL: OnceLock<Result<SyncSender<Job>, String>> = OnceLock::new();
static ACTIVE: AtomicUsize = AtomicUsize::new(0);
struct Slot;
impl Drop for Slot
{
    fn drop(&mut self)
    {
        ACTIVE.fetch_sub(1, Ordering::AcqRel);
    }
}

// A cancelled caller drops only its reply receiver. The owned OS job keeps its slot until it
// ends, so even an indefinitely stalled filesystem can retain at most four bounded jobs.
pub async fn run<T: Send + 'static>(
    operation: impl FnOnce() -> T + Send + 'static,
) -> Result<T, NativeFault>
{
    if ACTIVE.fetch_add(1, Ordering::AcqRel) >= 4
    {
        ACTIVE.fetch_sub(1, Ordering::AcqRel);
        return Err(NativeFault::new("LimitError", "Blocking I/O pool is full"));
    }
    let slot = Slot;
    let pool = POOL
        .get_or_init(|| {
            let (sender, receiver) = sync_channel::<Job>(4);
            let receiver = Arc::new(Mutex::new(receiver));
            for index in 0..4
            {
                let receiver = receiver.clone();
                Builder::new()
                    .name(format!("shelly-io-{}", index))
                    .spawn(move || {
                        loop
                        {
                            let job = receiver.lock().unwrap().recv();
                            match job
                            {
                                Ok(job) => job(),
                                Err(_) => break,
                            }
                        }
                    })
                    .map_err(|error| error.to_string())?;
            }
            Ok(sender)
        })
        .as_ref()
        .map_err(|message| NativeFault::new("OpenError", message.clone()))?;
    let (reply, receiver) = oneshot::channel();
    pool.try_send(Box::new(move || {
        let _slot = slot;
        let value = operation();
        let _ = reply.send(value);
    }))
    .map_err(|_| NativeFault::new("LimitError", "Blocking I/O queue is full"))?;
    receiver
        .await
        .map_err(|_| NativeFault::new("ReadError", "Blocking I/O worker stopped"))
}
