
use std::collections::BTreeMap;

use crate::language::{ ast::AstType, text::span::SourceSpan };

#[derive(Clone, Debug)]
pub(super) struct Symbol
{
    pub name: String,
    pub kind: Kind,
    pub ty: String,
    pub signature: String,
    pub owner: String,
    pub public: bool,
    pub span: Option<SourceSpan>,
    pub scope: usize,
    pub initializer: String,
    pub parameters: Vec<(String, String)>,
    pub property: Option<(bool, bool)>,
    pub argument: bool,
}

impl Symbol
{
    pub fn new(name: &str, kind: Kind, ty: &str, owner: &str, public: bool) -> Self
    {
        Self
            {
                name: name.into(), kind, ty: ty.into(), signature: String::new(),
                owner: owner.into(), public, span: None, scope: 0, initializer: String::new(),
                parameters: Vec::new(), property: None, argument: false,
            }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kind
{
    Type, Function, Method, Variable, Field, Module, External, Alias, Variant,
}

#[derive(Clone, Default)]
pub(super) struct Type
{
    pub symbol: Option<Symbol>,
    pub fields: BTreeMap<String, Symbol>,
    pub methods: Vec<Symbol>,
    pub variants: Vec<String>,
    pub parent: String,
}

pub(super) struct Scope
{
    pub span: SourceSpan,
    pub parent: Option<usize>,
    pub receiver: Option<String>,
    pub factory: bool,
    pub symbols: BTreeMap<String, Symbol>,
    pub history: BTreeMap<String, Vec<Symbol>>,
}

#[derive(Clone)]
pub(super) struct Import
{
    pub name: String,
    pub target: String,
    pub names: Vec<String>,
    pub public: bool,
    pub parent: bool,
}

pub(super) fn annotation(ty: &AstType) -> String
{
    match ty
    {
        AstType::Named(name) | AstType::ReceiverMember(name) => name.clone(),
        AstType::Array(item) => format!("[{}]", annotation(item)),
        AstType::FixedArray(items) => format!("[{}]", items.iter()
            .map(annotation).collect::<Vec<_>>().join(", ")),
        AstType::Map(key, item) => format!("[{}: {}]", annotation(key), annotation(item)),
        AstType::Union(items) => items.iter().map(annotation).collect::<Vec<_>>().join(" | "),
        AstType::Function(args, result) => format!("fn({}): {}", args.iter()
            .map(annotation).collect::<Vec<_>>().join(", "), annotation(result)),
    }
}
