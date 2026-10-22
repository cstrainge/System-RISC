use super::*;
mod sealed
{
    pub trait Sealed
    {
    }
}
/// Sealed closure chain; applications construct it only through `Drag::promise`.
#[doc(hidden)]
pub trait Promises: sealed::Sealed + Send + Sync
{
    fn empty() -> Self;
    fn supply(&mut self, request: &mut PromiseRequest<'_>) -> Result<()>;
}
#[doc(hidden)]
pub struct PromiseRequest<'a>
{
    pub(super) route:    Route<'a>,
    pub(super) item:     u32,
    pub(super) kind:     FlavorType,
    pub(super) supplied: bool
}
impl PromiseRequest<'_>
{
    pub(super) fn fail(&mut self, error: ToolboxError) -> Result<()>
    {
        self.route.call(wire::Call { operation: wire::FULFILL,
                                      item: self.item,
                                      kind: self.kind.0,
                                      error: error.status(),
                                      ..Default::default() },
                         &[],
                         &mut [])?;
        self.supplied = true;
        Ok(())
    }
}
pub struct NoPromises;
impl sealed::Sealed for NoPromises
{
}
impl Promises for NoPromises
{
    fn empty() -> Self
    {
        Self
    }

    fn supply(&mut self, _: &mut PromiseRequest<'_>) -> Result<()>
    {
        Ok(())
    }
}
#[doc(hidden)]
pub struct WithPromise<P, F, T>
{
    previous: P,
    producer: Option<F>,
    item:     u32,
    _data:    core::marker::PhantomData<fn() -> T>
}
impl<P, F, T> WithPromise<P, F, T>
{
    pub(super) fn new(previous: P, item: u32, producer: F) -> Self
    {
        Self { previous,
               item,
               producer: Some(producer),
               _data: core::marker::PhantomData }
    }
}
impl<P: Promises, F, T> sealed::Sealed for WithPromise<P, F, T>
{
}
impl<P: Promises, F: FnOnce() -> core::result::Result<T, ToolboxError> + Send + Sync, T: FlavorData>
    Promises for WithPromise<P, F, T>
{
    fn empty() -> Self
    {
        Self { previous: P::empty(),
               producer: None,
               item:     0,
               _data:    core::marker::PhantomData }
    }

    fn supply(&mut self, request: &mut PromiseRequest<'_>) -> Result<()>
    {
        if self.item == request.item && request.kind == T::TYPE
        {
            let Some(producer) = self.producer.take()
            else
            {
                return request.fail(ToolboxError::NotFound);
            };
            match producer()
            {
                Ok(data) =>
                {
                    match request.route.call(wire::Call { operation: wire::FULFILL,
                                                          item: request.item,
                                                          kind: request.kind.0,
                                                          ..Default::default() },
                                             data.bytes(),
                                             &mut [])
                    {
                        Ok(_) =>
                        {
                            request.supplied = true;
                            Ok(())
                        }
                        // The consumed producer cannot retry. Convert a rejected
                        // payload into a fulfilled error and wake its receiver.
                        Err(DragError::Unknown(status)) =>
                        {
                            request.fail(ToolboxError::from_status(status))
                        }
                        Err(error) => Err(error)
                    }
                }
                Err(error) => request.fail(error)
            }
        }
        else
        {
            self.previous.supply(request)
        }
    }
}
