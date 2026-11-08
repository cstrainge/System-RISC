
use std::{
    cell::RefCell,
    collections::HashSet,
    fmt::{self, Debug, Formatter},
    rc::Rc,
};

use crate::language::{
    bytecode::FunctionRef, data::value::Value, interpreter::CallFrame, text::location::Location,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureStatus
{
    Pending,
    Propagating,
    Acknowledged,
}

#[derive(Clone, Debug)]
pub struct RelatedFailure
{
    pub relation: String,
    pub snapshot: Value,
    pub origin: Location,
    pub native_origin: Option<Location>,
    pub frames: Vec<CallFrame>,
}

#[derive(Clone, Copy, Debug)]
pub enum ControlFailure
{
    Interrupted,
    CleanupAbort,
    CleanupTimeout,
}

#[derive(Clone)]
pub struct FailureOccurrence
{
    pub id: u64,
    pub owner: u64,
    pub control: Option<ControlFailure>,
    pub status: FailureStatus,
    pub origin: Location,
    pub native_origin: Option<Location>,
    pub frames: Vec<CallFrame>,
    pub propagation: Vec<Location>,
    pub related: Vec<RelatedFailure>,
    pub slots: HashSet<usize>,
    pub formatter: Option<FunctionRef>,
    pub rethrow_source: Option<u64>,
}

impl Debug for FailureOccurrence
{
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result
    {
        formatter
            .debug_struct("FailureOccurrence")
            .field("id", &self.id)
            .field("status", &self.status)
            .finish()
    }
}

impl PartialEq for FailureOccurrence
{
    fn eq(&self, other: &Self) -> bool
    {
        self.id == other.id
    }
}

pub type FailureRef = Rc<RefCell<FailureOccurrence>>;

pub fn occurrence(value: &Value) -> Option<FailureRef>
{
    match value.underlying()
    {
        Value::Struct(item) => item.failure.clone(),
        _ => None,
    }
}

pub fn pending(value: &Value) -> Option<FailureRef>
{
    occurrence(value).filter(|item| item.borrow().status == FailureStatus::Pending)
}

pub fn marked(value: &Value) -> bool
{
    matches!(value.underlying(), Value::Struct(item) if item.definition.error)
}
