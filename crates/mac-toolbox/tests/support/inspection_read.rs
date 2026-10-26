//! Real userspace readers retry a changed revision; asynchronous I/O may finish
//! between the individual inspection traps. Assertions still use one coherent
//! ledger revision, never a mixture of rows from different snapshots.
use mac_toolbox::{Error, System,
                  inspection::{LedgerSnapshot, Scope}};

pub fn own<T>(system: &System<'_>, mut read: impl FnMut(LedgerSnapshot<'_>) -> Result<T, Error>)
              -> Result<T, Error>
{
    for _ in 0..64
    {
        let result = (|| {
            let snapshot = system.inspection().snapshot(Scope::Own).map_err(|e| e.cause)?;
            read(snapshot.ledger(0).map_err(|e| e.cause)?)
        })();
        match result
        {
            Err(Error::ScanStale) => (),
            other => return other
        }
    }
    Err(Error::ScanStale)
}
