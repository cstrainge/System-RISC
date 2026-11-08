
use std::{ cmp::Ordering, collections::HashMap, rc::Rc, fmt::{ self, Debug, Formatter } };

use crate::{ runtime::process::Terminal,
             language::{ data::{ closure::ClosureValue, callable::CallableValue, map_key::MapKey,
                                 collection::Collection,
                                 range::Range,
                                 methods::BoundMethod,
                                 types::{ EnumValue, StructValue, NamedValue,
                                          TypeDefinition, TypeKind, TypeId } },
                         native::NativeFunction,
                         bytecode::FunctionRef } };

#[derive(Clone, Debug, PartialEq)]
pub enum ExecResult
{
    Value(u8),
    Signaled
}


#[derive(Clone)]
pub enum Executable
{
    Yes,
    No,
    External,
    Function(FunctionRef),
    Native(Rc<NativeFunction>),
    Method(Rc<BoundMethod>)
}

impl Debug for Executable
{
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::Yes => write!(f, "Yes"),
            Self::No => write!(f, "No"),
            Self::External => write!(f, "External"),
            Self::Function(function) => write!(f, "Function({:p})", Rc::as_ptr(function)),
            Self::Native(function) => write!(f, "Native({})", function.name),
            Self::Method(method) => write!(f, "Method({:p})", Rc::as_ptr(method))
        }
    }
}

impl PartialEq for Executable
{
    fn eq(&self, other: &Self) -> bool
    {
        match (self, other)
        {
            (Self::Yes, Self::Yes) | (Self::No, Self::No)
            | (Self::External, Self::External) => true,
            (Self::Function(left), Self::Function(right)) => Rc::ptr_eq(left, right),
            (Self::Native(left), Self::Native(right)) => Rc::ptr_eq(left, right),
            (Self::Method(left), Self::Method(right)) => Rc::ptr_eq(left, right),
            _ => false
        }
    }
}


#[derive(Clone, Debug, PartialEq)]
pub enum Value
{
    None,
    Native(crate::language::data::native_value::NativeValue),
    Terminal(Rc<Terminal>),
    Enum(Rc<EnumValue>),
    Struct(Rc<StructValue>),
    Named(Rc<NamedValue>),
    Callable(Rc<CallableValue>),
    Closure(Rc<ClosureValue>),
    Type(Rc<TypeDefinition>),
    ExecResult(ExecResult),
    Integer(i64),
    Float(f64, Option<String>),
    Boolean(bool),
    String(String, Executable),
    Array(Rc<Collection<Vec<Value>>>),
    HashMap(Rc<Collection<HashMap<MapKey, Value>>>),
    Range(Range),
    ArgumentExpansion(Rc<Collection<Vec<Value>>>),
}


impl Value
{
    pub fn underlying(&self) -> &Value
    {
        match self
        {
            Value::Named(item) => item.value.underlying(),
            value => value,
        }
    }

    pub fn is_callable(&self) -> bool
    {
        matches!(self.underlying(), Value::Closure(_) | Value::Callable(_) | Value::String(_,
            Executable::Function(_) | Executable::Native(_) | Executable::Method(_)))
    }

    pub fn is_executable(&self) -> bool
    {
        self.is_callable()
            || self.is_external()
            || matches!(self, Value::String(_, Executable::Yes))
    }

    pub fn is_external(&self) -> bool
    {
        matches!(self.underlying(), Value::String(_, Executable::External))
    }

    pub fn type_name(&self) -> String
    {
        match self
        {
            Value::Native(value) => return value.0.type_name().into(),
            Value::None => "None",
            Value::Terminal(_) => "Terminal",
            Value::Integer(_) => "Integer",
            Value::Float(_, _) => "Float",
            Value::Boolean(_) => "Boolean",
            Value::String(_, _) => "String",
            Value::Array(_) => "Array",
            Value::HashMap(_) => "HashMap",
            Value::Range(_) => "Range",
            Value::ArgumentExpansion(_) => "ArgumentExpansion",
            Value::ExecResult(_) => "ExecResult",
            Value::Enum(item) => return item.definition.name.clone(),
            Value::Struct(item) => return item.definition.name.clone(),
            Value::Named(item) => return item.definition.name.clone(),
            Value::Callable(item) => return item.prototype.name.clone(),
            Value::Closure(_) => "Function",
            Value::Type(_) => "Type",
        }.to_string()
    }

    pub fn from_hash_map(values: HashMap<MapKey, Value>) -> Value
    {
        Value::HashMap(Rc::new(values.into()))
    }

    pub fn from_array(values: Vec<Value>) -> Value
    {
        Value::Array(Rc::new(values.into()))
    }

    pub fn from_argument_expansion(values: Vec<Value>) -> Value
    {
        Value::ArgumentExpansion(Rc::new(values.into()))
    }

    pub fn collection_type(&self) -> Option<TypeId>
    {
        match self
        {
            Value::Array(values) => values.type_id,
            Value::HashMap(values) => values.type_id,
            _ => None,
        }
    }

    pub fn with_collection_type(mut self, id: TypeId) -> Self
    {
        match &mut self
        {
            Value::Array(values) => Rc::make_mut(values).type_id = Some(id),
            Value::HashMap(values) => Rc::make_mut(values).type_id = Some(id),
            _ => {},
        }
        self
    }

    pub fn from_string(s: String) -> Value
    {
        Value::String(s, Executable::No)
    }

    pub fn from_executable_string(s: String) -> Value
    {
        Value::String(s, Executable::Yes)
    }

    pub fn from_status_code(code: Option<i32>) -> Value
    {
        match code
        {
            Some(code) => Value::ExecResult(ExecResult::Value(code as u8)),
            None => Value::ExecResult(ExecResult::Signaled),
        }
    }

    pub fn as_text(&self) -> String
    {
        match self
        {
            Value::Native(value) => value.0.display(),
            Value::Named(item) => item.value.as_text(),
            Value::Callable(item) => item.target.as_text(),
            Value::Closure(_) => "<anonymous>".into(),
            Value::Type(item) => item.name.clone(),
            Value::None => "()".to_string(),
            Value::Terminal(_) => "Terminal".to_string(),
            Value::Enum(value) => value.to_string(),
            Value::Struct(value) =>
                {
                    let TypeKind::Struct(fields) = &value.definition.kind else { unreachable!(); };
                    format!(
                        "{}({})",
                        value.definition.name,
                        fields
                            .iter()
                            .zip(&value.fields)
                            .map(|(field, value)| format!(
                                "{}: {}",
                                field.name,
                                value.collection_text()
                            ))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                },
            Value::Range(range) => range.to_string(),
            Value::ExecResult(code) => format!("ExecResult({})", match code
                {
                    ExecResult::Value(v) => v.to_string(),
                    ExecResult::Signaled => "Signaled".to_string(),
                }),
            Value::Integer(i) => i.to_string(),
            Value::Float(_, Some(s)) => s.clone(),
            Value::Float(f, None) => format!("{f:?}"),
            Value::Boolean(b) => b.to_string(),
            Value::String(s, _) => s.clone(),
            Value::Array(arr) => arr.iter().map(|v| v.as_text()).collect::<Vec<String>>().join(":"),
            Value::ArgumentExpansion(args) => args
                .iter()
                .map(|v| v.as_text())
                .collect::<Vec<String>>()
                .join(":"),
            Value::HashMap(values) =>
                {
                    if values.is_empty() { return "[:]".to_string(); }
                    let mut entries: Vec<_> = values.iter().collect();
                    entries.sort_by(|(left, _), (right, _)| left.cmp(right));
                    format!(
                        "[{}]",
                        entries
                            .into_iter()
                            .map(|(key, value)| format!(
                                "{}: {}",
                                key.to_value().collection_text(),
                                value.collection_text()
                            ))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                },
        }
    }

    fn collection_text(&self) -> String
    {
        match self
        {
            Value::Named(item) => item.value.collection_text(),
            Value::String(text, _) => format!("{:?}", text),
            Value::Array(values) | Value::ArgumentExpansion(values) => format!("[{}]", values.iter()
                .map(Self::collection_text).collect::<Vec<_>>().join(", ")),
            value => value.as_text()
        }
    }

    pub fn as_integer(&self) -> i64
    {
        match self
        {
            Value::Named(item) => item.value.as_integer(),
            Value::Callable(_) | Value::Closure(_) | Value::Type(_) => 0,
            Value::None | Value::Terminal(_) | Value::Native(_) => 0,
            Value::HashMap(_) | Value::Range(_) | Value::Enum(_) | Value::Struct(_) => 0,
            Value::ExecResult(code) => match code
                {
                    ExecResult::Value(v) => *v as i64,
                    ExecResult::Signaled => 0,
                },
            Value::Integer(i) => *i,
            Value::Float(f, _) => *f as i64,
            Value::Boolean(b) => if *b { 1 } else { 0 },
            Value::String(s, _) => s.parse::<i64>().unwrap_or(0),
            Value::Array(arr) => arr.iter().map(|v| v.as_integer()).sum(),
            Value::ArgumentExpansion(args) => args.iter().map(|v| v.as_integer()).sum()
        }
    }

    pub fn as_bool(&self) -> bool
    {
        match self
        {
            Value::Named(item) => item.value.as_bool(),
            Value::Callable(_) | Value::Closure(_) | Value::Type(_) => true,
            Value::None => false,
            Value::Enum(_) | Value::Struct(_) | Value::Terminal(_) | Value::Native(_) => true,
            Value::Range(range) => !range.is_empty(),
            Value::ExecResult(ExecResult::Value(code)) => *code == 0,
            Value::ExecResult(ExecResult::Signaled) => false,
            Value::Integer(value) => *value != 0,
            Value::Float(value, _) => *value != 0.0,
            Value::Boolean(value) => *value,
            Value::String(value, _) => !value.is_empty(),
            Value::Array(values) | Value::ArgumentExpansion(values) => !values.is_empty(),
            Value::HashMap(values) => !values.is_empty()
        }
    }

    // Compare language values without string execution flags or float source spelling.
    pub fn equals(&self, other: &Value) -> bool
    {
        match (self, other)
        {
            (Value::Callable(left), Value::Callable(right)) => left == right,
            (Value::Closure(left), Value::Closure(right)) => left == right,
            (Value::Type(left), Value::Type(right)) => left == right,
            (Value::Named(left), Value::Named(right)) =>
                left.definition.id == right.definition.id && left.value.equals(&right.value),
            (Value::Native(left), Value::Native(right)) => left == right,
            (Value::None, Value::None) => true,
            (Value::Terminal(left), Value::Terminal(right)) => left == right,
            (Value::Enum(left), Value::Enum(right)) => left == right,
            (Value::Struct(left), Value::Struct(right)) => left.definition.id == right.definition.id
                && left.fields.len() == right.fields.len()
                && left.fields.iter().zip(&right.fields).all(|(left, right)| left.equals(right)),
            (Value::Range(left), Value::Range(right)) => left == right,
            (Value::ExecResult(left), Value::ExecResult(right)) => left == right,
            (Value::Integer(left), Value::Integer(right)) => left == right,
            (Value::Float(left, _), Value::Float(right, _)) => left == right,
            (Value::Integer(integer), Value::Float(float, _))
            | (Value::Float(float, _), Value::Integer(integer)) =>
                {
                    Self::integer_float_order(*integer, *float) == Ok(Ordering::Equal)
                },
            (Value::Boolean(left), Value::Boolean(right)) => left == right,
            (Value::String(left, _), Value::String(right, _)) => left == right,
            (Value::HashMap(left), Value::HashMap(right)) => left.len() == right.len()
                && left.iter()
                    .all(|(key, value)| right.get(key).is_some_and(|other| value.equals(other))),
            (Value::Array(left), Value::Array(right))
            | (Value::ArgumentExpansion(left), Value::ArgumentExpansion(right)) =>
                left.len() == right.len()
                    && left.iter().zip(right.iter()).all(|(left, right)| left.equals(right)),
            _ => false
        }
    }

    pub fn arithmetic_error(&self) -> Option<&'static str>
    {
        match self
        {
            Value::Native(_) => Some("Native values cannot be used in arithmetic"),
            Value::Named(item) => item.value.arithmetic_error(),
            Value::Callable(_) | Value::Closure(_) =>
                Some("Functions cannot be used in arithmetic"),
            Value::Type(_) => Some("Types cannot be used in arithmetic"),
            Value::Integer(_) | Value::Float(_, _) => None,
            Value::Terminal(_) => Some("Terminals cannot be used in arithmetic"),
            Value::Enum(_) => Some("Enums cannot be used in arithmetic"),
            Value::Struct(_) => Some("Structs cannot be used in arithmetic"),
            Value::String(_, _) => Some("Strings cannot be used in arithmetic"),
            Value::Boolean(_) => Some("Booleans cannot be used in arithmetic"),
            Value::Array(_) | Value::ArgumentExpansion(_) =>
                Some("Arrays cannot be used in arithmetic"),
            Value::HashMap(_) => Some("Maps cannot be used in arithmetic"),
            Value::Range(_) => Some("Ranges cannot be used in arithmetic"),
            Value::ExecResult(_) => Some("Command statuses cannot be used in arithmetic"),
            Value::None => Some("None cannot be used in arithmetic"),
        }
    }

    pub fn numeric_order(&self, other: &Value) -> Result<Ordering, &'static str>
    {
        if matches!(self, Value::Named(_)) || matches!(other, Value::Named(_))
        {
            let (Value::Named(left), Value::Named(right)) = (self, other) else
            { return Err("Named comparisons require matching types; convert explicitly"); };
            if left.definition.id != right.definition.id
            { return Err("Named comparisons require matching types; convert explicitly"); }
            return left.value.numeric_order(&right.value);
        }
        match (self, other)
        {
            (Value::Integer(left), Value::Integer(right)) => Ok(left.cmp(right)),
            (Value::Integer(left), Value::Float(right, _)) =>
                Self::integer_float_order(*left, *right),
            (Value::Float(left, _), Value::Integer(right)) =>
                Self::integer_float_order(*right, *left).map(Ordering::reverse),
            (Value::Float(left, _), Value::Float(right, _)) =>
                {
                    if !left.is_finite() || !right.is_finite()
                    { return Err("Non-finite floating-point operand"); }
                    Ok(left.partial_cmp(right).unwrap())
                },
            _ => Err("Ordering comparisons require numbers"),
        }
    }


    // Compare against the Float's integer part without rounding the Integer to
    // f64. Check the exact i64 bounds before casting, then resolve fractions.
    fn integer_float_order(integer: i64, float: f64) -> Result<Ordering, &'static str>
    {
        if !float.is_finite() { return Err("Non-finite floating-point operand"); }
        if float >= -(i64::MIN as f64) { return Ok(Ordering::Less); }
        if float < i64::MIN as f64 { return Ok(Ordering::Greater); }
        Ok(integer.cmp(&(float as i64)).then_with(||
            {
                if float.fract() > 0.0 { Ordering::Less }
                else if float.fract() < 0.0 { Ordering::Greater }
                else { Ordering::Equal }
            }))
    }


    pub fn arithmetic_float(&self) -> Option<f64>
    {
        let number = match self
            {
                Value::Integer(number) => *number as f64,
                Value::Float(number, _) => *number,
                _ => return None,
            };
        number.is_finite().then_some(number)
    }
}
