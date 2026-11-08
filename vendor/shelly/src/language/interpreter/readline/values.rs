
use std::{any::Any, collections::HashSet, rc::Rc};

use nu_ansi_term::Style;

use reedline::Color;

use crate::{
    language::{
        data::{
            map_key::MapKey,
            native_value::{NativeData, NativeValue},
            types::{EnumValue, StructValue, TypeKind},
            value::Value,
        },
        interpreter::Interpreter,
    },
    runtime::readline::{Fault, Limits, Text},
};

use super::Result;

pub(super) fn native<T: NativeData>(value: &Value) -> Option<&T>
{
    if let Value::Native(value) = value.underlying()
    {
        value.downcast()
    }
    else
    {
        None
    }
}
pub(super) fn data(value: impl NativeData) -> Value
{
    Value::Native(NativeValue::new(value))
}
pub(super) fn field<'a>(value: &'a Value, name: &str) -> &'a Value
{
    match value.underlying()
    {
        Value::HashMap(fields) => fields
            .get(&MapKey::String(name.into()))
            .unwrap_or(&Value::None),
        Value::Struct(item) =>
        {
            let TypeKind::Struct(fields) = &item.definition.kind
            else
            {
                return &Value::None;
            };
            fields
                .iter()
                .position(|field| field.name == name)
                .map_or(&Value::None, |index| &item.fields[index])
        }
        _ => &Value::None,
    }
}
pub(super) fn string(value: &Value) -> Option<&str>
{
    if let Value::String(text, _) = value.underlying()
    {
        Some(text)
    }
    else
    {
        None
    }
}
pub(super) fn integer(value: &Value) -> Option<i64>
{
    if let Value::Integer(value) = value.underlying()
    {
        Some(*value)
    }
    else
    {
        None
    }
}
pub(super) fn optional_string(value: &Value) -> std::result::Result<Option<String>, Fault>
{
    if matches!(value.underlying(), Value::None)
    {
        Ok(None)
    }
    else
    {
        string(value)
            .map(|v| Some(v.into()))
            .ok_or_else(|| Fault::new("ConfigError", "Expected String"))
    }
}
pub(super) fn enum_name(value: &Value) -> Option<&str>
{
    if    let Value::Enum(item) = value.underlying()
       && let TypeKind::Enum(variants) = &item.definition.kind
    {
        return Some(&variants[item.variant].name);
    }
    None
}
pub(super) fn record(interpreter: &Interpreter, name: &str, fields: Vec<Value>) -> Value
{
    let registry = &interpreter.scope().types;
    Value::Struct(Rc::new(StructValue {
        definition: registry.get(registry.names[name]),
        fields,
        constructing: false,
        failure: None,
    }))
}
pub(super) fn enumeration(interpreter: &Interpreter, name: &str, member: &str) -> Value
{
    let registry = &interpreter.scope().types;
    let definition = registry.get(registry.names[name]);
    let TypeKind::Enum(variants) = &definition.kind
    else
    {
        unreachable!()
    };
    let variant = variants.iter().position(|v| v.name == member).unwrap();
    Value::Enum(Rc::new(EnumValue {
        definition,
        variant,
    }))
}
pub(super) fn bounded(
    value: &Value,
    default: i64,
    min: i64,
    max: i64,
) -> std::result::Result<i64, Fault>
{
    if matches!(value.underlying(), Value::None)
    {
        return Ok(default);
    }
    integer(value)
        .filter(|v| (min..=max).contains(v))
        .ok_or_else(|| {
            Fault::new(
                "ConfigError",
                format!("Expected Integer in {}..{}", min, max),
            )
        })
}
pub(super) fn limits(value: &Value) -> std::result::Result<Limits, Fault>
{
    Ok(Limits {
        input: bounded(field(value, "max_input_bytes"), 65536, 1, 1048576)? as usize,
        completions: bounded(field(value, "max_completions"), 1000, 1, 10000)? as usize,
        completion_bytes: bounded(field(value, "max_completion_bytes"), 1048576, 1, 4194304)?
            as usize,
        spans: bounded(field(value, "max_highlight_spans"), 4096, 1, 16384)? as usize,
        milliseconds: bounded(field(value, "callback_timeout_ms"), 50, 1, 100)? as u64,
        instructions: bounded(field(value, "callback_instructions"), 100000, 1, 250000)? as usize,
    })
}
impl NativeData for Text
{
    fn type_name(&self) -> &'static str
    {
        "__readline_Text"
    }
    fn display(&self) -> String
    {
        self.0.iter().map(|(_, text)| text.as_str()).collect()
    }
    fn as_any(&self) -> &dyn Any
    {
        self
    }
    fn retained_bytes(&self) -> Option<usize>
    {
        Some(self.bytes() + self.0.len() * 128)
    }
}
#[derive(Clone)]
struct NativeColor(Color);
impl NativeData for NativeColor
{
    fn type_name(&self) -> &'static str
    {
        "__readline_Color"
    }
    fn display(&self) -> String
    {
        format!("{:?}", self.0)
    }
    fn as_any(&self) -> &dyn Any
    {
        self
    }
    fn retained_bytes(&self) -> Option<usize>
    {
        Some(32)
    }
}
pub(super) fn text(value: &Value) -> std::result::Result<Text, Fault>
{
    if let Some(text) = native::<Text>(value)
    {
        if text.bytes() > 16384
        {
            return Err(Fault::new("LimitError", "Text exceeds 16 KiB"));
        }
        Ok(text.clone())
    }
    else if let Some(value) = string(value)
    {
        if value.len() > 16384
        {
            return Err(Fault::new("LimitError", "Text exceeds 16 KiB"));
        }
        let text = Text::plain(value);
        if text.bytes() > 16384
        {
            return Err(Fault::new("LimitError", "Escaped text exceeds 16 KiB"));
        }
        Ok(text)
    }
    else
    {
        Err(Fault::new(
            "ConfigError",
            "Prompt must return String or Text",
        ))
    }
}
pub(super) fn style(value: &Value) -> std::result::Result<Style, Fault>
{
    let color = |name| {
        let value = field(value, name);
        if matches!(value.underlying(), Value::None)
        {
            Ok(None)
        }
        else
        {
            native::<NativeColor>(value)
                .map(|v| Some(v.0))
                .ok_or_else(|| Fault::new("ConfigError", "Style color must be Color"))
        }
    };
    let boolean = |name| {
        let value = field(value, name);
        match value.underlying()
        {
            Value::None => Ok(false),
            Value::Boolean(value) => Ok(*value),
            _ => Err(Fault::new("ConfigError", "Style attribute must be Boolean")),
        }
    };
    let mut style = Style::new();
    style.foreground = color("foreground")?;
    style.background = color("background")?;
    style.is_bold = boolean("bold")?;
    style.is_dimmed = boolean("dim")?;
    style.is_underline = boolean("underline")?;
    Ok(style)
}
pub(super) fn span(value: &Value, text: &str) -> std::result::Result<reedline::Span, Fault>
{
    let start = integer(field(value, "start"))
        .ok_or_else(|| Fault::new("CallbackError", "Span start must be Integer"))?;
    let end = integer(field(value, "end"))
        .ok_or_else(|| Fault::new("CallbackError", "Span end must be Integer"))?;
    if start < 0
        || end < start
        || end as usize > text.len()
        || !text.is_char_boundary(start as usize)
        || !text.is_char_boundary(end as usize)
    {
        return Err(Fault::new(
            "CallbackError",
            "Span must be a valid UTF-8 byte range",
        ));
    }
    Ok(reedline::Span::new(start as usize, end as usize))
}

impl Interpreter
{
    pub(super) fn readline_value(
        &self,
        command: &str,
        input: &Value,
        rest: &[Value],
    ) -> Result<Option<Value>>
    {
        let value = match command
        {
            "text" => data(text(input)?),
            "styled" =>
            {
                let text =
                    string(input).ok_or_else(|| Fault::new("ConfigError", "Expected String"))?;
                if text.len() > 16384
                {
                    return Err(Fault::new("LimitError", "Styled text exceeds 16 KiB").into());
                }
                data(Text(vec![(
                    style(rest.first().unwrap_or(&Value::None))?,
                    crate::runtime::readline::visible(text),
                )]))
            }
            "concat" =>
            {
                let mut left = text(input)?;
                let right = text(rest.first().unwrap_or(&Value::None))?;
                if left.bytes() + right.bytes() > 16384 || left.0.len() + right.0.len() > 4096
                {
                    return Err(Fault::new("LimitError", "Text exceeds its limit").into());
                }
                left.0.extend(right.0);
                data(left)
            }
            "color" =>
            {
                let color = if let Some(index) = integer(input)
                {
                    Color::Fixed(
                        u8::try_from(index)
                            .map_err(|_| Fault::new("ConfigError", "Color is 0..255"))?,
                    )
                }
                else if let Value::Array(items) = input.underlying()
                {
                    let rgb = items
                        .iter()
                        .map(|v| integer(v).and_then(|v| u8::try_from(v).ok()))
                        .collect::<Option<Vec<_>>>()
                        .filter(|v| v.len() == 3)
                        .ok_or_else(|| Fault::new("ConfigError", "RGB needs three bytes"))?;
                    Color::Rgb(rgb[0], rgb[1], rgb[2])
                }
                else if let Value::Enum(value) = input.underlying()
                {
                    Color::Fixed(value.variant as u8)
                }
                else
                {
                    return Err(Fault::new("ConfigError", "Invalid Color").into());
                };
                data(NativeColor(color))
            }
            "slice" =>
            {
                let text =
                    string(input).ok_or_else(|| Fault::new("ConfigError", "Expected String"))?;
                let span = span(rest.first().unwrap_or(&Value::None), text)?;
                Value::from_string(text[span.start..span.end].into())
            }
            "words" =>
            {
                let Value::Array(items) = input.underlying()
                else
                {
                    return Err(Fault::new("ConfigError", "Expected words Array").into());
                };
                let prefix = rest.first().and_then(string).unwrap_or("");
                let mut seen = HashSet::new();
                let mut results = Vec::new();
                for item in items.iter()
                {
                    let value = string(item)
                        .ok_or_else(|| Fault::new("ConfigError", "Expected word String"))?;
                    if value.starts_with(prefix) && seen.insert(value)
                    {
                        results.push(Value::from_string(value.into()));
                    }
                }
                Value::from_array(results)
            }
            _ => return Ok(None),
        };
        Ok(Some(value))
    }
}
