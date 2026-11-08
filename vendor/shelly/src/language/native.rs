
use std::{ collections::HashMap, rc::Rc };

use crate::{
    language::{
        interpreter::{BuiltIn, Interpreter, InterpreterError, InterpreterResult},
        data::{types::TypeId, value::Value},
        text::location::Location,
    },
    runtime::tasks::{NativeFuture, NativeReply},
};

pub enum NativeOutcome
{
    Ready(Value),
    Failed(InterpreterError),
    Wait(NativeWait),
}

pub struct NativeWait
{
    pub future: NativeFuture,
    pub finish: Box<dyn FnOnce(&mut Interpreter, NativeReply) -> InterpreterResult<Value>>,
}

pub type NativeOperation = fn(&mut Interpreter, &Location, &[Value]) -> NativeOutcome;


#[derive(Clone, Copy, PartialEq, Eq)]
pub enum NativeVisibility
{
    Hidden,
    Visible,
}


pub struct NativeFunction
{
    pub name: &'static str,
    pub visibility: NativeVisibility,
    pub body: BuiltIn<'static>,
    pub operation: Option<NativeOperation>,
}


impl NativeFunction
{
    pub fn new(name: &'static str, visibility: NativeVisibility, body: BuiltIn<'static>)
    -> Rc<Self>
    {
        Rc::new(Self {
            name,
            visibility,
            body,
            operation: None,
        })
    }

    pub fn suspending(
        name: &'static str,
        visibility: NativeVisibility,
        operation: NativeOperation,
    ) -> Rc<Self>
    {
        Rc::new(Self {
            name,
            visibility,
            operation: Some(operation),
            body: Rc::new(|_, _, _| unreachable!("Native operation bypassed dispatch")),
        })
    }
}


pub type NativeFunctions = HashMap<&'static str, Rc<NativeFunction>>;


#[derive(Clone)]
pub struct NativeType
{
    pub id: TypeId,
    pub visibility: NativeVisibility,
}
