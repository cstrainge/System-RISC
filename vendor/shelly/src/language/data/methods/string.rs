
use crate::language::data::{ methods::NativeMethodError, value::Value };

fn text(value: &Value) -> Result<&str, NativeMethodError>
{
    match value
    {
        Value::String(text, _) => Ok(text),
        _ => Err("String methods require String arguments".into()),
    }
}

pub(super) fn chars(receiver: &mut Value, _: &[Value]) -> Result<Value, NativeMethodError>
{
    Ok(Value::from_array(text(receiver)?.chars()
        .map(|character| Value::from_string(character.to_string())).collect()))
}

pub(super) fn contains(receiver: &mut Value, args: &[Value]) -> Result<Value, NativeMethodError>
{
    Ok(Value::Boolean(text(receiver)?.contains(text(&args[0])?)))
}

pub(super) fn starts_with(receiver: &mut Value, args: &[Value]) -> Result<Value, NativeMethodError>
{
    Ok(Value::Boolean(text(receiver)?.starts_with(text(&args[0])?)))
}

pub(super) fn ends_with(receiver: &mut Value, args: &[Value]) -> Result<Value, NativeMethodError>
{
    Ok(Value::Boolean(text(receiver)?.ends_with(text(&args[0])?)))
}

pub(super) fn replace(receiver: &mut Value, args: &[Value]) -> Result<Value, NativeMethodError>
{
    Ok(Value::from_string(text(receiver)?.replace(text(&args[0])?, text(&args[1])?)))
}

pub(super) fn split(receiver: &mut Value, args: &[Value]) -> Result<Value, NativeMethodError>
{
    Ok(Value::from_array(text(receiver)?.split(text(&args[0])?)
        .map(|part| Value::from_string(part.to_string())).collect()))
}

pub(super) fn trim(receiver: &mut Value, _: &[Value]) -> Result<Value, NativeMethodError>
{
    Ok(Value::from_string(text(receiver)?.trim().to_string()))
}

pub(super) fn trim_start(receiver: &mut Value, _: &[Value]) -> Result<Value, NativeMethodError>
{
    Ok(Value::from_string(text(receiver)?.trim_start().to_string()))
}

pub(super) fn trim_end(receiver: &mut Value, _: &[Value]) -> Result<Value, NativeMethodError>
{
    Ok(Value::from_string(text(receiver)?.trim_end().to_string()))
}
