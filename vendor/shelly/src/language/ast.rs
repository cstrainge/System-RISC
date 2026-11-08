
use std::cell::{ Cell, RefCell };

use crate::language::{ text::location::Location, data::{ value::Value, types::TypeId } };

#[derive(Debug)]
pub enum AstError
{
    ExpressionNotString(Location)
}


pub struct AstSymbol
{
    pub name: String
}


impl AstSymbol
{
    pub fn is_glob(&self) -> bool
    {
        self.name.contains(['*', '?', '['])
    }
}


pub struct AstLiteral
{
    pub value: Value
}


pub enum AstMathOperator
{
    Add,
    Subtract,
    Multiply,
    Divide,
    Modulo
}


pub enum AstBooleanOperator
{
    Equal,
    NotEqual,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
    And,
    Or
}


pub enum AstExpressionKind
{
    // Same-line `name ()`: the checker resolves the type/command ambiguity.
    SpacedEmptyCall(String),
    EnumVariant(String, String),
    StructConstructor(Box<AstStructConstructor>),
    // A field name beginning with '$' resolves a String variable at runtime.
    Field(Box<AstExpression>, String, Option<usize>),
    Variable(AstSymbol),
    VariableSplat(AstSymbol),
    Array(Vec<AstExpression>),
    HashMap(Vec<(AstExpression, AstExpression)>),
    Range(Option<Box<AstExpression>>, Option<Box<AstExpression>>, bool),
    Index(Box<AstExpression>, Box<AstExpression>),
    Splat(Box<AstExpression>),
    Symbol(AstSymbol),
    Literal(AstLiteral),
    Grouped(Box<AstExpression>),
    // Preserve the referenced value without implicitly invoking it as an argument.
    ExecutableReference(Box<AstExpression>),
    Execute(Box<AstExecuteStatement>),
    Redirect(Box<AstExpression>, Vec<AstRedirection>),
    Pipeline(Box<AstPipeline>),
    TryExecute(Box<AstExpression>),
    MathExpression(AstMathOperator, Box<AstExpression>, Box<AstExpression>),
    BooleanExpression(AstBooleanOperator, Box<AstExpression>, Box<AstExpression>),
    BooleanNot(Box<AstExpression>),
    MathNegate(Box<AstExpression>),
    // The binding checker records explicit collection constraints for conversion lookup.
    TypeConversion(String, Box<AstExpression>, Option<TypeId>, Cell<Option<TypeId>>),
    IfExpression(Box<AstIfExpression>),
    MatchExpression(Box<AstMatchExpression>),
    AnonymousFunction(Box<AstFunctionStatement>)
}


pub enum AstStringFlag
{
    Interpolated(Vec<usize>),
    NonInterpolated,
}


pub struct AstExpression
{
    pub location: Location,
    pub kind: AstExpressionKind,
    pub dispatch_type: Cell<Option<TypeId>>,
    pub interpolation_members: RefCell<Vec<AstExpression>>,
    pub string_flag: Option<AstStringFlag>
}


pub fn new_ast_variable(location: Location, name: String) -> AstExpression
{
    AstExpression
        {
            location: location.clone(),
            kind: AstExpressionKind::Variable(AstSymbol
                {
                    name
                }),
            interpolation_members: Default::default(),
            dispatch_type: Default::default(),
            string_flag:None
        }
}


pub fn new_ast_variable_splat(location: Location, name: String) -> AstExpression
{
    AstExpression
        {
            location: location.clone(),
            kind: AstExpressionKind::VariableSplat(AstSymbol
                {
                    name
                }),
            interpolation_members: Default::default(),
            dispatch_type: Default::default(),
            string_flag:None
        }
}


pub fn new_ast_symbol(location: Location, name: String,
                      string_flag: Option<AstStringFlag>) -> AstExpression
{
    AstExpression
        {
            location: location.clone(),
            kind: AstExpressionKind::Symbol(AstSymbol
                {
                    name
                }),
            interpolation_members: Default::default(),
            dispatch_type: Cell::new(None),
            string_flag
        }
}


pub fn new_ast_literal(location: Location,
                       value: Value,
                       string_flag: Option<AstStringFlag>) -> AstExpression
{
    AstExpression
        {
            location: location.clone(),
            kind: AstExpressionKind::Literal(AstLiteral
                {
                    value
                }),
            interpolation_members: Default::default(),
            dispatch_type: Cell::new(None),
            string_flag
        }
}


impl AstExpression
{
    pub fn uses_pipeline_input(&self) -> bool
    {
        match &self.kind
        {
            AstExpressionKind::Variable(item) | AstExpressionKind::VariableSplat(item) =>
                item.name == "$",
            AstExpressionKind::Execute(call) => call.executable.uses_pipeline_input()
                || call.arguments.iter().any(Self::uses_pipeline_input),
            AstExpressionKind::Array(items) => items.iter().any(Self::uses_pipeline_input),
            AstExpressionKind::HashMap(items) => items.iter().any(|(key, value)|
                key.uses_pipeline_input() || value.uses_pipeline_input()),
            AstExpressionKind::StructConstructor(item) => item.fields.iter()
                .any(|(_, _, value)| value.uses_pipeline_input()),
            AstExpressionKind::Index(a, b) | AstExpressionKind::MathExpression(_, a, b)
            | AstExpressionKind::BooleanExpression(_, a, b) =>
                a.uses_pipeline_input() || b.uses_pipeline_input(),
            AstExpressionKind::Field(inner, _, _) | AstExpressionKind::Splat(inner)
            | AstExpressionKind::Grouped(inner) | AstExpressionKind::ExecutableReference(inner)
            | AstExpressionKind::TryExecute(inner) | AstExpressionKind::MathNegate(inner)
            | AstExpressionKind::BooleanNot(inner)
            | AstExpressionKind::TypeConversion(_, inner, _, _) => inner.uses_pipeline_input(),
            AstExpressionKind::Range(start, end, _) => start.iter().chain(end.iter())
                .any(|value| value.uses_pipeline_input()),
            AstExpressionKind::Redirect(inner, redirects) => inner.uses_pipeline_input()
                || redirects.iter().any(|item| item.target.uses_pipeline_input()),
            AstExpressionKind::IfExpression(item) => item.branches.iter().any(|branch|
                branch.condition.uses_pipeline_input() || branch.body.uses_pipeline_input())
                || item.else_body.as_ref().is_some_and(AstBlockStatement::uses_pipeline_input),
            AstExpressionKind::MatchExpression(item) => item.value.uses_pipeline_input()
                || item.arms.iter().any(|arm|
                    arm.pattern.as_ref().is_some_and(Self::uses_pipeline_input)
                        || arm.body.uses_pipeline_input()),
            // A nested pipeline supplies its own input after its source expression.
            AstExpressionKind::Pipeline(item) => item.stages[0].uses_pipeline_input(),
            _ => false,
        }
    }

    pub fn resolve_as_text(&self) -> Result<String, AstError>
    {
        match &self.kind
        {
            AstExpressionKind::Symbol(symbol) => Ok(symbol.name.clone()),
            AstExpressionKind::EnumVariant(_, _) =>
                Err(AstError::ExpressionNotString(self.location.clone())),
            AstExpressionKind::Variable(variable) => Ok(variable.name.clone()),
            AstExpressionKind::VariableSplat(variable) => Ok(variable.name.clone()),
            AstExpressionKind::Literal(literal) => Ok(literal.value.as_text()),
            AstExpressionKind::Grouped(_)
            | AstExpressionKind::AnonymousFunction(_)
            | AstExpressionKind::Array(_)
            | AstExpressionKind::HashMap(_)
            | AstExpressionKind::Range(_, _, _)
            | AstExpressionKind::StructConstructor(_)
            | AstExpressionKind::SpacedEmptyCall(_)
            | AstExpressionKind::Field(_, _, _)
            | AstExpressionKind::Index(_, _)
            | AstExpressionKind::Splat(_)
            | AstExpressionKind::ExecutableReference(_)
            | AstExpressionKind::Execute(_)
            | AstExpressionKind::Redirect(_, _)
            | AstExpressionKind::Pipeline(_)
            | AstExpressionKind::TryExecute(_)
            | AstExpressionKind::MathExpression(_, _, _)
            | AstExpressionKind::BooleanExpression(_, _, _)
            | AstExpressionKind::BooleanNot(_)
            | AstExpressionKind::MathNegate(_)
            | AstExpressionKind::TypeConversion(_, _, _, _)
            | AstExpressionKind::IfExpression(_)
            | AstExpressionKind::MatchExpression(_) =>
                Err(AstError::ExpressionNotString(self.location.clone()))
        }
    }
}


#[derive(PartialEq, Eq)]
pub enum AstExportFlag
{
    Exported,
    NonExported,
}


pub struct AstLetStatement
{
    pub public: bool,
    pub location: Location,
    pub export_flag: AstExportFlag,
    pub identifier: String,
    pub annotation: Option<AstType>,
    pub type_id: Option<TypeId>,
    pub default_initialize: bool,
    pub deferred_initialize: bool,
    // Retype an existing binding and retain its external representation.
    pub retype: bool,
    pub expression: AstExpression,
}


pub struct AstDestructureStatement
{
    pub public: bool,
    pub location: Location,
    pub export_flag: AstExportFlag,
    pub bindings: Vec<(Location, Option<String>)>,
    pub expression: AstExpression,
}


pub struct AstExecuteStatement
{
    // Only `pub visible` publishes a native/scripted reference through this call.
    pub public: bool,
    pub location: Location,
    pub executable: AstExpression,
    pub expand_path: bool,
    pub arguments: Vec<AstExpression>
}


#[derive(Clone, Copy)]
pub enum RedirectStream
{
    Output,
    Error,
    Both
}


pub struct AstRedirection
{
    pub location: Location,
    pub stream: RedirectStream,
    pub target: AstExpression
}


pub struct AstPipeBoundary
{
    pub location: Location,
    pub layout: crate::language::text::layout::LayoutPosition,
    pub decode: Option<String>,
    // Resolved against registered formats and visible type names at the boundary.
    pub annotation: Option<String>,
    pub type_id: Option<TypeId>,
}


pub struct AstPipeline
{
    pub stages: Vec<AstExpression>,
    pub boundaries: Vec<AstPipeBoundary>,
    // Each boundary feeds stage index + 1 from this earlier producer.
    pub parents: Vec<usize>,
    pub primary_len: usize,
    pub layout_error: Option<(Location, String)>,
}


pub struct AstSetStatement
{
    pub location: Location,
    pub identifier: String,
    pub indexes: Vec<AstAccess>,
    pub expression: AstExpression,
}


pub struct AstAliasArgument
{
    pub value: Value,
    pub expand_path: bool
}


pub struct AstAliasStatement
{
    pub public: bool,
    pub location: Location,
    pub alias: String,
    pub target: String,
    pub arguments: Vec<AstAliasArgument>
}


pub struct AstParameter
{
    pub location: Location,
    pub name: String,
    pub annotation: Option<AstType>,
    pub optional: bool,
    pub variadic: bool,
    pub type_id: Option<TypeId>
}


pub struct AstFunctionStatement
{
    pub public: bool,
    // Evaluated before registration; excluded functions never reach type checking.
    pub condition: Option<AstExpression>,
    pub location: Location,
    pub name: String,
    pub receiver: Option<String>,
    pub receiver_type: Option<TypeId>,
    pub parameters: Vec<AstParameter>,
    pub return_annotation: Option<AstType>,
    pub return_type: Option<TypeId>,
    pub body: AstTopLevel
}


pub struct AstReturnStatement
{
    pub location: Location,
    pub expression: Option<AstExpression>
}


pub fn new_ast_let_statement(
    location: Location,
    export_flag: AstExportFlag,
    identifier: String,
    expression: AstExpression,
) -> Option<AstStatement>
{
    Some(AstStatement::LetStatement(Box::new(AstLetStatement
        {
            public: false,
            location,
            export_flag,
            identifier,
            annotation: None,
            type_id: None,
            default_initialize: false,
            deferred_initialize: false,
            retype: false,
            expression,
        })))
}


pub fn new_ast_set_statement(location: Location,
                             identifier: String,
                             indexes: Vec<AstAccess>,
                             expression: AstExpression) -> Option<AstStatement>
{
    Some(AstStatement::SetStatement(Box::new(AstSetStatement
        {
            location,
            identifier,
            indexes,
            expression,
        })))
}


pub fn new_ast_execute_statement(location: Location,
                                 expand_path: bool,
                                 executable: AstExpression,
                                 arguments: Vec<AstExpression>) -> Option<AstStatement>
{
    Some(AstStatement::ExecuteStatement(Box::new(AstExecuteStatement
        {
            public: false,
            location,
            executable,
            expand_path,
            arguments,
        })))
}


pub fn new_ast_alias_statement(location: Location,
                               alias: String,
                               target: String,
                               arguments: Vec<AstAliasArgument>) -> Option<AstStatement>
{
    Some(AstStatement::AliasStatement(Box::new(AstAliasStatement
        {
            public: false,
            location,
            alias,
            target,
            arguments,
        })))
}


pub struct AstBlockStatement
{
    // An annotated block contributes statements to its parent instead of creating a scope.
    pub condition: Option<AstExpression>,
    pub location: Location,
    pub body: AstTopLevel
}


impl AstBlockStatement
{
    fn uses_pipeline_input(&self) -> bool
    {
        self.condition.as_ref().is_some_and(AstExpression::uses_pipeline_input)
            || self.body.iter().any(AstStatement::uses_pipeline_input)
    }
}


pub struct AstIfBranch
{
    pub condition: AstExpression,
    pub body: AstBlockStatement
}


pub struct AstIfExpression
{
    pub location: Location,
    pub branches: Vec<AstIfBranch>,
    pub else_body: Option<AstBlockStatement>
}


pub struct AstMatchArm
{
    // None denotes the bare wildcard, not a value expression.
    pub pattern: Option<AstExpression>,
    pub body: AstBlockStatement
}


pub struct AstMatchExpression
{
    pub location: Location,
    pub value: AstExpression,
    pub arms: Vec<AstMatchArm>
}


pub struct AstForStatement
{
    pub location: Location,
    // A discard has no name, but still occupies one position in a destructuring pattern.
    pub bindings: Vec<Option<String>>,
    pub destructure: bool,
    pub iterable: AstExpression,
    pub body: AstBlockStatement
}


pub struct AstConditionalLoopStatement
{
    pub condition: AstExpression,
    pub body: AstBlockStatement,
    pub until: bool
}


pub enum AstType
{
    Function(Vec<AstType>, Box<AstType>),
    ReceiverMember(String),
    Named(String),
    Array(Box<AstType>),
    FixedArray(Vec<AstType>),
    Map(Box<AstType>, Box<AstType>),
    Union(Vec<AstType>)
}


impl AstType
{
    pub fn uses_receiver(&self) -> bool
    {
        match self
        {
            Self::ReceiverMember(_) => true,
            Self::Array(item) => item.uses_receiver(),
            Self::Map(key, value) => key.uses_receiver() || value.uses_receiver(),
            Self::FixedArray(items) | Self::Union(items) => items.iter().any(Self::uses_receiver),
            Self::Function(args, result) => args.iter().any(Self::uses_receiver)
                || result.uses_receiver(),
            Self::Named(_) => false,
        }
    }
}

pub struct AstFieldDeclaration
{
    pub public: bool,
    pub name: String,
    pub annotation: AstType,
    pub location: Location
}

pub struct AstStructDeclaration
{
    pub public: bool,
    pub error: bool,
    pub name: String,
    pub fields: Vec<AstFieldDeclaration>,
    pub location: Location,
}

pub struct AstTypeDeclaration
{
    // Resolved default factory; explicit declarations with its name take precedence.
    pub conversion: Option<Box<AstFunctionStatement>>,
    pub public: bool,
    pub name: String,
    pub annotation: AstType,
    pub location: Location,
}

pub struct AstStructConstructor
{
    pub raw: bool,
    pub name: String,
    pub fields: Vec<(String, Location, AstExpression)>,
    pub type_id: Option<TypeId>,
    pub field_indexes: Vec<usize>
}

pub enum AstAccess
{
    Index(AstExpression),
    // As with expression fields, '$' marks a dynamic String variable name.
    Field(String)
}


pub struct AstEnumDeclaration
{
    pub public: bool,
    pub location: Location,
    pub name: String,
    pub variants: Vec<(String, Location)>
}


pub struct AstImportStatement
{
    pub public: bool,
    pub location: Location,
    pub module: String,
    pub names: Vec<String>,
    pub condition: Option<AstExpression>,
    pub enabled: bool,
}

pub struct AstCatch
{
    pub location: Location,
    pub stack: Option<String>,
    pub error: String,
    pub annotation: Option<AstType>,
    pub type_id: Option<TypeId>,
    pub body: AstBlockStatement,
}

pub struct AstTry
{
    pub location: Location,
    pub body: AstBlockStatement,
    pub catches: Vec<AstCatch>,
    pub finally: Option<AstBlockStatement>,
}

pub enum AstStatement
{
    TryStatement(Box<AstTry>),
    ThrowStatement(AstExpression),
    ImportStatement(Box<AstImportStatement>),
    EnumDeclaration(Box<AstEnumDeclaration>),
    StructDeclaration(Box<AstStructDeclaration>),
    TypeDeclaration(Box<AstTypeDeclaration>),
    LetStatement(Box<AstLetStatement>),
    DestructureStatement(Box<AstDestructureStatement>),
    DiscardStatement(AstExpression),
    SetStatement(Box<AstSetStatement>),
    AliasStatement(Box<AstAliasStatement>),
    ExecuteStatement(Box<AstExecuteStatement>),
    ExpressionStatement(AstExpression),
    ReturnStatement(Box<AstReturnStatement>),
    FunctionDefinition(Box<AstFunctionStatement>),
    BlockStatement(Box<AstBlockStatement>),
    ForStatement(Box<AstForStatement>),
    LoopStatement(Box<AstBlockStatement>),
    ConditionalLoopStatement(Box<AstConditionalLoopStatement>),
    BreakStatement(Location),
    ContinueStatement(Location),
    NullStatement,
}


pub type AstTopLevel = Vec<AstStatement>;


impl AstStatement
{
    fn uses_pipeline_input(&self) -> bool
    {
        match self
        {
            Self::TryStatement(item) => item.body.uses_pipeline_input()
                || item.catches.iter().any(|catch| catch.body.uses_pipeline_input())
                || item.finally.as_ref().is_some_and(AstBlockStatement::uses_pipeline_input),
            Self::ThrowStatement(item) => item.uses_pipeline_input(),
            Self::LetStatement(item) => item.expression.uses_pipeline_input(),
            Self::DestructureStatement(item) => item.expression.uses_pipeline_input(),
            Self::SetStatement(item) => item.expression.uses_pipeline_input()
                || item.indexes.iter().any(|access| match access
                    { AstAccess::Index(value) => value.uses_pipeline_input(), _ => false }),
            Self::DiscardStatement(value) | Self::ExpressionStatement(value) =>
                value.uses_pipeline_input(),
            Self::ExecuteStatement(item) => item.executable.uses_pipeline_input()
                || item.arguments.iter().any(AstExpression::uses_pipeline_input),
            Self::ReturnStatement(item) => item.expression.as_ref()
                .is_some_and(AstExpression::uses_pipeline_input),
            Self::BlockStatement(item) | Self::LoopStatement(item) => item.uses_pipeline_input(),
            Self::ForStatement(item) => item.iterable.uses_pipeline_input()
                || item.body.uses_pipeline_input(),
            Self::ConditionalLoopStatement(item) => item.condition.uses_pipeline_input()
                || item.body.uses_pipeline_input(),
            _ => false,
        }
    }

    pub fn module_declaration(&self) -> Option<(&str, &Location, bool)>
    {
        Some(match self
            {
                Self::LetStatement(item) if !item.retype || item.public =>
                    (&item.identifier, &item.location, item.public),
                Self::FunctionDefinition(item) => (&item.name, &item.location, item.public),
                Self::StructDeclaration(item) => (&item.name, &item.location, item.public),
                Self::EnumDeclaration(item) => (&item.name, &item.location, item.public),
                Self::TypeDeclaration(item) => (&item.name, &item.location, item.public),
                Self::AliasStatement(item) => (&item.alias, &item.location, item.public),
                Self::ImportStatement(item) => (&item.module, &item.location, item.public),
                Self::ExecuteStatement(item) if item.public
                    => ("", &item.location, true),
                _ => return None,
            })
    }

    pub fn module_declarations(&self) -> Vec<(&str, &Location, bool)>
    {
        if let Self::DestructureStatement(item) = self
        {
            return item.bindings.iter().filter_map(|(location, name)|
                name.as_deref().map(|name| (name, location, item.public))).collect();
        }
        self.module_declaration().into_iter().collect()
    }
}
