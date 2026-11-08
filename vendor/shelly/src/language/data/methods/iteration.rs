
use std::rc::Rc;

use crate::language::data::{ methods::NativeMethodError, value::Value };


// Direct method calls advance the receiver. For loops use a private snapshot and
// an equivalent native cursor to avoid repeatedly copying or shifting collections.
pub(super) fn next_item(receiver: &mut Value, _: &[Value]) -> Result<Value, NativeMethodError>
{
    match receiver
    {
        Value::Array(values) | Value::ArgumentExpansion(values) =>
            {
                if values.is_empty() { return Ok(Value::None); }
                Ok(Rc::make_mut(values).remove(0))
            },
        Value::HashMap(values) =>
            {
                let Some(key) = values.keys().next().cloned() else { return Ok(Value::None); };
                let value = Rc::make_mut(values).remove(&key).unwrap();
                Ok(Value::from_array(vec![key.to_value(), value]))
            },
        Value::Range(range) =>
            {
                let (Some(start), Some(end)) = (range.start, range.end) else
                { return Err("Iteration requires a range with both bounds".into()); };
                if range.is_empty() { return Ok(Value::None); }
                if start == end
                { range.inclusive = false; }
                else { range.start = Some(start + 1); }
                Ok(Value::Integer(start))
            },
        _ => Err("Native next_item requires an array, hash map, or range".into()),
    }
}
