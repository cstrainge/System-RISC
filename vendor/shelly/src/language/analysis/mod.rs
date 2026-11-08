
use std::{ collections::{ BTreeMap, BTreeSet }, ops::{ Index, IndexMut }, sync::Arc };

use crate::language::{ parser::{ EditorAnalysis,
                                 editor::{ analyze_editor_tree_with_tab_size,
                                           analyze_editor_tree_configured } },
                       text::span::SourceSpan };

mod semantic;
pub mod highlight;


#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SymbolKind
{
    Variable,
    Type,
    Function,
    Method,
    Builtin,
    Alias,
    ExternalCommand,
    Field,
    EnumVariant,
    Module,
}


#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SymbolDescriptor
{
    pub kind: SymbolKind,
    pub type_id: Option<usize>,
    pub return_type: Option<usize>,
}

impl SymbolDescriptor
{
    pub fn new(kind: SymbolKind, type_id: Option<usize>) -> Self
    { Self { kind, type_id, return_type: None } }
}


/**
 * Owned type relationships only. No value, receiver, getter, bytecode, or module loader
 * survives the snapshot boundary. Registered IDs retain their nominal identities.
 */
#[derive(Clone, Debug, Default)]
pub struct TypeDescriptor
{
    pub name: String,
    pub parent: Option<usize>,
    pub item: Option<usize>,
    pub key: Option<usize>,
    pub fields: BTreeMap<String, SymbolDescriptor>,
    pub properties: BTreeMap<String, SymbolDescriptor>,
    pub variants: BTreeSet<String>,
    pub methods: BTreeMap<String, SymbolDescriptor>,
    pub call_result: Option<usize>,
}


#[derive(Clone, Debug, Default)]
pub struct SymbolSnapshot
{
    pub tab_width: Option<usize>,
    pub symbols: BTreeMap<String, SymbolDescriptor>,
    pub types: Arc<Vec<TypeDescriptor>>,
    pub external_commands: BTreeSet<String>,
    pub method_arguments: BTreeMap<(usize, String), usize>,
    pub hidden_symbols: BTreeSet<String>,
}


/**
 * Share the immutable registered table between editing passes. Provisional types are
 * appended locally, so ordinary keystrokes do not copy every known method and field.
 */
#[derive(Clone, Debug)]
pub struct TypeTable
{
    registered: Arc<Vec<TypeDescriptor>>,
    local: Vec<TypeDescriptor>,
}

impl TypeTable
{
    pub(super) fn new(registered: Arc<Vec<TypeDescriptor>>) -> Self
    { Self { registered, local: Vec::new() } }

    pub fn get(&self, id: usize) -> Option<&TypeDescriptor>
    {
        if id < self.registered.len() { self.registered.get(id) }
        else { self.local.get(id - self.registered.len()) }
    }

    pub(super) fn len(&self) -> usize { self.registered.len() + self.local.len() }

    pub(super) fn push(&mut self, descriptor: TypeDescriptor) { self.local.push(descriptor); }
}

impl Index<usize> for TypeTable
{
    type Output = TypeDescriptor;

    fn index(&self, id: usize) -> &Self::Output { self.get(id).unwrap() }
}

impl IndexMut<usize> for TypeTable
{
    fn index_mut(&mut self, id: usize) -> &mut Self::Output
    {
        // Registered descriptors cannot be changed by analysis.
        &mut self.local[id - self.registered.len()]
    }
}


#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemanticFact
{
    pub span: SourceSpan,
    pub kind: SymbolKind,
    pub type_id: Option<usize>,
}


pub struct SemanticAnalysis
{
    pub source: EditorAnalysis,
    pub facts: Vec<SemanticFact>,
    // Includes provisional local types; IDs in facts index this owned table.
    pub types: TypeTable,
}


/**
 * Resolve understood references against existing information. This is source tooling,
 * not type checking: uncertain types and names remain unclassified, without diagnostics.
 */
pub fn analyze_semantics(source: &str, snapshot: &SymbolSnapshot) -> SemanticAnalysis
{
    let (analysis, statements) = analyze_editor_tree_with_tab_size(source, snapshot.tab_width);
    let (facts, types) = semantic::classify(source, &analysis, &statements, snapshot);
    SemanticAnalysis { source: analysis, facts, types }
}


// Whole-file frontends can use a larger work budget without changing REPL limits.
pub fn analyze_semantics_with_limits(source: &str, snapshot: &SymbolSnapshot,
                                    max_bytes: usize, max_steps: usize) -> SemanticAnalysis
{
    let (analysis, statements) = analyze_editor_tree_configured(
        source, max_bytes, max_steps, snapshot.tab_width);
    let (facts, types) = semantic::classify(source, &analysis, &statements, snapshot);
    SemanticAnalysis { source: analysis, facts, types }
}
