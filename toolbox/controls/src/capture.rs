//! One cancellation discipline for SDK modal tracking owners.
use crate::{Error, Result};
pub(crate) trait Cancel: Copy
{
    fn cancel(self) -> Result<()>;
}
pub(crate) struct Capture<C: Cancel>(Option<C>);
impl<C: Cancel> Capture<C>
{
    pub fn new(control: C) -> Self
    {
        Self(Some(control))
    }

    pub fn finish<T>(mut self, result: Result<T>) -> Result<T>
    {
        let cancelled = match self.0.map_or(Ok(()), Cancel::cancel)
        {
            Err(Error::NotFound) => Ok(()), // Callback may have disposed the object.
            result => result
        };
        if cancelled.is_ok()
        {
            self.0 = None;
        }
        // Keep the operation's original cause; Drop retries a failed cancellation.
        result.and_then(|value| cancelled.map(|()| value))
    }
}
impl<C: Cancel> Drop for Capture<C>
{
    fn drop(&mut self)
    {
        if let Some(control) = self.0
        {
            let _ = control.cancel();
        }
    }
}
