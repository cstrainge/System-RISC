
use std::{collections::HashMap, sync::Arc};

use crate::language::data::{
    formats::json::{
        document::{Document, ExactNumber, JsonNode},
        parser::{JsonError, Limits},
    },
    map_key::MapKey,
    native_value::NativeValue,
    value::Value,
};

pub fn materialize(node: &JsonNode, exact: bool) -> Result<Value, JsonError>
{
    Ok(match node
    {
        JsonNode::Null => Value::None,
        JsonNode::Boolean(value) => Value::Boolean(*value),
        JsonNode::String(value) => Value::from_string(value.clone()),
        JsonNode::Number(token) =>
        {
            if !token.contains(['.', 'e', 'E'])
                && let Ok(value) = token.parse::<i64>()
            {
                Value::Integer(value)
            }
            else if exact
            {
                Value::Native(NativeValue::new(ExactNumber(token.clone())))
            }
            else
            {
                Value::Float(float(token)?, None)
            }
        }
        JsonNode::Array(items) => Value::from_array(
            items
                .iter()
                .map(|value| materialize(value, exact))
                .collect::<Result<_, _>>()?,
        ),
        JsonNode::Object(fields) => Value::from_hash_map(
            fields
                .iter()
                .map(|(name, value)| Ok((MapKey::String(name.clone()), materialize(value, exact)?)))
                .collect::<Result<HashMap<_, _>, JsonError>>()?,
        ),
    })
}

pub fn float(token: &str) -> Result<f64, JsonError>
{
    let value: f64 = token.parse().map_err(|_| {
        JsonError::new("MappingError", "JSON number cannot be represented as Float")
    })?;
    if !value.is_finite()
    {
        return Err(JsonError::new(
            "MappingError",
            "JSON number overflows Float",
        ));
    }
    let mantissa = token.split(['e', 'E']).next().unwrap();
    if value == 0.0 && mantissa.bytes().any(|byte| matches!(byte, b'1'..=b'9'))
    {
        return Err(JsonError::new(
            "MappingError",
            "JSON number underflows Float to zero",
        ));
    }
    Ok(value)
}

pub fn integer(token: &str) -> Result<i64, JsonError>
{
    let negative = token.starts_with('-');
    let mut parts = token.trim_start_matches('-').split(['e', 'E']);
    let mantissa = parts.next().unwrap();
    let exponent: i32 = parts
        .next()
        .unwrap_or("0")
        .parse()
        .map_err(|_| JsonError::new("MappingError", "Invalid number exponent"))?;
    let fraction = mantissa
        .split_once('.')
        .map_or(0, |(_, tail)| tail.len() as i32);
    let mut digits: String = mantissa
        .chars()
        .filter(|character| *character != '.')
        .collect();
    let scale = exponent - fraction;
    if scale < 0
    {
        let remove = (-scale) as usize;
        if remove > digits.len()
        {
            if digits.bytes().all(|byte| byte == b'0')
            {
                return Ok(0);
            }
            return Err(JsonError::new(
                "MappingError",
                "JSON number is not integral",
            ));
        }
        if !digits.as_bytes()[digits.len() - remove..]
            .iter()
            .all(|byte| *byte == b'0')
        {
            return Err(JsonError::new(
                "MappingError",
                "JSON number is not integral",
            ));
        }
        digits.truncate(digits.len() - remove);
    }
    let trimmed = digits.trim_start_matches('0');
    if trimmed.is_empty()
    {
        return Ok(0);
    }
    if trimmed.len().saturating_add(scale.max(0) as usize) > 19
    {
        return Err(JsonError::new(
            "MappingError",
            "JSON integer is out of range",
        ));
    }
    let mut result = if negative
    {
        "-".to_string()
    }
    else
    {
        String::new()
    };
    result.push_str(trimmed);
    for _ in 0..scale.max(0)
    {
        result.push('0');
    }
    result
        .parse()
        .map_err(|_| JsonError::new("MappingError", "JSON integer is out of range"))
}

pub fn encode_document(
    document: &Document,
    limits: &Limits,
    pretty: bool,
) -> Result<String, JsonError>
{
    let mut encoder = Encoder {
        output: String::new(),
        members: 0,
        limits,
        pretty,
    };
    encoder.value(&document.value, 0)?;
    Ok(encoder.output)
}

struct Encoder<'a>
{
    output: String,
    members: usize,
    limits: &'a Limits,
    pretty: bool,
}

impl Encoder<'_>
{
    fn push(&mut self, text: &str) -> Result<(), JsonError>
    {
        if self.output.len().saturating_add(text.len()) > self.limits.bytes
        {
            return Err(JsonError::new(
                "LimitError",
                "Encoded JSON exceeds its byte limit",
            ));
        }
        self.output.push_str(text);
        Ok(())
    }

    fn indent(&mut self, depth: usize) -> Result<(), JsonError>
    {
        if self.pretty
        {
            self.push(&format!("\n{}", "  ".repeat(depth)))?;
        }
        Ok(())
    }

    fn value(&mut self, node: &JsonNode, depth: usize) -> Result<(), JsonError>
    {
        if depth > self.limits.depth
        {
            return Err(JsonError::new(
                "LimitError",
                "JSON encoding exceeds its depth limit",
            ));
        }
        self.members += 1;
        if self.members > self.limits.members
        {
            return Err(JsonError::new(
                "LimitError",
                "JSON encoding exceeds its member limit",
            ));
        }
        match node
        {
            JsonNode::Null => self.push("null"),
            JsonNode::Boolean(value) => self.push(if *value { "true" } else { "false" }),
            JsonNode::Number(value) => self.push(value),
            JsonNode::String(value) =>
            {
                if value.len() > self.limits.scalar
                {
                    return Err(JsonError::new(
                        "LimitError",
                        "JSON string exceeds scalar limit",
                    ));
                }
                self.push(
                    &serde_json::to_string(value)
                        .map_err(|error| JsonError::new("EncodingError", error.to_string()))?,
                )
            }
            JsonNode::Array(items) =>
            {
                self.push("[")?;
                for (index, item) in items.iter().enumerate()
                {
                    if index > 0
                    {
                        self.push(",")?;
                    }
                    self.indent(depth + 1)?;
                    self.value(item, depth + 1)?;
                }
                if !items.is_empty()
                {
                    self.indent(depth)?;
                }
                self.push("]")
            }
            JsonNode::Object(fields) =>
            {
                self.push("{")?;
                for (index, (name, value)) in fields.iter().enumerate()
                {
                    if index > 0
                    {
                        self.push(",")?;
                    }
                    self.indent(depth + 1)?;
                    if name.len() > self.limits.scalar
                    {
                        return Err(JsonError::new(
                            "LimitError",
                            "JSON name exceeds scalar limit",
                        ));
                    }
                    self.push(
                        &serde_json::to_string(name)
                            .map_err(|error| JsonError::new("EncodingError", error.to_string()))?,
                    )?;
                    self.push(if self.pretty { ": " } else { ":" })?;
                    self.value(value, depth + 1)?;
                }
                if !fields.is_empty()
                {
                    self.indent(depth)?;
                }
                self.push("}")
            }
        }
    }
}

pub fn pointer_parts(pointer: &str) -> Result<Vec<String>, JsonError>
{
    if pointer.is_empty()
    {
        return Ok(Vec::new());
    }
    if !pointer.starts_with('/')
    {
        return Err(JsonError::new(
            "PathError",
            "JSON pointer must be empty or start with '/'",
        ));
    }
    pointer[1..]
        .split('/')
        .map(|part| {
            let mut result = String::new();
            let mut chars = part.chars();
            while let Some(character) = chars.next()
            {
                if character != '~'
                {
                    result.push(character);
                    continue;
                }
                result.push(match chars.next()
                {
                    Some('0') => '~',
                    Some('1') => '/',
                    _ =>
                    {
                        return Err(JsonError::new("PathError", "Invalid JSON pointer escape"));
                    }
                });
            }
            Ok(result)
        })
        .collect()
}

pub fn find(document: &Document, pointer: &str) -> Result<Option<Document>, JsonError>
{
    let mut value = document.value.clone();
    for part in pointer_parts(pointer)?
    {
        value = match value.as_ref()
        {
            JsonNode::Object(fields) => match fields.iter().find(|(name, _)| *name == part)
            {
                Some((_, value)) => value.clone(),
                None => return Ok(None),
            },
            JsonNode::Array(items) =>
            {
                if part.is_empty()
                    || part.len() > 1 && part.starts_with('0')
                    || !part.bytes().all(|byte| byte.is_ascii_digit())
                {
                    return Err(JsonError::new("PathError", "Invalid JSON array index"));
                }
                let index: usize = part
                    .parse()
                    .map_err(|_| JsonError::new("PathError", "JSON array index overflows"))?;
                match items.get(index)
                {
                    Some(value) => value.clone(),
                    None => return Ok(None),
                }
            }
            _ =>
            {
                return Err(JsonError::new(
                    "PathError",
                    "JSON pointer traverses a scalar",
                ));
            }
        };
    }
    Ok(Some(document.view(value)))
}

pub fn edit(
    node: &Arc<JsonNode>,
    parts: &[String],
    replacement: Option<Arc<JsonNode>>,
    append: bool,
) -> Result<Arc<JsonNode>, JsonError>
{
    let Some((part, tail)) = parts.split_first()
    else
    {
        if append
        {
            let JsonNode::Array(items) = node.as_ref()
            else
            {
                return Err(JsonError::new("PathError", "append requires a JSON array"));
            };
            let mut items = items.clone();
            items.push(replacement.ok_or_else(|| JsonError::new("PathError", "Missing value"))?);
            return Ok(Arc::new(JsonNode::Array(items)));
        }
        return replacement
            .ok_or_else(|| JsonError::new("PathError", "Cannot remove document root"));
    };
    match node.as_ref()
    {
        JsonNode::Object(fields) =>
        {
            let mut fields = fields.clone();
            if let Some(index) = fields.iter().position(|(name, _)| name == part)
            {
                if tail.is_empty() && replacement.is_none() && !append
                {
                    fields.remove(index);
                }
                else
                {
                    fields[index].1 = edit(&fields[index].1, tail, replacement, append)?;
                }
            }
            else if tail.is_empty()
                && !append
                && let Some(value) = replacement
            {
                fields.push((part.clone(), value));
            }
            else
            {
                return Err(JsonError::new(
                    "PathError",
                    "JSON pointer location is missing",
                ));
            }
            Ok(Arc::new(JsonNode::Object(fields)))
        }
        JsonNode::Array(items) =>
        {
            if part.is_empty()
                || part.len() > 1 && part.starts_with('0')
                || !part.bytes().all(|byte| byte.is_ascii_digit())
            {
                return Err(JsonError::new("PathError", "Invalid JSON array index"));
            }
            let index: usize = part
                .parse()
                .map_err(|_| JsonError::new("PathError", "JSON array index overflows"))?;
            if index >= items.len()
            {
                return Err(JsonError::new(
                    "PathError",
                    "JSON array index is out of range",
                ));
            }
            let mut items = items.clone();
            if tail.is_empty() && replacement.is_none() && !append
            {
                items.remove(index);
            }
            else
            {
                items[index] = edit(&items[index], tail, replacement, append)?;
            }
            Ok(Arc::new(JsonNode::Array(items)))
        }
        _ => Err(JsonError::new(
            "PathError",
            "JSON pointer traverses a scalar",
        )),
    }
}
