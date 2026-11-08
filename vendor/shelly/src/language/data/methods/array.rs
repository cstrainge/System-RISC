
use std::cmp::Ordering;

use crate::language::data::{ methods::NativeMethodError, value::Value };



fn elements(receiver: &Value) -> Result<&[Value], NativeMethodError>
{
    match receiver
    {
        Value::Array(values) | Value::ArgumentExpansion(values) => Ok(values),
        _ => Err("Array methods require an array or argument expansion".into()),
    }
}

pub(super) fn count(receiver: &mut Value, _: &[Value]) -> Result<Value, NativeMethodError>
{
    let length = i64::try_from(elements(receiver)?.len())
        .map_err(|_| "Array length exceeds Integer range".to_string())?;
    Ok(Value::Integer(length))
}

pub(super) fn reverse(receiver: &mut Value, _: &[Value]) -> Result<Value, NativeMethodError>
{
    let mut result = Value::from_array(elements(receiver)?.iter().rev().cloned().collect());
    if let Some(id) = receiver.collection_type() { result = result.with_collection_type(id); }
    Ok(result)
}

pub(super) fn zip(receiver: &mut Value, arguments: &[Value]) -> Result<Value, NativeMethodError>
{
    let left = elements(receiver)?;
    let right = elements(&arguments[0])?;
    Ok(Value::from_array(left.iter().zip(right).map(|(left, right)|
        Value::from_array(vec![left.clone(), right.clone()])).collect()))
}

fn compare(left: &Value, right: &Value) -> Option<Ordering>
{
    match (left, right)
    {
        (Value::Integer(_) | Value::Float(_, _), Value::Integer(_) | Value::Float(_, _)) =>
            left.numeric_order(right).ok(),
        (Value::String(left, _), Value::String(right, _)) => Some(left.cmp(right)),
        (Value::Boolean(left), Value::Boolean(right)) => Some(left.cmp(right)),
        (Value::Enum(left), Value::Enum(right)) if left.definition.id == right.definition.id =>
            Some(left.variant.cmp(&right.variant)),
        _ => None,
    }
}

pub(super) fn sort(receiver: &mut Value, _: &[Value]) -> Result<Value, NativeMethodError>
{
    let values = elements(receiver)?;
    // Validate even singleton inputs. Comparison must be total within the chosen category.
    if let Some(first) = values.first()
    {
        for value in values
        {
            if compare(first, value).is_none()
            {
                return Err("sort requires numbers, strings, booleans, or variants of one enum; \
                    values must be mutually comparable and numbers finite".into());
            }
        }
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(|left, right| compare(left, right).unwrap());
    let mut result = Value::from_array(sorted);
    if let Some(id) = receiver.collection_type() { result = result.with_collection_type(id); }
    Ok(result)
}
