
use std::rc::Rc;

use heck::ToSnakeCase;

use crate::language::data::value::{ ExecResult, Value };

// Conversion names use the unqualified type name with ordinary snake-case boundaries.
pub fn conversion_type_name(name: &str) -> String
{
    name.rsplit("::").next().unwrap_or(name).to_snake_case()
}

pub fn is_conversion_method(name: &str) -> bool
{
    name.starts_with("from_")
}

// Explicit conversions are separate from the permissive arithmetic helpers on Value.
// TypeRegistry owns target resolution; this module implements the builtin conversions.
pub fn convert_builtin(target: &str, value: &Value) -> Result<Value, String>
{
    let unsupported = || format!("Cannot convert {} to {}", value.type_name(), target);
    match target
    {
        "any" => Ok(value.clone()),
        "None" => Ok(Value::None),
        "Boolean" => Ok(Value::Boolean(value.as_bool())),
        "String" => Ok(Value::from_string(value.as_text())),
        "Integer" => integer(value).map(Value::Integer),
        "Float" => float(value).map(|number| Value::Float(number, None)),
        "Number" => match value
            {
                Value::Float(_, _) => float(value).map(|_| value.clone()),
                Value::String(text, _) =>
                    {
                        if let Ok(number) = text.trim().parse::<i64>()
                        { return Ok(Value::Integer(number)); }
                        float(value).map(|number| Value::Float(number, None))
                    },
                _ => integer(value).map(Value::Integer),
            },
        "ExecResult" => match value
            {
                Value::ExecResult(_) => Ok(value.clone()),
                Value::Boolean(success) =>
                    Ok(Value::ExecResult(ExecResult::Value(u8::from(!success)))),
                _ =>
                    {
                        let code = u8::try_from(integer(value)?)
                            .map_err(|_| "ExecResult requires an exit status in 0..=255"
                                .to_string())?;
                        Ok(Value::ExecResult(ExecResult::Value(code)))
                    },
            },
        "Array" | "ArgumentExpansion" =>
            {
                let values = match value
                    {
                        Value::Array(values) | Value::ArgumentExpansion(values) => values.clone(),
                        Value::Range(range) =>
                            {
                                let length = range.len().ok_or_else(||
                                    "Cannot convert a range with omitted bounds to a collection"
                                        .to_string())?;
                                let length = usize::try_from(length)
                                    .map_err(|_| "Range is too large to convert".to_string())?;
                                let mut values = Vec::new();
                                values.try_reserve_exact(length)
                                    .map_err(|_| "Range is too large to convert".to_string())?;
                                values.extend(range.iter().unwrap().map(Value::Integer));
                                Rc::new(values.into())
                            },
                        _ => return Err(unsupported()),
                    };
                Ok(if target == "Array" { Value::Array(values) }
                    else { Value::ArgumentExpansion(values) })
            },
        "HashMap" if matches!(value, Value::HashMap(_)) => Ok(value.clone()),
        "Type" if matches!(value, Value::Type(_)) => Ok(value.clone()),
        "Range" if matches!(value, Value::Range(_)) => Ok(value.clone()),
        _ => Err(unsupported()),
    }
}


fn integer(value: &Value) -> Result<i64, String>
{
    match value
    {
        Value::Integer(number) => Ok(*number),
        Value::Boolean(value) => Ok(i64::from(*value)),
        Value::ExecResult(ExecResult::Value(code)) => Ok(i64::from(*code)),
        Value::String(text, _) => text.trim().parse::<i64>()
            .map_err(|_| "Integer requires decimal integer text in the signed 64-bit range"
                .to_string()),
        Value::Float(number, _) =>
            {
                // The upper bound is exclusive: i64::MAX rounds up when represented as f64.
                if    !number.is_finite()
                   || *number < i64::MIN as f64
                   || *number >= 9_223_372_036_854_775_808.0
                {
                    return Err("Integer conversion is outside the signed 64-bit range"
                        .to_string());
                }
                if number.fract() != 0.0
                { return Err("Integer conversion would discard a fractional part".to_string()); }
                Ok(*number as i64)
            },
        _ => Err(format!("Cannot convert {} to Integer", value.type_name())),
    }
}


fn float(value: &Value) -> Result<f64, String>
{
    let number = match value
        {
            Value::Float(number, _) => *number,
            Value::Integer(number) => *number as f64,
            Value::Boolean(value) => if *value { 1.0 } else { 0.0 },
            Value::ExecResult(ExecResult::Value(code)) => f64::from(*code),
            Value::String(text, _) => text.trim().parse::<f64>()
                .map_err(|_| "Expected numeric text for floating-point conversion".to_string())?,
            _ => return Err(format!("Cannot convert {} to Float", value.type_name())),
        };
    if !number.is_finite()
    { return Err("Numeric conversion requires a finite value".to_string()); }
    Ok(number)
}
