
use std::{
    any::Any,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use tokio::sync::Mutex;

use crate::{
    language::data::{
        formats::json::{
            document::{Document, JsonNode},
            parser::{JsonError, Limits, parse_document},
            values::{find, pointer_parts},
        },
        io::handles::Reader,
        native_value::NativeData,
    },
    runtime::tasks::NativeFault,
};

#[derive(Clone)]
struct Frame
{
    path: Vec<String>,
    object: bool,
    key: Option<String>,
    index: usize,
    separator: bool,
}

struct State
{
    bytes: Vec<u8>,
    cursor: usize,
    token_start: Option<usize>,
    quoted: bool,
    escape: bool,
    frames: Vec<Frame>,
    target: Vec<String>,
    array: Option<usize>,
    item_start: Option<usize>,
    item_depth: usize,
    item_string: bool,
    item_escape: bool,
    after_item: bool,
    after_comma: bool,
    ended: bool,
    eof: bool,
}

struct Storage
{
    reader: Reader,
    limits: Limits,
    state: Mutex<State>,
    busy: AtomicBool,
    closed: AtomicBool,
}
#[derive(Clone)]
pub struct ArrayReader(Arc<Storage>);

pub enum Reply
{
    Item(Document),
    End,
    Failed(JsonError),
}

fn syntax(message: &str) -> JsonError
{
    JsonError::new("SyntaxError", message)
}
fn space(byte: u8) -> bool
{
    matches!(byte, b' ' | b'\t' | b'\r' | b'\n')
}

impl State
{
    fn value_path(&self) -> Vec<String>
    {
        self.frames.last().map_or_else(Vec::new, |frame| {
            let mut path = frame.path.clone();
            path.push(
                if frame.object
                {
                    frame.key.clone().unwrap_or_default()
                }
                else
                {
                    frame.index.to_string()
                },
            );
            path
        })
    }
    fn complete(&mut self)
    {
        if let Some(frame) = self.frames.last_mut()
        {
            frame.separator = true;
            if !frame.object
            {
                frame.index += 1;
            }
        }
    }
    fn token(&mut self, start: usize, end: usize, limits: &Limits) -> Result<(), JsonError>
    {
        let byte = self.bytes[start];
        match byte
        {
            b'{' | b'[' =>
            {
                let path = self.value_path();
                if byte == b'[' && path == self.target
                {
                    self.array = Some(end);
                    return Ok(());
                }
                if self.frames.len() >= limits.depth
                {
                    return Err(JsonError::new(
                        "LimitError",
                        "JSON nesting exceeds its limit",
                    ));
                }
                self.frames.push(Frame {
                    path,
                    object: byte == b'{',
                    key: None,
                    index: 0,
                    separator: false,
                });
            }
            b'}' | b']' =>
            {
                self.frames.pop();
                self.complete();
            }
            b',' =>
            {
                if let Some(frame) = self.frames.last_mut()
                {
                    frame.separator = false;
                    if frame.object
                    {
                        frame.key = None;
                    }
                }
            }
            b':' =>
            {}
            b'"' if self
                .frames
                .last()
                .is_some_and(|frame| frame.object && frame.key.is_none()) =>
            {
                let text = std::str::from_utf8(&self.bytes[start..end])
                    .map_err(|_| syntax("Invalid UTF-8 in JSON key"))?;
                let document = parse_document(text, limits)?;
                let JsonNode::String(name) = document.value.as_ref()
                else
                {
                    unreachable!();
                };
                self.frames.last_mut().unwrap().key = Some(name.clone());
            }
            _ => self.complete(),
        }
        Ok(())
    }
    fn locate(&mut self, limits: &Limits) -> Result<(), JsonError>
    {
        while self.array.is_none() && self.cursor < self.bytes.len()
        {
            let byte = self.bytes[self.cursor];
            if let Some(start) = self.token_start
            {
                if self.quoted
                {
                    self.cursor += 1;
                    if self.escape
                    {
                        self.escape = false;
                    }
                    else if byte == b'\\'
                    {
                        self.escape = true;
                    }
                    else if byte == b'"'
                    {
                        self.token_start = None;
                        self.quoted = false;
                        self.token(start, self.cursor, limits)?;
                    }
                }
                else if space(byte) || matches!(byte, b',' | b'}' | b']' | b':')
                {
                    self.token_start = None;
                    self.token(start, self.cursor, limits)?;
                }
                else
                {
                    self.cursor += 1;
                }
                if self.cursor.saturating_sub(start) > limits.scalar
                {
                    return Err(JsonError::new(
                        "LimitError",
                        "JSON scalar exceeds its limit",
                    ));
                }
            }
            else if space(byte)
            {
                self.cursor += 1;
            }
            else if matches!(byte, b'{' | b'[' | b'}' | b']' | b',' | b':')
            {
                let start = self.cursor;
                self.cursor += 1;
                self.token(start, self.cursor, limits)?;
            }
            else
            {
                self.token_start = Some(self.cursor);
                self.quoted = byte == b'"';
                self.cursor += 1;
            }
        }
        Ok(())
    }
    fn poll(&mut self, limits: &Limits) -> Result<Option<Reply>, JsonError>
    {
        self.locate(limits)?;
        if self.array.is_none()
        {
            return Ok(None);
        }
        while !self.ended && self.cursor < self.bytes.len()
        {
            let byte = self.bytes[self.cursor];
            if let Some(start) = self.item_start
            {
                let delimiter = !self.item_string
                    && self.item_depth == 0
                    && (space(byte) || byte == b',' || byte == b']');
                if delimiter
                {
                    let text = std::str::from_utf8(&self.bytes[start..self.cursor])
                        .map_err(|_| syntax("Invalid UTF-8 in JSON array item"))?;
                    let document = parse_document(text, limits)?;
                    self.item_start = None;
                    self.after_item = true;
                    return Ok(Some(Reply::Item(document)));
                }
                self.cursor += 1;
                if self.item_string
                {
                    if self.item_escape
                    {
                        self.item_escape = false;
                    }
                    else if byte == b'\\'
                    {
                        self.item_escape = true;
                    }
                    else if byte == b'"'
                    {
                        self.item_string = false;
                    }
                }
                else
                {
                    match byte
                    {
                        b'"' => self.item_string = true,
                        b'{' | b'[' => self.item_depth += 1,
                        b'}' | b']' => self.item_depth = self.item_depth.saturating_sub(1),
                        _ =>
                        {}
                    }
                }
                if self.cursor - start > limits.bytes
                {
                    return Err(JsonError::new(
                        "LimitError",
                        "JSON array item exceeds byte limit",
                    ));
                }
            }
            else if space(byte)
            {
                self.cursor += 1;
            }
            else if byte == b']'
            {
                if self.after_comma
                {
                    return Err(syntax("Trailing comma in JSON array"));
                }
                self.ended = true;
                self.cursor += 1;
            }
            else if self.after_item
            {
                if byte != b','
                {
                    return Err(syntax("Expected comma after JSON array item"));
                }
                self.after_item = false;
                self.after_comma = true;
                self.cursor += 1;
            }
            else
            {
                self.item_start = Some(self.cursor);
                self.item_depth = 0;
                self.item_string = false;
                self.item_escape = false;
                self.after_comma = false;
            }
        }
        Ok(None)
    }
    fn finish(&self, limits: &Limits) -> Result<(), JsonError>
    {
        let text = std::str::from_utf8(&self.bytes).map_err(|_| syntax("Invalid UTF-8 in JSON"))?;
        let document = parse_document(text, limits)?;
        let pointer = if self.target.is_empty()
        {
            String::new()
        }
        else
        {
            format!(
                "/{}",
                self.target
                    .iter()
                    .map(|part| part.replace('~', "~0").replace('/', "~1"))
                    .collect::<Vec<_>>()
                    .join("/")
            )
        };
        let selected = find(&document, &pointer)?
            .ok_or_else(|| JsonError::new("PathError", "Array pointer is missing"))?;
        if !matches!(selected.value.as_ref(), JsonNode::Array(_))
        {
            return Err(JsonError::new(
                "MappingError",
                "Selected JSON value is not an array",
            ));
        }
        Ok(())
    }
}

impl ArrayReader
{
    pub fn new(reader: Reader, pointer: &str, limits: Limits) -> Result<Self, JsonError>
    {
        let target = pointer_parts(pointer)?;
        if !target.is_empty() && limits.duplicates == super::parser::DuplicateNames::LastWins
        {
            return Err(JsonError::new(
                "ValidationError",
                "LastWins needs whole-document selection; streaming pointers require Reject or FirstWins",
            ));
        }
        let reader = reader
            .leased()
            .map_err(|error| JsonError::new("ValidationError", error.message))?;
        Ok(Self(Arc::new(Storage {
            reader,
            limits,
            busy: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            state: Mutex::new(State {
                bytes: Vec::new(),
                cursor: 0,
                token_start: None,
                quoted: false,
                escape: false,
                frames: Vec::new(),
                target,
                array: None,
                item_start: None,
                item_depth: 0,
                item_string: false,
                item_escape: false,
                after_item: false,
                after_comma: false,
                ended: false,
                eof: false,
            }),
        })))
    }
    pub fn close(&self)
    {
        self.0.closed.store(true, Ordering::Release);
        self.0.reader.close();
    }
    pub async fn next(&self) -> Result<Reply, NativeFault>
    {
        if self.0.busy.swap(true, Ordering::AcqRel)
        {
            return Err(NativeFault::new(
                "LeaseError",
                "JSON array reader already has a consumer",
            ));
        }
        struct Busy<'a>(&'a AtomicBool);
        impl Drop for Busy<'_>
        {
            fn drop(&mut self)
            {
                self.0.store(false, Ordering::Release);
            }
        }
        let _busy = Busy(&self.0.busy);
        let mut state = self.0.state.lock().await;
        loop
        {
            match state.poll(&self.0.limits)
            {
                Ok(Some(reply)) => return Ok(reply),
                Ok(None) =>
                {}
                Err(error) =>
                {
                    self.close();
                    return Ok(Reply::Failed(error));
                }
            }
            if state.eof
            {
                let result = state.finish(&self.0.limits);
                self.close();
                return Ok(result.map_or_else(Reply::Failed, |_| Reply::End));
            }
            match self.0.reader.read(4096).await?
            {
                Some(bytes) =>
                {
                    if state.bytes.len().saturating_add(bytes.0.len()) > self.0.limits.bytes
                    {
                        self.close();
                        return Ok(Reply::Failed(JsonError::new(
                            "LimitError",
                            "JSON document exceeds byte limit",
                        )));
                    }
                    state.bytes.extend_from_slice(&bytes.0);
                }
                None => state.eof = true,
            }
        }
    }
}
impl NativeData for ArrayReader
{
    fn type_name(&self) -> &'static str
    {
        "__JsonArrayReader"
    }
    fn display(&self) -> String
    {
        "<JSON array reader>".into()
    }
    fn as_any(&self) -> &dyn Any
    {
        self
    }
    fn operation(&self) -> bool
    {
        !self.0.closed.load(Ordering::Acquire)
    }
    fn release(&self)
    {
        self.close();
    }
}
