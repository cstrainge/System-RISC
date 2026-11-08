
use std::{ rc::Rc, cmp::Ordering, hash::{ Hash, Hasher } };

use crate::{ runtime::process::Terminal,
             language::data::{ closure::ClosureValue, callable::CallableValue,
                               value::{ ExecResult, Value },
                               range::Range,
                               types::{ EnumValue, StructValue, NamedValue, TypeDefinition } } };

// Immutable, canonical keys keep hashing consistent with language equality.
// Collections are snapshots; map entry order and string execution flags do not
// affect identity. NaNs form one canonical key so lookup remains reflexive.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MapKey
{
    None,
    Native(Rc<NativeKey>),
    Terminal(Rc<Terminal>),
    Enum(Rc<EnumValue>),
    Struct(Rc<StructKey>),
    Named(Rc<NamedKey>),
    Callable(Rc<CallableValue>),
    Closure(Rc<ClosureValue>),
    Type(Rc<TypeDefinition>),
    ExecResult(u8),
    Signaled,
    Integer(i64),
    Float(u64),
    Boolean(bool),
    String(String),
    Range(Range),
    Array(Rc<Vec<MapKey>>),
    ArgumentExpansion(Rc<Vec<MapKey>>),
    HashMap(Rc<Vec<(MapKey, MapKey)>>),
}


impl MapKey
{
    pub fn from_value(value: &Value) -> Self
    {
        match value
        {
            Value::Closure(value) => Self::Closure(value.clone()),
            Value::Type(value) => Self::Type(value.clone()),
            Value::Callable(value) => Self::Callable(value.clone()),
            Value::Named(value) => Self::Named(Rc::new(NamedKey
                { value: value.clone(), contents: Self::from_value(&value.value) })),
            Value::Native(value) => Self::Native(Rc::new(NativeKey
                { name: value.0.type_name(), key: value.key(), value: value.clone() })),
            Value::None => Self::None,
            Value::Terminal(value) => Self::Terminal(value.clone()),
            Value::Enum(value) => Self::Enum(value.clone()),
            Value::Struct(value) => Self::Struct(Rc::new(StructKey
                {
                    value: value.clone(),
                    fields: value.fields.iter().map(Self::from_value).collect(),
                })),
            Value::Range(range) => Self::Range(*range),
            Value::ExecResult(ExecResult::Value(code)) => Self::ExecResult(*code),
            Value::ExecResult(ExecResult::Signaled) => Self::Signaled,
            Value::Integer(value) => Self::Integer(*value),
            Value::Float(value, _) =>
                {
                    if    value.fract() == 0.0
                       && *value >= i64::MIN as f64
                       && *value < -(i64::MIN as f64)
                    {
                        Self::Integer(*value as i64)
                    }
                    else
                    {
                        Self::Float(
                            if value.is_nan()
                            {
                                f64::NAN.to_bits()
                            }
                            else
                            {
                                value.to_bits()
                            },
                        )
                    }
                },
            Value::Boolean(value) => Self::Boolean(*value),
            Value::String(value, _) => Self::String(value.clone()),
            Value::Array(values) =>
                Self::Array(Rc::new(values.iter().map(Self::from_value).collect())),
            Value::ArgumentExpansion(values) =>
                Self::ArgumentExpansion(Rc::new(values.iter().map(Self::from_value).collect())),
            Value::HashMap(values) =>
                {
                    let mut entries: Vec<_> = values.iter()
                        .map(|(key, value)| (key.clone(), Self::from_value(value))).collect();
                    entries.sort();
                    Self::HashMap(Rc::new(entries))
                }
        }
    }

    pub fn to_value(&self) -> Value
    {
        match self
        {
            Self::Closure(value) => Value::Closure(value.clone()),
            Self::Type(value) => Value::Type(value.clone()),
            Self::Callable(value) => Value::Callable(value.clone()),
            Self::Named(key) => Value::Named(key.value.clone()),
            Self::Native(key) => Value::Native(key.value.clone()),
            Self::None => Value::None,
            Self::Terminal(value) => Value::Terminal(value.clone()),
            Self::Enum(value) => Value::Enum(value.clone()),
            Self::Struct(key) => Value::Struct(key.value.clone()),
            Self::Range(range) => Value::Range(*range),
            Self::ExecResult(code) => Value::ExecResult(ExecResult::Value(*code)),
            Self::Signaled => Value::ExecResult(ExecResult::Signaled),
            Self::Integer(value) => Value::Integer(*value),
            Self::Float(bits) => Value::Float(f64::from_bits(*bits), None),
            Self::Boolean(value) => Value::Boolean(*value),
            Self::String(value) => Value::from_string(value.clone()),
            Self::Array(values) => Value::from_array(values.iter().map(Self::to_value).collect()),
            Self::ArgumentExpansion(values) =>
                Value::from_argument_expansion(values.iter().map(Self::to_value).collect()),
            Self::HashMap(values) => Value::from_hash_map(values.iter()
                .map(|(key, value)| (key.clone(), value.to_value())).collect())
        }
    }
}

#[derive(Clone, Debug)]
pub struct NamedKey
{
    value: Rc<NamedValue>,
    contents: MapKey,
}

impl PartialEq for NamedKey
{
    fn eq(&self, other: &Self) -> bool
    { self.value.definition.id == other.value.definition.id && self.contents == other.contents }
}
impl Eq for NamedKey {}
impl PartialOrd for NamedKey
{
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> { Some(self.cmp(other)) }
}
impl Ord for NamedKey
{
    fn cmp(&self, other: &Self) -> Ordering
    {
        (self.value.definition.id, &self.contents)
            .cmp(&(other.value.definition.id, &other.contents))
    }
}
impl Hash for NamedKey
{
    fn hash<H: Hasher>(&self, state: &mut H)
    {
        self.value.definition.id.hash(state);
        self.contents.hash(state);
    }
}


// Preserve the typed snapshot for iteration; hash and compare canonical fields.
// Rebuilding a Float field from an integral canonical key would lose its type.
#[derive(Clone, Debug)]
pub struct StructKey
{
    value: Rc<StructValue>,
    fields: Vec<MapKey>
}
impl PartialEq for StructKey
{
    fn eq(&self, other: &Self) -> bool
    {
        self.value.definition.id == other.value.definition.id && self.fields == other.fields
    }
}
impl Eq for StructKey {}
impl PartialOrd for StructKey
{
    fn partial_cmp(&self, other: &Self) -> Option<Ordering>
    {
        Some(self.cmp(other))
    }
}
impl Ord for StructKey
{
    fn cmp(&self, other: &Self) -> Ordering
    {
        (self.value.definition.id, &self.fields).cmp(&(other.value.definition.id, &other.fields))
    }
}
impl Hash for StructKey
{
    fn hash<H: Hasher>(&self, state: &mut H)
    {
        self.value.definition.id.hash(state);
        self.fields.hash(state);
    }
}

#[derive(Clone, Debug)]
pub struct NativeKey
{
    name: &'static str,
    key: Vec<u8>,
    value: crate::language::data::native_value::NativeValue,
}

impl PartialEq for NativeKey
{
    fn eq(&self, other: &Self) -> bool
    {
        self.name == other.name && self.key == other.key
    }
}
impl Eq for NativeKey {}
impl PartialOrd for NativeKey
{
    fn partial_cmp(&self, other: &Self) -> Option<Ordering>
    {
        Some(self.cmp(other))
    }
}
impl Ord for NativeKey
{
    fn cmp(&self, other: &Self) -> Ordering
    {
        (self.name, &self.key).cmp(&(other.name, &other.key))
    }
}
impl Hash for NativeKey
{
    fn hash<H: Hasher>(&self, state: &mut H)
    {
        self.name.hash(state);
        self.key.hash(state);
    }
}
