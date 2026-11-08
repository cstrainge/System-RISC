
use std::collections::BTreeMap;

use crate::language::{ analysis::{ SemanticFact, SymbolDescriptor, SymbolKind, SymbolSnapshot,
                                   TypeDescriptor, TypeTable },
                       ast::{ AstBlockStatement, AstExpression, AstExpressionKind,
                              AstFunctionStatement, AstStatement, AstTopLevel, AstType },
                       data::conversion::is_conversion_method,
                       parser::{ EditorAnalysis, source::{ SyntaxFact, SyntaxRole } },
                       text::{ interpolation::interpolation_references,
                               location::Location, span::SourceSpan },
                       tokenizer::{ Token, TokenKind, source::StringPartKind } };


struct Classifier<'a>
{
    source: &'a str,
    analysis: &'a EditorAnalysis,
    snapshot: &'a SymbolSnapshot,
    scopes: Vec<BTreeMap<String, SymbolDescriptor>>,
    types: TypeTable,
    facts: BTreeMap<usize, SemanticFact>,
    locations: BTreeMap<(usize, usize), usize>,
    depth: usize,
    methods: Vec<BTreeMap<(usize, String), SymbolDescriptor>>,
    properties: Vec<BTreeMap<(usize, String), SymbolDescriptor>>,
    factory: Option<usize>,
    regions: BTreeMap<usize, SourceSpan>,
    roles: BTreeMap<usize, SyntaxRole>,
    strings: BTreeMap<usize, usize>,
}


#[derive(Clone, Copy, Default)]
struct ExpressionInfo
{
    type_id: Option<usize>,
    result: Option<usize>,
    end: usize,
}


pub(super) fn classify(source: &str, analysis: &EditorAnalysis, statements: &AstTopLevel,
                       snapshot: &SymbolSnapshot) -> (Vec<SemanticFact>, TypeTable)
{
    let mut classifier = Classifier
        {
            source, analysis, snapshot, scopes: vec![BTreeMap::new()],
            types: TypeTable::new(snapshot.types.clone()), facts: BTreeMap::new(), depth: 0,
            methods: vec![BTreeMap::new()], properties: vec![BTreeMap::new()], factory: None,
            regions: region_spans(analysis),
            roles: analysis.syntax.facts.iter().map(|fact| (fact.span.start, fact.role)).collect(),
            strings: analysis.lexical.strings.iter().enumerate()
                .map(|(index, string)| (string.span.start, index)).collect(),
            locations: analysis.tokens.iter().enumerate().map(|(index, token)|
                ((token.location.line, token.location.column), index)).collect(),
        };
    classifier.paint(&analysis.syntax.facts);
    for string in &analysis.lexical.strings { classifier.string_references(string.span); }
    classifier.statements(statements);
    (classifier.facts.into_values().collect(), classifier.types)
}


// A sweep avoids scanning every statement region for every reference in a large paste.
fn region_spans(analysis: &EditorAnalysis) -> BTreeMap<usize, SourceSpan>
{
    let mut regions: Vec<SourceSpan> = analysis.regions.iter().map(|region| region.span).collect();
    regions.sort_by_key(|span| (span.start, usize::MAX - span.end));
    let mut next = 0;
    let mut active: Vec<SourceSpan> = Vec::new();
    let mut spans = BTreeMap::new();
    for token in &analysis.tokens
    {
        while next < regions.len() && regions[next].start <= token.span.start
        { active.push(regions[next]); next += 1; }
        active.retain(|span| span.end >= token.span.end);
        if let Some(span) = active.iter().min_by_key(|span| span.end - span.start)
        { spans.insert(token.span.start, *span); }
    }
    spans
}


impl Classifier<'_>
{
    fn symbol(&self, name: &str) -> Option<SymbolDescriptor>
    {
        self.scopes.iter().rev().find_map(|scope| scope.get(name))
            .or_else(|| self.snapshot.symbols.get(name)).cloned()
    }

    fn type_name(&self, name: &str) -> Option<usize>
    {
        self.symbol(name).filter(|symbol| symbol.kind == SymbolKind::Type)
            .and_then(|symbol| symbol.type_id)
    }

    fn enter_scope(&mut self)
    {
        self.scopes.push(BTreeMap::new());
        self.methods.push(BTreeMap::new());
        self.properties.push(BTreeMap::new());
    }

    fn exit_scope(&mut self)
    {
        self.scopes.pop();
        self.methods.pop();
        self.properties.pop();
    }

    fn bind(&mut self, name: &str, symbol: SymbolDescriptor)
    { self.scopes.last_mut().unwrap().insert(name.into(), symbol); }

    fn mark(&mut self, span: SourceSpan, symbol: &SymbolDescriptor)
    {
        self.facts.insert(span.start, SemanticFact
            {
                span, kind: symbol.kind, type_id: symbol.type_id
            });
    }

    fn token(&self, location: &Location) -> Option<&Token>
    {
        self.locations.get(&(location.line, location.column))
            .and_then(|index| self.analysis.tokens.get(*index))
    }

    fn qualified(&self, span: SourceSpan) -> (String, Vec<SourceSpan>)
    {
        let tokens = &self.analysis.tokens;
        let start = tokens.partition_point(|token| token.span.start < span.start);
        let mut name = span.slice(self.source).unwrap_or_default().to_string();
        let mut spans = vec![span];
        let mut index = start + 1;
        while    tokens.get(index).is_some_and(|token| token.kind == TokenKind::Scope)
              && tokens.get(index + 1).is_some_and(|token| token.kind == TokenKind::Symbol)
        {
            let token = &tokens[index + 1];
            name.push_str("::");
            name.push_str(&token.token_value_text());
            spans.push(token.span);
            index += 2;
        }
        (name, spans)
    }

    fn mark_reference(&mut self, spans: &[SourceSpan], symbol: &SymbolDescriptor)
    {
        let mut prefix = String::new();
        for (index, span) in spans.iter().enumerate()
        {
            if index > 0 { prefix.push_str("::"); }
            prefix.push_str(span.slice(self.source).unwrap_or_default());
            if index + 1 == spans.len() { self.mark(*span, symbol); }
            else if let Some(namespace) = self.symbol(&prefix)
            { self.mark(*span, &namespace); }
            else
            {
                let prefix = format!("{}::", prefix);
                if self.snapshot.symbols.range(prefix.clone()..).next()
                    .is_some_and(|(name, _)| name.starts_with(&prefix))
                { self.mark(*span, &SymbolDescriptor::new(SymbolKind::Module, None)); }
            }
        }
    }

    fn paint(&mut self, facts: &[SyntaxFact])
    {
        for fact in facts
        {
            let raw = fact.span.slice(self.source).unwrap_or_default();
            let symbol = match fact.role
            {
                SyntaxRole::VariableDeclaration | SyntaxRole::ParameterDeclaration =>
                    Some(SymbolDescriptor::new(SymbolKind::Variable, None)),
                SyntaxRole::FunctionDeclaration =>
                    Some(SymbolDescriptor::new(SymbolKind::Function, None)),
                SyntaxRole::AliasDeclaration =>
                    Some(SymbolDescriptor::new(SymbolKind::Alias, None)),
                SyntaxRole::TypeDeclaration =>
                    Some(SymbolDescriptor::new(SymbolKind::Type, self.type_name(raw))),
                SyntaxRole::EnumVariantDeclaration =>
                    Some(SymbolDescriptor::new(SymbolKind::EnumVariant, None)),
                SyntaxRole::Variable => self.symbol(raw),
                SyntaxRole::Member if raw.starts_with(".$") || raw.starts_with(".::$") =>
                    Some(self.symbol(raw.strip_prefix(".::").unwrap_or(&raw[1..]))
                        .unwrap_or_else(|| SymbolDescriptor::new(SymbolKind::Variable, None))),
                SyntaxRole::Name | SyntaxRole::CommandName | SyntaxRole::TypeName
                | SyntaxRole::ImportName | SyntaxRole::ModuleName | SyntaxRole::PipeAnnotation =>
                    {
                        let (name, spans) = self.qualified(fact.span);
                        if let Some(symbol) = self.symbol(&name)
                        {
                            self.mark_reference(&spans, &symbol);
                            None
                        }
                        else if    fact.role == SyntaxRole::CommandName
                                && self.snapshot.external_commands.contains(&name)
                        { Some(SymbolDescriptor::new(SymbolKind::ExternalCommand, None)) }
                        else
                        {
                            if let Some(prefix) = self.symbol(raw)
                                .filter(|symbol|
                                    matches!(symbol.kind, SymbolKind::Module | SymbolKind::Type))
                            { self.mark(fact.span, &prefix); }
                            None
                        }
                    },
                _ => None,
            };
            if let Some(symbol) = symbol { self.mark(fact.span, &symbol); }
        }
    }

    fn paint_region(&mut self, location: &Location)
    {
        let Some(token) = self.token(location) else { return; };
        let Some(region) = self.regions.get(&token.span.start) else { return; };
        let facts = &self.analysis.syntax.facts;
        let start = facts.partition_point(|fact| fact.span.start < region.start);
        let end = facts.partition_point(|fact| fact.span.start < region.end);
        self.paint(&facts[start..end]);
    }

    fn add_type(&mut self, descriptor: TypeDescriptor) -> usize
    {
        let id = self.types.len();
        self.types.push(descriptor);
        id
    }

    fn container(&mut self, name: &str, item: Option<usize>, key: Option<usize>) -> usize
    {
        let parent = self.type_name(name);
        self.add_type(TypeDescriptor
            {
                name: name.into(), parent, item, key,
                ..TypeDescriptor::default()
            })
    }

    fn annotation(&mut self, annotation: &AstType) -> Option<usize>
    {
        match annotation
        {
            AstType::Named(name) => self.type_name(name),
            AstType::Array(item) =>
                { let item = self.annotation(item); Some(self.container("Array", item, None)) },
            AstType::Map(key, item) =>
                {
                    let key = self.annotation(key);
                    let item = self.annotation(item);
                    Some(self.container("HashMap", item, key))
                },
            AstType::Function(_, result) =>
                {
                    let result = self.annotation(result);
                    Some(self.add_type(TypeDescriptor
                        {
                            name: "Function".into(),
                            call_result: result, ..TypeDescriptor::default()
                        }))
                },
            // A union does not prove which member is the live value while editing.
            _ => None,
        }
    }

    fn statements(&mut self, statements: &AstTopLevel)
    {
        // Declarations that compilation registers ahead of bodies are provisional here.
        for statement in statements
        {
            let name = match statement
                {
                    AstStatement::TypeDeclaration(item) => Some(item.name.as_str()),
                    AstStatement::StructDeclaration(item) => Some(item.name.as_str()),
                    AstStatement::EnumDeclaration(item) => Some(item.name.as_str()),
                    _ => None,
                };
            if let Some(name) = name
            {
                let id = self.add_type(TypeDescriptor
                    {
                        name: name.into(),
                        ..TypeDescriptor::default()
                    });
                self.bind(name, SymbolDescriptor::new(SymbolKind::Type, Some(id)));
            }
        }
        for statement in statements
        {
            match statement
            {
                AstStatement::TypeDeclaration(item) =>
                    {
                        let parent = self.annotation(&item.annotation);
                        let id = self.type_name(&item.name).unwrap();
                        self.types[id].parent = parent;
                    },
                AstStatement::StructDeclaration(item) =>
                    {
                        let id = self.type_name(&item.name).unwrap();
                        for field in &item.fields
                        {
                            let ty = self.annotation(&field.annotation);
                            self.types[id].fields.insert(field.name.clone(),
                                SymbolDescriptor::new(SymbolKind::Field, ty));
                        }
                    },
                AstStatement::EnumDeclaration(item) =>
                    {
                        let id = self.type_name(&item.name).unwrap();
                        self.types[id].variants.extend(item.variants.iter()
                            .map(|(name, _)| name.clone()));
                    },
                AstStatement::FunctionDefinition(function) =>
                    {
                        let result = match &function.return_annotation
                            {
                                Some(annotation) => self.annotation(annotation),
                                None => self.type_name("None"),
                            };
                        let symbol = SymbolDescriptor
                            {
                                kind: SymbolKind::Function,
                                type_id: None, return_type: result
                            };
                        if let Some(receiver) = function.receiver.as_ref()
                            .and_then(|name| self.type_name(name))
                        {
                            let property = if let Some(name) = function.name.strip_prefix("get_")
                                { Some((name, result, true)) }
                                else if let Some(name) = function.name.strip_prefix("set_")
                                {
                                    let ty = function.parameters.get(1)
                                        .and_then(|parameter| parameter.annotation.as_ref())
                                        .and_then(|annotation| self.annotation(annotation));
                                    Some((name, ty, false))
                                }
                                else { None };
                            if let Some((name, ty, getter)) = property
                            {
                                let entry = self.properties.last_mut().unwrap()
                                    .entry((receiver, name.into()));
                                let symbol = SymbolDescriptor::new(SymbolKind::Field, ty);
                                if getter { entry.insert_entry(symbol); }
                                else { entry.or_insert(symbol); }
                            }
                            self.methods.last_mut().unwrap()
                                .insert((receiver, function.name.clone()),
                                SymbolDescriptor { kind: SymbolKind::Method, ..symbol });
                        }
                        else if function.receiver.is_none() { self.bind(&function.name, symbol); }
                    },
                AstStatement::AliasStatement(item) =>
                    self.bind(&item.alias, SymbolDescriptor::new(SymbolKind::Alias, None)),
                _ => {},
            }
        }
        for statement in statements { self.statement(statement); }
    }

    fn block(&mut self, block: &AstBlockStatement)
    {
        if let Some(condition) = &block.condition
        {
            self.expression(condition, false);
            // The condition is not evaluated. Its declarations are only prospective.
            self.statements(&block.body);
        }
        else
        {
            self.enter_scope();
            self.statements(&block.body);
            self.exit_scope();
        }
    }

    fn function(&mut self, function: &AstFunctionStatement)
    {
        self.paint_region(&function.location);
        if    function.receiver.is_some()
           && let Some(token) = self.token(&function.location)
        { self.mark(token.span, &SymbolDescriptor::new(SymbolKind::Method, None)); }
        if let Some(condition) = &function.condition { self.expression(condition, false); }
        self.enter_scope();
        let previous_factory = self.factory;
        let receiver_bound = function.receiver.is_some() && function.name != "new"
            && !is_conversion_method(&function.name);
        if let Some(name) = &function.receiver
        {
            self.factory = if function.name == "new" { self.type_name(name) } else { None };
            if receiver_bound
            { self.bind("$self", SymbolDescriptor::new(
                SymbolKind::Variable, self.type_name(name))); }
        }
        for parameter in &function.parameters
        {
            // The parser supplies an implicit receiver with the method's source location.
            if receiver_bound && parameter.name == "$self" { continue; }
            let ty = parameter.annotation.as_ref()
                .and_then(|annotation| self.annotation(annotation));
            let ty = if parameter.variadic { Some(self.container("Array", ty, None)) } else { ty };
            let symbol = SymbolDescriptor::new(SymbolKind::Variable, ty);
            self.bind(&parameter.name, symbol.clone());
            if let Some(token) = self.token(&parameter.location) { self.mark(token.span, &symbol); }
        }
        self.statements(&function.body);
        self.factory = previous_factory;
        self.exit_scope();
    }

    fn statement(&mut self, statement: &AstStatement)
    {
        match statement
        {
            AstStatement::LetStatement(item) =>
                {
                    self.paint_region(&item.location);
                    let inferred = self.expression(&item.expression, false).type_id;
                    let ty = match &item.annotation
                        {
                            Some(annotation) => self.annotation(annotation),
                            None => inferred,
                        };
                    let symbol = SymbolDescriptor::new(SymbolKind::Variable, ty);
                    self.bind(&item.identifier, symbol.clone());
                    if let Some(token) = self.token(&item.location)
                    { self.mark(token.span, &symbol); }
                },
            AstStatement::DestructureStatement(item) =>
                {
                    self.paint_region(&item.location);
                    let mut expression = &item.expression;
                    while let AstExpressionKind::Grouped(inner) = &expression.kind
                    { expression = inner; }
                    // Inspect the initializer before binding names that may shadow its inputs.
                    let inferred: Vec<_> = match &expression.kind
                        {
                            AstExpressionKind::Array(items) if !items.iter().any(|item|
                                matches!(item.kind, AstExpressionKind::Splat(_)
                                    | AstExpressionKind::VariableSplat(_))) =>
                                items.iter().map(|item| self.expression(item, false).type_id)
                                    .collect(),
                            _ =>
                                {
                                    let ty = self.expression(expression, false).type_id;
                                    vec![ty.and_then(|id| self.types[id].item); item.bindings.len()]
                                },
                        };
                    for (index, (location, name)) in item.bindings.iter().enumerate()
                    {
                        let Some(name) = name else { continue; };
                        let symbol = SymbolDescriptor::new(SymbolKind::Variable,
                            inferred.get(index).copied().flatten());
                        self.bind(name, symbol.clone());
                        if let Some(token) = self.token(location)
                        { self.mark(token.span, &symbol); }
                    }
                },
            AstStatement::SetStatement(item) =>
                { self.paint_region(&item.location); self.expression(&item.expression, false); },
            AstStatement::DiscardStatement(item) | AstStatement::ExpressionStatement(item) =>
                { self.paint_region(&item.location); self.expression(item, true); },
            AstStatement::ExecuteStatement(item) =>
                {
                    self.paint_region(&item.location);
                    self.expression(&item.executable, true);
                    for argument in &item.arguments { self.expression(argument, false); }
                },
            AstStatement::ReturnStatement(item) =>
                {
                    self.paint_region(&item.location);
                    if let Some(value) = &item.expression { self.expression(value, false); }
                },
            AstStatement::ThrowStatement(item) =>
                { self.paint_region(&item.location); self.expression(item, false); },
            AstStatement::TryStatement(item) =>
                {
                    self.paint_region(&item.location);
                    self.enter_scope();
                    self.statements(&item.body.body);
                    self.exit_scope();
                    for catch in &item.catches
                    {
                        self.enter_scope();
                        if let Some(name) = &catch.stack
                        { self.bind(name, SymbolDescriptor::new(SymbolKind::Variable,
                            self.type_name("CallStack"))); }
                        let ty = catch.annotation.as_ref().and_then(|ty| self.annotation(ty));
                        self.bind(&catch.error, SymbolDescriptor::new(SymbolKind::Variable, ty));
                        self.statements(&catch.body.body);
                        self.exit_scope();
                    }
                    if let Some(finally) = &item.finally
                    {
                        self.enter_scope();
                        self.statements(&finally.body);
                        self.exit_scope();
                    }
                },
            AstStatement::FunctionDefinition(item) => self.function(item),
            AstStatement::BlockStatement(item) | AstStatement::LoopStatement(item) =>
                self.block(item),
            AstStatement::ConditionalLoopStatement(item) =>
                { self.expression(&item.condition, false); self.block(&item.body); },
            AstStatement::ForStatement(item) =>
                {
                    let iterable = self.expression(&item.iterable, false).type_id;
                    let ty = iterable.and_then(|id| self.types[id].item.or_else(||
                        {
                            if self.types[id].name == "Range" { self.type_name("Integer") }
                            else { None }
                        }));
                    self.enter_scope();
                    for name in item.bindings.iter().flatten()
                    { self.bind(name, SymbolDescriptor::new(SymbolKind::Variable,
                        if item.destructure { None } else { ty })); }
                    self.statements(&item.body.body);
                    self.exit_scope();
                },
            AstStatement::ImportStatement(item) =>
                {
                    self.paint_region(&item.location);
                    if let Some(condition) = &item.condition { self.expression(condition, false); }
                    // Selected names are known only for already loaded modules.
                    for name in &item.names
                    {
                        let qualified = if let Some(name) = name.strip_prefix('$')
                            { format!("${}::{}", item.module, name) }
                            else { format!("{}::{}", item.module, name) };
                        if let Some(symbol) = self.snapshot.symbols.get(&qualified)
                        { self.bind(name, symbol.clone()); }
                    }
                },
            AstStatement::TypeDeclaration(item) => self.paint_region(&item.location),
            AstStatement::StructDeclaration(item) => self.paint_region(&item.location),
            AstStatement::EnumDeclaration(item) => self.paint_region(&item.location),
            AstStatement::AliasStatement(item) => self.paint_region(&item.location),
            _ => {},
        }
    }

    fn expression(&mut self, expression: &AstExpression, command: bool) -> ExpressionInfo
    {
        // Left-associative chains can be deeper than the grammar's nesting limit.
        if self.depth >= 64 { return ExpressionInfo::default(); }
        self.depth += 1;
        let info = self.expression_inner(expression, command);
        self.depth -= 1;
        info
    }

    fn expression_inner(&mut self, expression: &AstExpression, command: bool) -> ExpressionInfo
    {
        let span = self.token(&expression.location).map(|token| token.span);
        let mut info = ExpressionInfo
            {
                end: span.map_or(0, |span| span.end),
                ..ExpressionInfo::default()
            };
        match &expression.kind
        {
            AstExpressionKind::Variable(item) | AstExpressionKind::VariableSplat(item)
            | AstExpressionKind::Symbol(item) =>
                {
                    let variable = item.name.starts_with('$');
                    let external = item.name.strip_prefix("::");
                    let expression_name = matches!(expression.kind, AstExpressionKind::Symbol(_))
                        && span.is_some_and(|span|
                            self.roles.get(&span.start) == Some(&SyntaxRole::Name));
                    let symbol = if variable || command || expression_name
                        {
                            if let Some(name) = external
                            {
                                self.snapshot.external_commands.contains(name)
                                    .then(||
                                        SymbolDescriptor::new(SymbolKind::ExternalCommand, None))
                            }
                            else { self.symbol(&item.name) }
                        }
                        else { None };
                    if let Some(symbol) = symbol
                    {
                        info.type_id = if symbol.kind == SymbolKind::Type
                            { self.type_name("Type") } else { symbol.type_id };
                        info.result = symbol.return_type;
                        if    let Some(span) = span
                           && external.is_none()
                        {
                            let (_, spans) = self.qualified(span);
                            self.mark_reference(&spans, &symbol);
                            info.end = spans.last().map_or(info.end, |span| span.end);
                        }
                    }
                    if let (Some(external), Some(span)) = (external, span)
                    {
                        self.facts.remove(&span.start);
                        let next = self.analysis.tokens
                            .partition_point(|token| token.span.start < span.end);
                        if let Some(token) = self.analysis.tokens.get(next)
                        {
                            self.facts.remove(&token.span.start);
                            if self.snapshot.external_commands.contains(external)
                            { self.mark(token.span, &SymbolDescriptor::new(
                                SymbolKind::ExternalCommand, None)); }
                            info.end = token.span.end;
                        }
                    }
                    if let Some(span) = span { self.string_references(span); }
                },
            AstExpressionKind::Literal(item) =>
                {
                    info.type_id = self.type_name(&item.value.type_name());
                    if let Some(span) = span { self.string_references(span); }
                },
            AstExpressionKind::Grouped(inner) | AstExpressionKind::ExecutableReference(inner)
            | AstExpressionKind::Splat(inner) | AstExpressionKind::MathNegate(inner) =>
                info = self.expression(inner, true),
            AstExpressionKind::Field(inner, name, _) =>
                {
                    info = self.expression(inner, true);
                    let start = self.analysis.tokens
                        .partition_point(|token| token.span.start < info.end);
                    let member = self.analysis.tokens[start..].iter()
                        .find(|token| token.kind == TokenKind::Member)
                        .filter(|token| token.token_value_text() == *name).map(|token| token.span);
                    if let Some(span) = member
                    {
                        let raw = name.starts_with("::");
                        let name = name.strip_prefix("::").unwrap_or(name);
                        let symbol = if name.starts_with('$')
                            { Some(self.symbol(name).unwrap_or_else(||
                                SymbolDescriptor::new(SymbolKind::Variable, None))) }
                            else { info.type_id.and_then(|id|
                                if raw { self.storage_member(id, name, 0) }
                                else { self.member(id, name, 0) }) };
                        if let Some(symbol) = symbol
                        {
                            self.mark(span, &symbol);
                            info.type_id = if name.starts_with('$') { None }
                                else { symbol.type_id.or(symbol.return_type) };
                            info.result = if name.starts_with('$') { None }
                                else { symbol.return_type };
                        }
                        else { info.type_id = None; info.result = None; }
                        info.end = span.end;
                    }
                },
            AstExpressionKind::Index(inner, index) =>
                {
                    info = self.expression(inner, false);
                    let index_info = self.expression(index, false);
                    info.end = info.end.max(index_info.end);
                    info.type_id = info.type_id.and_then(|id| self.types[id].item);
                },
            AstExpressionKind::Execute(call) =>
                {
                    info = self.expression(&call.executable, true);
                    for argument in &call.arguments
                    { info.end = info.end.max(self.expression(argument, false).end); }
                    let result = info.result.or_else(|| info.type_id
                        .and_then(|id| self.types[id].call_result));
                    info.type_id = result.or(
                        if call.arguments.is_empty() { info.type_id } else { None });
                },
            AstExpressionKind::TypeConversion(name, value, _, _) =>
                {
                    info = self.expression(value, false);
                    info.type_id = self.type_name(name);
                },
            AstExpressionKind::StructConstructor(item) =>
                {
                    info.type_id = self.type_name(&item.name)
                        .and_then(|id| self.constructor_result(id));
                    for (_, _, value) in &item.fields
                    { info.end = info.end.max(self.expression(value, false).end); }
                },
            AstExpressionKind::EnumVariant(name, variant) =>
                {
                    info.type_id = self.type_name(name);
                    if let Some(id) = info.type_id
                        && !self.types[id].variants.contains(variant)
                        && let Some(method) = self.method(id, variant, 0)
                    {
                        if let Some(span) = span
                        {
                            let (_, spans) = self.qualified(span);
                            self.mark_reference(&spans, &method);
                            info.end = spans.last().map_or(info.end, |span| span.end);
                        }
                        info.type_id = method.return_type;
                        info.result = method.return_type;
                    }
                    else if    let Some(id) = info.type_id
                            .filter(|id| self.types[*id].variants.contains(variant))
                       && let Some(span) = span
                    {
                        let (_, spans) = self.qualified(span);
                        if let Some(last) = spans.last()
                        { self.mark(*last, &SymbolDescriptor::new(
                            SymbolKind::EnumVariant, Some(id))); }
                    }
                },
            AstExpressionKind::Array(items) =>
                {
                    let mut common = None;
                    for (index, item) in items.iter().enumerate()
                    {
                        let child = self.expression(item, false);
                        info.end = info.end.max(child.end);
                        common = if index == 0 { child.type_id }
                            else if common == child.type_id { common }
                            else { self.type_name("any") };
                    }
                    if    items.len() == 1
                       && let Some(id) = self.bare_type(&items[0])
                    { common = Some(id); }
                    info.type_id = Some(self.container("Array", common, None));
                },
            AstExpressionKind::HashMap(items) =>
                {
                    for (key, value) in items
                    {
                        self.expression(key, false);
                        info.end = info.end.max(self.expression(value, false).end);
                    }
                    info.type_id = if items.len() == 1
                        {
                            let key = self.bare_type(&items[0].0);
                            let item = self.bare_type(&items[0].1);
                            if key.is_some() && item.is_some()
                            { Some(self.container("HashMap", item, key)) }
                            else { self.type_name("HashMap") }
                        }
                        else { self.type_name("HashMap") };
                },
            AstExpressionKind::MathExpression(_, left, right)
            | AstExpressionKind::BooleanExpression(_, left, right) =>
                {
                    let left = self.expression(left, false);
                    let right = self.expression(right, false);
                    info.end = left.end.max(right.end);
                    info.type_id = if matches!(expression.kind,
                        AstExpressionKind::BooleanExpression(..))
                        { self.type_name("Boolean") }
                        else if left.type_id == right.type_id { left.type_id } else { None };
                },
            AstExpressionKind::BooleanNot(value) =>
                { info = self.expression(value, false); info.type_id = self.type_name("Boolean"); },
            AstExpressionKind::TryExecute(value) =>
                {
                    info = self.expression(value, true);
                    info.type_id = info.result.or_else(|| info.type_id
                        .and_then(|id| self.types[id].call_result)).or(info.type_id);
                },
            AstExpressionKind::Range(start, end, _) =>
                {
                    for value in start.iter().chain(end.iter()) { self.expression(value, false); }
                    info.type_id = self.type_name("Range");
                },
            AstExpressionKind::Redirect(value, redirects) =>
                {
                    info = self.expression(value, command);
                    for redirect in redirects { self.expression(&redirect.target, false); }
                },
            AstExpressionKind::Pipeline(pipeline) =>
                {
                    self.enter_scope();
                    let mut outputs: Vec<ExpressionInfo> = Vec::new();
                    for (index, stage) in pipeline.stages.iter().enumerate()
                    {
                        if index > 0
                        {
                            let parent = pipeline.parents[index - 1];
                            let ty = outputs[parent].type_id;
                            let ty = if parent == 0 || pipeline.primary_len == pipeline.stages.len()
                                { ty.and_then(|id| self.types[id].item).or(ty) } else { ty };
                            self.bind("$", SymbolDescriptor::new(SymbolKind::Variable, ty));
                        }
                        info = self.expression(stage, true);
                        outputs.push(info);
                    }
                    info = outputs[pipeline.primary_len - 1];
                    self.exit_scope();
                },
            AstExpressionKind::IfExpression(item) =>
                {
                    for branch in &item.branches
                    { self.expression(&branch.condition, false); self.block(&branch.body); }
                    if let Some(block) = &item.else_body { self.block(block); }
                },
            AstExpressionKind::MatchExpression(item) =>
                {
                    self.expression(&item.value, false);
                    for arm in &item.arms
                    {
                        if let Some(pattern) = &arm.pattern { self.expression(pattern, false); }
                        self.block(&arm.body);
                    }
                },
            AstExpressionKind::AnonymousFunction(function) =>
                {
                    self.function(function);
                    let result = match &function.return_annotation
                        {
                            Some(annotation) => self.annotation(annotation),
                            None => self.type_name("None"),
                        };
                    info.type_id = Some(self.add_type(TypeDescriptor
                        {
                            name: "Function".into(),
                            call_result: result, ..TypeDescriptor::default()
                        }));
                },
            AstExpressionKind::SpacedEmptyCall(name) =>
                { info.type_id = self.symbol(name).and_then(|symbol|
                    if symbol.kind == SymbolKind::Type
                    { symbol.type_id.and_then(|id| self.constructor_result(id)) }
                    else { symbol.return_type.or(symbol.type_id) }); },
        }
        info
    }

    fn bare_type(&self, expression: &AstExpression) -> Option<usize>
    {
        match &expression.kind
        {
            AstExpressionKind::Symbol(item) => self.type_name(&item.name),
            AstExpressionKind::EnumVariant(owner, name) =>
                self.type_name(&format!("{}::{}", owner, name)),
            _ => None,
        }
    }

    fn constructor_result(&self, id: usize) -> Option<usize>
    {
        if self.factory == Some(id) { return Some(id); }
        match self.method(id, "new", 0)
        {
            Some(method) => method.return_type,
            None => Some(id),
        }
    }

    fn method(&self, id: usize, name: &str, depth: usize) -> Option<SymbolDescriptor>
    {
        if depth >= 64 { return None; }
        let descriptor = self.types.get(id)?;
        self.methods.iter().rev()
            .find_map(|methods| methods.get(&(id, name.into())).cloned())
            .or_else(|| descriptor.methods.get(name).cloned())
            .or_else(|| descriptor.parent.and_then(|id| self.method(id, name, depth + 1)))
    }

    fn storage_member(&self, id: usize, name: &str, depth: usize) -> Option<SymbolDescriptor>
    {
        if depth >= 64 { return None; }
        let descriptor = self.types.get(id)?;
        descriptor.fields.get(name).cloned()
            .or_else(|| descriptor.parent.and_then(|id| self.storage_member(id, name, depth + 1)))
    }

    fn member(&self, id: usize, name: &str, depth: usize) -> Option<SymbolDescriptor>
    {
        if depth >= 64 { return None; }
        let descriptor = self.types.get(id)?;
        self.properties.iter().rev()
            .find_map(|properties| properties.get(&(id, name.into())).cloned())
            .or_else(|| descriptor.properties.get(name).cloned())
            .or_else(|| descriptor.fields.get(name).cloned())
            .or_else(|| self.methods.iter().rev()
                .find_map(|methods| methods.get(&(id, name.into())).cloned()))
            .or_else(|| descriptor.methods.get(name).cloned())
            .or_else(|| descriptor.parent.and_then(|id| self.member(id, name, depth + 1)))
            .or_else(|| match name
                {
                    "type" =>
                        Some(SymbolDescriptor::new(SymbolKind::Field, self.type_name("Type"))),
                    "value_type" if matches!(descriptor.name.as_str(), "Array" | "HashMap") =>
                        Some(SymbolDescriptor::new(SymbolKind::Field, self.type_name("Type"))),
                    "key_type" if descriptor.name == "HashMap" =>
                        Some(SymbolDescriptor::new(SymbolKind::Field, self.type_name("Type"))),
                    "index" if !descriptor.variants.is_empty() =>
                        Some(SymbolDescriptor::new(SymbolKind::Field, self.type_name("Integer"))),
                    "name" if descriptor.name == "Type" =>
                        Some(SymbolDescriptor::new(SymbolKind::Field, self.type_name("String"))),
                    _ => None,
                })
    }

    fn string_references(&mut self, span: SourceSpan)
    {
        let Some(index) = self.strings.get(&span.start) else { return; };
        let string = &self.analysis.lexical.strings[*index];
        if !string.interpolated { return; }
        let escaped: Vec<usize> = string.parts.iter().filter(|part|
            part.kind == StringPartKind::Escape && part.decoded.slice(&string.decoded) == Some("$"))
            .map(|part| part.decoded.start).collect();
        for reference in interpolation_references(&string.decoded, &escaped)
        {
            let mut names = reference.name.split('.');
            let root = names.next().unwrap();
            let symbol = self.symbol(root)
                .unwrap_or_else(|| SymbolDescriptor::new(SymbolKind::Variable, None));
            if !reference.name.contains('.') || reference.error.is_some()
            {
                if let Some(span) = string.source_span(reference.span)
                { self.mark(span, &symbol); }
                continue;
            }
            // Braced member paths include an extra '{' before the decoded root name.
            let mut end = reference.span.start + root.len() + 1;
            if let Some(span) = string.source_span(SourceSpan::new(reference.span.start, end))
            { self.mark(span, &symbol); }
            let mut receiver = symbol.type_id;
            for name in names
            {
                let start = end;
                end += 1 + name.len();
                let raw = name.starts_with("::");
                let name = name.strip_prefix("::").unwrap_or(name);
                let dynamic = name.starts_with('$');
                let symbol = if dynamic
                    { self.symbol(name).unwrap_or_else(||
                        SymbolDescriptor::new(SymbolKind::Variable, None)) }
                    else
                    { receiver.and_then(|id|
                        if raw { self.storage_member(id, name, 0) }
                        else { self.member(id, name, 0) }).unwrap_or_else(||
                        SymbolDescriptor::new(SymbolKind::Field, None)) };
                if let Some(span) = string.source_span(SourceSpan::new(start, end))
                { self.mark(span, &symbol); }
                receiver = if dynamic { None } else { symbol.return_type.or(symbol.type_id) };
            }
        }
    }
}
