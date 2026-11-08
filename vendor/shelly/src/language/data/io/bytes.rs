
use std::{any::Any, sync::Arc};

use crate::language::data::native_value::NativeData;

#[derive(Clone, Debug)]
pub struct Bytes(pub Arc<[u8]>);

impl NativeData for Bytes
{
    fn equivalent(&self, other: &dyn NativeData) -> Option<bool>
    {
        other
            .as_any()
            .downcast_ref::<Self>()
            .map(|other| self.0 == other.0)
    }
    fn type_name(&self) -> &'static str
    {
        "__io_Bytes"
    }
    fn display(&self) -> String
    {
        format!("<Bytes {}>", self.0.len())
    }
    fn as_any(&self) -> &dyn Any
    {
        self
    }
    fn key(&self) -> Option<Vec<u8>>
    {
        Some(self.0.to_vec())
    }
}
