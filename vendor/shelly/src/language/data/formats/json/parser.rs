
use std::{collections::HashSet, panic::Location as NativeLocation, sync::Arc};

use crate::language::{
    data::formats::json::document::{Document, JsonNode},
    text::location::Location,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DuplicateNames
{
    Reject,
    FirstWins,
    LastWins,
}

#[derive(Clone)]
pub struct Limits
{
    pub bytes: usize,
    pub depth: usize,
    pub scalar: usize,
    pub members: usize,
    pub duplicates: DuplicateNames,
}

impl Default for Limits
{
    fn default() -> Self
    {
        Self {
            bytes: 16 * 1024 * 1024,
            depth: 128,
            scalar: 1024 * 1024,
            members: 100_000,
            duplicates: DuplicateNames::Reject,
        }
    }
}

#[derive(Clone, Debug)]
pub struct JsonError
{
    pub kind: &'static str,
    pub message: String,
    pub pointer: String,
    pub line: usize,
    pub column: usize,
    pub native: Location,
}

impl JsonError
{
    #[track_caller]
    pub fn new(kind: &'static str, message: impl Into<String>) -> Self
    {
        let caller = NativeLocation::caller();
        Self {
            kind,
            message: message.into(),
            pointer: String::new(),
            line: 1,
            column: 1,
            native: Location::new(
                caller.file(),
                caller.line() as usize,
                caller.column() as usize,
            ),
        }
    }

    pub fn at(mut self, pointer: String) -> Self
    {
        self.pointer = pointer;
        self
    }
}

struct Parser<'a>
{
    text: &'a str,
    position: usize,
    members: usize,
    limits: &'a Limits,
}

pub fn parse_document(text: &str, limits: &Limits) -> Result<Document, JsonError>
{
    if text.len() > limits.bytes
    {
        return Err(JsonError::new(
            "LimitError",
            "JSON document exceeds its byte limit",
        ));
    }
    let mut parser = Parser {
        text,
        position: 0,
        members: 0,
        limits,
    };
    let result = (|| {
        parser.space();
        let value = parser.value(0)?;
        parser.space();
        if parser.position != text.len()
        {
            return Err(JsonError::new(
                "SyntaxError",
                "Trailing input after JSON document",
            ));
        }
        Ok(Document::new(value))
    })();
    result.map_err(|mut error| {
        let mut position = parser.position.min(text.len());
        while !text.is_char_boundary(position)
        {
            position -= 1;
        }
        let prefix = &text[..position];
        error.line = prefix.bytes().filter(|byte| *byte == b'\n').count() + 1;
        error.column = prefix.rsplit('\n').next().unwrap_or("").chars().count() + 1;
        error
    })
}

impl Parser<'_>
{
    fn byte(&self) -> Option<u8>
    {
        self.text.as_bytes().get(self.position).copied()
    }
    fn space(&mut self)
    {
        while matches!(self.byte(), Some(b' ' | b'\t' | b'\r' | b'\n'))
        {
            self.position += 1;
        }
    }

    fn member(&mut self) -> Result<(), JsonError>
    {
        self.members += 1;
        if self.members > self.limits.members
        {
            return Err(JsonError::new(
                "LimitError",
                "JSON document exceeds its member limit",
            ));
        }
        Ok(())
    }

    fn value(&mut self, depth: usize) -> Result<Arc<JsonNode>, JsonError>
    {
        if depth > self.limits.depth
        {
            return Err(JsonError::new(
                "LimitError",
                "JSON nesting exceeds its depth limit",
            ));
        }
        self.space();
        let value = match self.byte()
        {
            Some(b'n') =>
            {
                self.keyword("null")?;
                JsonNode::Null
            }
            Some(b't') =>
            {
                self.keyword("true")?;
                JsonNode::Boolean(true)
            }
            Some(b'f') =>
            {
                self.keyword("false")?;
                JsonNode::Boolean(false)
            }
            Some(b'"') => JsonNode::String(self.string()?),
            Some(b'-' | b'0'..=b'9') => JsonNode::Number(self.number()?),
            Some(b'[') =>
            {
                self.position += 1;
                self.space();
                let mut values = Vec::new();
                if self.byte() != Some(b']')
                {
                    loop
                    {
                        self.member()?;
                        values.push(self.value(depth + 1)?);
                        self.space();
                        if self.byte() != Some(b',')
                        {
                            break;
                        }
                        self.position += 1;
                    }
                }
                self.expect(b']')?;
                JsonNode::Array(values)
            }
            Some(b'{') =>
            {
                self.position += 1;
                self.space();
                let mut values = Vec::new();
                let mut names = HashSet::new();
                if self.byte() != Some(b'}')
                {
                    loop
                    {
                        self.member()?;
                        self.space();
                        let name = self.string()?;
                        let fresh = names.insert(name.clone());
                        if !fresh && self.limits.duplicates == DuplicateNames::Reject
                        {
                            return Err(JsonError::new(
                                "SyntaxError",
                                "Duplicate JSON object name",
                            ));
                        }
                        self.space();
                        self.expect(b':')?;
                        let value = self.value(depth + 1)?;
                        if fresh
                        {
                            values.push((name, value));
                        }
                        else if self.limits.duplicates == DuplicateNames::LastWins
                        {
                            let entry = values.iter_mut().find(|(key, _)| *key == name).unwrap();
                            entry.1 = value;
                        }
                        self.space();
                        if self.byte() != Some(b',')
                        {
                            break;
                        }
                        self.position += 1;
                    }
                }
                self.expect(b'}')?;
                JsonNode::Object(values)
            }
            _ => return Err(JsonError::new("SyntaxError", "Expected a JSON value")),
        };
        Ok(Arc::new(value))
    }

    fn expect(&mut self, byte: u8) -> Result<(), JsonError>
    {
        if self.byte() != Some(byte)
        {
            return Err(JsonError::new(
                "SyntaxError",
                format!("Expected '{}'", byte as char),
            ));
        }
        self.position += 1;
        Ok(())
    }

    fn keyword(&mut self, keyword: &str) -> Result<(), JsonError>
    {
        if !self.text[self.position..].starts_with(keyword)
        {
            return Err(JsonError::new("SyntaxError", "Invalid JSON literal"));
        }
        self.position += keyword.len();
        Ok(())
    }

    fn string(&mut self) -> Result<String, JsonError>
    {
        let start = self.position;
        self.expect(b'"')?;
        let mut escaped = false;
        while let Some(byte) = self.byte()
        {
            self.position += 1;
            if byte == b'"' && !escaped
            {
                let value: String = serde_json::from_str(&self.text[start..self.position])
                    .map_err(|error| JsonError::new("SyntaxError", error.to_string()))?;
                if value.len() > self.limits.scalar
                {
                    return Err(JsonError::new(
                        "LimitError",
                        "JSON string exceeds its scalar limit",
                    ));
                }
                return Ok(value);
            }
            if byte < 0x20
            {
                return Err(JsonError::new("SyntaxError", "Unescaped control character"));
            }
            if escaped
            {
                escaped = false;
            }
            else if byte == b'\\'
            {
                escaped = true;
            }
            if self.position - start > self.limits.scalar.saturating_mul(6).saturating_add(2)
            {
                return Err(JsonError::new(
                    "LimitError",
                    "JSON string token exceeds its limit",
                ));
            }
        }
        Err(JsonError::new("SyntaxError", "Unterminated JSON string"))
    }

    fn number(&mut self) -> Result<String, JsonError>
    {
        let start = self.position;
        if self.byte() == Some(b'-')
        {
            self.position += 1;
        }
        match self.byte()
        {
            Some(b'0') => self.position += 1,
            Some(b'1'..=b'9') => self.digits(),
            _ => return Err(JsonError::new("SyntaxError", "Invalid JSON number")),
        }
        if self.byte() == Some(b'.')
        {
            self.position += 1;
            self.require_digits()?;
        }
        if matches!(self.byte(), Some(b'e' | b'E'))
        {
            self.position += 1;
            if matches!(self.byte(), Some(b'+' | b'-'))
            {
                self.position += 1;
            }
            let exponent = self.position;
            self.require_digits()?;
            let value: u32 = self.text[exponent..self.position].parse().map_err(|_| {
                JsonError::new("LimitError", "JSON exponent exceeds its work limit")
            })?;
            if value > 10_000
            {
                return Err(JsonError::new(
                    "LimitError",
                    "JSON exponent exceeds its work limit",
                ));
            }
        }
        if self.position - start > 1024
        {
            return Err(JsonError::new(
                "LimitError",
                "JSON number token exceeds its limit",
            ));
        }
        Ok(self.text[start..self.position].into())
    }

    fn digits(&mut self)
    {
        while matches!(self.byte(), Some(b'0'..=b'9'))
        {
            self.position += 1;
        }
    }

    fn require_digits(&mut self) -> Result<(), JsonError>
    {
        if !matches!(self.byte(), Some(b'0'..=b'9'))
        {
            return Err(JsonError::new(
                "SyntaxError",
                "Expected a digit in JSON number",
            ));
        }
        self.digits();
        Ok(())
    }
}
