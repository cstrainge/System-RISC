
use std::{
    any::Any,
    fmt::{self, Debug, Formatter},
    rc::Rc,
};

/// Immutable or explicitly shared native storage. Script modules expose its public interface.
pub trait NativeData: Any
{
    fn type_name(&self) -> &'static str;
    fn display(&self) -> String;
    fn as_any(&self) -> &dyn Any;
    fn key(&self) -> Option<Vec<u8>>
    {
        None
    }
    fn equivalent(&self, _: &dyn NativeData) -> Option<bool>
    {
        None
    }
    fn retained_bytes(&self) -> Option<usize>
    {
        None
    }
    fn operation(&self) -> bool
    {
        false
    }
    fn release(&self) {}
}

#[derive(Clone)]
pub struct NativeValue(pub Rc<dyn NativeData>);

impl NativeValue
{
    pub fn new(value: impl NativeData) -> Self
    {
        Self(Rc::new(value))
    }
    pub fn downcast<T: NativeData>(&self) -> Option<&T>
    {
        self.0.as_any().downcast_ref()
    }
    pub fn key(&self) -> Vec<u8>
    {
        self.0.key().unwrap_or_else(|| {
            (Rc::as_ptr(&self.0) as *const () as usize)
                .to_ne_bytes()
                .to_vec()
        })
    }
}

impl Debug for NativeValue
{
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result
    {
        formatter.write_str(&self.0.display())
    }
}

impl PartialEq for NativeValue
{
    fn eq(&self, other: &Self) -> bool
    {
        Rc::ptr_eq(&self.0, &other.0)
            || self.0.type_name() == other.0.type_name()
                && self.0.equivalent(other.0.as_ref()).unwrap_or_else(|| {
                    self.0
                        .key()
                        .zip(other.0.key())
                        .is_some_and(|(left, right)| left == right)
                })
    }
}
