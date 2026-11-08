
use std::{collections::HashSet, mem::size_of, rc::Rc, sync::Arc};

use crate::language::data::{
    formats::json::document::{Document, JsonNode},
    io::Bytes,
    map_key::MapKey,
    value::{Executable, Value},
};

pub struct Budget
{
    bytes: usize,
    nodes: usize,
    seen: HashSet<(u8, usize)>,
}
impl Default for Budget
{
    fn default() -> Self
    {
        Self {
            bytes: 16 * 1024 * 1024,
            nodes: 100_000,
            seen: HashSet::new(),
        }
    }
}
impl Budget
{
    pub fn used_bytes(&self) -> usize { 16 * 1024 * 1024 - self.bytes }

    fn charge(&mut self, bytes: usize) -> Result<(), String>
    {
        self.bytes = self
            .bytes
            .checked_sub(bytes)
            .ok_or("Pipeline retained-byte limit exceeded")?;
        self.nodes = self
            .nodes
            .checked_sub(1)
            .ok_or("Pipeline retained-node limit exceeded")?;
        Ok(())
    }
    fn json(&mut self, node: &Arc<JsonNode>, depth: usize) -> Result<(), String>
    {
        if !self.seen.insert((1, Arc::as_ptr(node) as usize))
        {
            return Ok(());
        }
        if depth > 256
        {
            return Err("Retained graph depth limit exceeded".into());
        }
        self.charge(size_of::<JsonNode>())?;
        match node.as_ref()
        {
            JsonNode::String(text) | JsonNode::Number(text) => self.charge(text.capacity()),
            JsonNode::Array(items) =>
            {
                self.charge(items.capacity() * size_of::<Arc<JsonNode>>())?;
                for node in items
                {
                    self.json(node, depth + 1)?;
                }
                Ok(())
            }
            JsonNode::Object(items) =>
            {
                self.charge(items.capacity() * size_of::<(String, Arc<JsonNode>)>())?;
                for (name, node) in items
                {
                    self.charge(name.capacity())?;
                    self.json(node, depth + 1)?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }
    pub fn value(&mut self, value: &Value) -> Result<(), String>
    {
        self.visit(value, 0)
    }
    fn visit(&mut self, value: &Value, depth: usize) -> Result<(), String>
    {
        if depth > 256
        {
            return Err("Retained graph depth limit exceeded".into());
        }
        self.charge(size_of::<Value>())?;
        match value
        {
            Value::String(_, Executable::Function(_) | Executable::Method(_))
            | Value::Callable(_)
            | Value::Closure(_) => Err("Callable captures need an explicit stream adapter".into()),
            Value::String(text, _) => self.charge(text.capacity()),
            Value::Named(item) =>
            {
                if !self.seen.insert((2, Rc::as_ptr(item) as usize))
                {
                    return Ok(());
                }
                self.visit(&item.value, depth + 1)
            }
            Value::Struct(item) =>
            {
                if !self.seen.insert((3, Rc::as_ptr(item) as usize))
                {
                    return Ok(());
                }
                for value in &item.fields
                {
                    self.visit(value, depth + 1)?;
                }
                Ok(())
            }
            Value::Array(items) | Value::ArgumentExpansion(items) =>
            {
                if !self.seen.insert((4, Rc::as_ptr(items) as usize))
                {
                    return Ok(());
                }
                for value in items.iter()
                {
                    self.visit(value, depth + 1)?;
                }
                Ok(())
            }
            Value::HashMap(items) =>
            {
                if !self.seen.insert((5, Rc::as_ptr(items) as usize))
                {
                    return Ok(());
                }
                for (key, value) in items.iter()
                {
                    if let MapKey::String(key) = key
                    {
                        self.charge(key.capacity())?;
                    }
                    self.visit(value, depth + 1)?;
                }
                Ok(())
            }
            Value::Native(native) =>
            {
                if let Some(document) = native.downcast::<Document>()
                {
                    return self.json(&document.root, depth);
                }
                if let Some(bytes) = native.downcast::<Bytes>()
                {
                    if self
                        .seen
                        .insert((6, Arc::as_ptr(&bytes.0) as *const u8 as usize))
                    {
                        self.charge(bytes.0.len())?;
                    }
                    return Ok(());
                }
                if !self
                    .seen
                    .insert((7, Rc::as_ptr(&native.0) as *const () as usize))
                {
                    return Ok(());
                }
                self.charge(
                    native
                        .0
                        .retained_bytes()
                        .ok_or("Opaque value needs an explicit stream adapter")?,
                )
            }
            _ => Ok(()),
        }
    }
}
