
use std::{any::Any, sync::Arc};

use crate::language::data::types::TypeId;
use crate::language::data::{
    native_value::{NativeData, NativeValue},
    value::Value,
};

#[derive(Clone, Debug)]
pub enum JsonNode
{
    Null,
    Boolean(bool),
    Number(String),
    String(String),
    Array(Vec<Arc<JsonNode>>),
    Object(Vec<(String, Arc<JsonNode>)>),
}

#[derive(Clone, Debug)]
pub struct Document
{
    pub root: Arc<JsonNode>,
    pub value: Arc<JsonNode>,
}

impl Document
{
    pub fn new(value: Arc<JsonNode>) -> Self
    {
        Self {
            root: value.clone(),
            value,
        }
    }
    pub fn view(&self, value: Arc<JsonNode>) -> Self
    {
        Self {
            root: self.root.clone(),
            value,
        }
    }
    pub fn detach(&self) -> Self
    {
        Self::new(self.value.clone())
    }
    pub fn into_value(self) -> Value
    {
        Value::Native(NativeValue::new(self))
    }
    pub fn kind(&self) -> &'static str
    {
        match self.value.as_ref()
        {
            JsonNode::Null => "Null",
            JsonNode::Boolean(_) => "Boolean",
            JsonNode::Number(_) => "Number",
            JsonNode::String(_) => "String",
            JsonNode::Array(_) => "Array",
            JsonNode::Object(_) => "Object",
        }
    }
}

impl NativeData for Document
{
    fn key(&self) -> Option<Vec<u8>>
    {
        Some((Arc::as_ptr(&self.value) as usize).to_ne_bytes().to_vec())
    }
    fn type_name(&self) -> &'static str
    {
        "__JsonDocument"
    }
    fn display(&self) -> String
    {
        format!("<JSON {}>", self.kind())
    }
    fn as_any(&self) -> &dyn Any
    {
        self
    }
}

#[derive(Clone, Debug)]
pub struct ExactNumber(pub String);

#[derive(Clone, Debug)]
pub struct Codec
{
    pub target: TypeId,
    pub enum_names: Vec<(usize, String)>,
    pub field_names: Vec<(String, String)>,
    pub omit_null: Vec<String>,
    pub ignore_unknown: Option<bool>,
}

impl NativeData for Codec
{
    fn type_name(&self) -> &'static str
    {
        "__JsonCodec"
    }
    fn display(&self) -> String
    {
        "<JSON Codec>".into()
    }
    fn as_any(&self) -> &dyn Any
    {
        self
    }
}

impl NativeData for ExactNumber
{
    fn retained_bytes(&self) -> Option<usize>
    {
        Some(self.0.len())
    }
    fn type_name(&self) -> &'static str
    {
        "__JsonNumber"
    }
    fn display(&self) -> String
    {
        self.0.clone()
    }
    fn as_any(&self) -> &dyn Any
    {
        self
    }
    fn key(&self) -> Option<Vec<u8>>
    {
        Some(self.0.as_bytes().to_vec())
    }
}

pub fn document(value: &Value) -> Option<&Document>
{
    match value.underlying()
    {
        Value::Native(value) => value.downcast(),
        _ => None,
    }
}

pub fn exact_number(value: &Value) -> Option<&ExactNumber>
{
    match value.underlying()
    {
        Value::Native(value) => value.downcast(),
        _ => None,
    }
}
