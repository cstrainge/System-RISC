//! Loader storage can be a contiguous slice or a set of separately owned pages.
//! Execution addresses remain in LoadPlan; these offsets name storage bytes.
use core::fmt;

/// Unpublished storage stays owned by the loader through success and refusal.
/// Each write validates its range and preserves its typed cause. A late refusal
/// may leave partial initialization; callers must not publish that image and
/// may retry the same retained storage. This trait itself never allocates.
pub trait Destination
{
    type Error;
    fn length(&self) -> usize;
    fn clear(&mut self) -> Result<(), Self::Error>;
    fn write(&mut self, offset: usize, bytes: &[u8]) -> Result<(), Self::Error>;
}
#[derive(Debug)]
pub enum WriteError<E>
{
    Format(crate::Error),
    Destination(E)
}
impl<E: fmt::Display> fmt::Display for WriteError<E>
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::Format(error) => error.fmt(f),
            Self::Destination(error) =>
            {
                write!(f, "native image destination refused initialization: {error}")
            }
        }
    }
}
impl<E: core::error::Error + 'static> core::error::Error for WriteError<E>
{
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)>
    {
        match self
        {
            Self::Format(error) => Some(error),
            Self::Destination(error) => Some(error)
        }
    }
}
pub(super) struct Slice<'a>(pub &'a mut [u8]);
impl Destination for Slice<'_>
{
    type Error = crate::Error;

    fn length(&self) -> usize
    {
        self.0.len()
    }

    fn clear(&mut self) -> crate::Result<()>
    {
        self.0.fill(0);
        Ok(())
    }

    fn write(&mut self, offset: usize, bytes: &[u8]) -> crate::Result<()>
    {
        let end = offset.checked_add(bytes.len())
                        .ok_or(crate::Error::Malformed("native destination offset overflow"))?;
        let output = self.0
                         .get_mut(offset..end)
                         .ok_or(crate::Error::Malformed("native write lies outside destination"))?;
        output.copy_from_slice(bytes);
        Ok(())
    }
}
