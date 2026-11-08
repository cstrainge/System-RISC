use crate::runtime::paths::split_paths;

use std::{ cell::Cell, collections::{ BTreeMap, BTreeSet }, env::var,
           fs::{ canonicalize, metadata, read_dir, read_to_string },
           path::{ Path, PathBuf } };

use crate::{ language::{ analysis::{ SymbolKind, SymbolSnapshot },
                        ast::{ AstExpression, AstExpressionKind, AstFunctionStatement,
                               AstStatement, AstTopLevel, AstType },
                        data::{ formats::FACTORIES, value::Value },
                        interpreter::bundled_module_source,
                        parser::{ EditorAnalysis, editor::analyze_editor_tree_with_limits },
                        text::{ location::Location, span::SourceSpan } },
             server::intelligence::{ MAX_BYTES, MAX_STEPS,
                 model::{ Import, Kind, Scope, Symbol, Type, annotation } } };

pub(super) const NATIVE: &str = "@native";

pub(super) struct Module
{
    pub source: String,
    pub analysis: EditorAnalysis,
    pub imports: Vec<Import>,
    pub scopes: Vec<Scope>,
    pub methods: Vec<(String, Symbol)>,
    pub visible: BTreeMap<String, bool>,
}

pub(in crate::server) struct Project
{
    pub(super) modules: BTreeMap<String, Module>,
    pub(super) types: BTreeMap<String, Type>,
    pub(super) native: BTreeMap<String, Symbol>,
    pub(super) hidden: BTreeMap<String, Symbol>,
    pub commands: BTreeSet<String>,
    pub overlays: BTreeMap<String, String>,
    bytes: usize,
    pub(super) operations: Cell<usize>,
}

impl Module
{
    pub fn scope_at(&self, offset: usize) -> usize
    {
        self.scopes.iter().enumerate().filter(|(_, scope)|
            scope.span.start <= offset && offset <= scope.span.end)
            .min_by_key(|(_, scope)| scope.span.end - scope.span.start)
            .map_or(0, |(index, _)| index)
    }

    pub fn span(&self, location: &Location) -> Option<SourceSpan>
    {
        self.analysis.tokens.iter().find(|token|
            token.location.line == location.line && token.location.column == location.column)
            .map(|token| token.span)
    }

    fn region(&self, location: &Location) -> SourceSpan
    {
        let Some(span) = self.span(location) else { return self.scopes[0].span; };
        self.analysis.regions.iter().filter(|region|
            region.span.start <= span.start && span.end <= region.span.end)
            .min_by_key(|region| region.span.end - region.span.start)
            .map_or(span, |region| region.span)
    }

    fn child(&mut self, scope: usize, location: &Location, receiver: Option<String>) -> usize
    {
        let index = self.scopes.len();
        self.scopes.push(Scope
            {
                span: self.region(location), parent: Some(scope), receiver, factory: false,
                symbols: BTreeMap::new(), history: BTreeMap::new(),
            });
        index
    }

    fn bind(&mut self, scope: usize, mut symbol: Symbol, location: &Location)
    {
        symbol.scope = scope;
        symbol.span = self.span(location);
        if self.scopes[scope].symbols.get(&symbol.name).is_some_and(|previous| previous.public)
        { symbol.public = true; }
        if symbol.kind == Kind::Variable
        { self.scopes[scope].history.entry(symbol.name.clone()).or_default().push(symbol.clone()); }
        self.scopes[scope].symbols.insert(symbol.name.clone(), symbol);
    }

    fn function(&mut self, home: &str, scope: usize, function: &AstFunctionStatement,
                types: &mut BTreeMap<String, Type>)
    {
        if !enabled(function.condition.as_ref()) { return; }
        let ty = function.return_annotation.as_ref().map(annotation)
            .unwrap_or_else(|| "()".into());
        let kind = if function.receiver.is_some() { Kind::Method } else { Kind::Function };
        let mut symbol = Symbol::new(&function.name, kind, &ty, home, function.public);
        symbol.parameters = function.parameters.iter().filter(|parameter|
            !(function.receiver.is_some() && parameter.name == "$self"
                && function.name != "new")).map(|parameter|
                (parameter.name.trim_start_matches('$').into(), parameter.annotation.as_ref()
                    .map(annotation).unwrap_or("any".into()))).collect();
        let parameters = function.parameters.iter().filter(|parameter|
            !(function.receiver.is_some() && parameter.name == "$self"
                && function.name != "new")).map(|parameter|
                {
                    let ty = parameter.annotation.as_ref().map(|ty|
                        if parameter.variadic && let AstType::Array(inner) = ty
                        { annotation(inner) } else { annotation(ty) }).unwrap_or("any".into());
                    format!("{}: {}{}", parameter.name, ty,
                        if parameter.variadic { "..." } else { "" })
                }).collect::<Vec<_>>().join(", ");
        let name = function.receiver.as_ref().map_or_else(|| function.name.clone(),
            |receiver| format!("{}::{}", receiver, function.name));
        symbol.signature = format!("{}fn {}({}): {}",
            if function.public { "pub " } else { "" }, name, parameters, ty);
        symbol.span = self.span(&function.location);
        symbol.scope = scope;
        if let Some(receiver) = &function.receiver
        {
            if self.methods.iter().any(|(previous, method)| previous == receiver
                && method.name == symbol.name && method.scope == scope && method.public)
            { symbol.public = true; }
            self.methods.push((receiver.clone(), symbol));
        }
        else { self.bind(scope, symbol, &function.location); }
        let inner = self.child(scope, &function.location, function.receiver.clone());
        self.scopes[inner].factory = function.receiver.is_some() && function.name == "new";
        for parameter in &function.parameters
        {
            let ty = parameter.annotation.as_ref().map(annotation)
                .or_else(|| (parameter.name == "$self").then(||
                    function.receiver.clone().unwrap_or_default())).unwrap_or("any".into());
            let symbol = Symbol::new(&parameter.name, Kind::Variable, &ty, home, false);
            self.bind(inner, symbol, &parameter.location);
        }
        self.statements(home, inner, &function.body, types);
    }

    fn statements(&mut self, home: &str, scope: usize, statements: &AstTopLevel,
                  types: &mut BTreeMap<String, Type>)
    {
        for statement in statements
        {
            let item = match statement
            {
                AstStatement::StructDeclaration(item) => Some((
                    &item.name, item.public, &item.location, "struct", String::new())),
                AstStatement::EnumDeclaration(item) => Some((
                    &item.name, item.public, &item.location, "enum", String::new())),
                AstStatement::TypeDeclaration(item) => Some((
                    &item.name, item.public, &item.location, "type", annotation(&item.annotation))),
                _ => None,
            };
            if let Some((name, public, location, kind, parent)) = item
            {
                let key = format!("{}#{}::{}", home, scope, name);
                let mut symbol = Symbol::new(name, Kind::Type, &key, home, public);
                symbol.signature = format!("{}{} {}", if public { "pub " } else { "" }, kind, name);
                symbol.scope = scope;
                symbol.span = self.span(location);
                self.bind(scope, symbol.clone(), location);
                types.insert(key, Type { symbol: Some(symbol), parent, ..Type::default() });
            }
        }
        for statement in statements
        {
            match statement
            {
                AstStatement::StructDeclaration(item) =>
                {
                    let key = self.scopes[scope].symbols[&item.name].ty.clone();
                    for field in &item.fields
                    {
                        let ty = annotation(&field.annotation);
                        let mut symbol =
                            Symbol::new(&field.name, Kind::Field, &ty, home, field.public);
                        symbol.signature = format!("{}{}: {}",
                            if field.public { "pub " } else { "" }, field.name, ty);
                        symbol.scope = scope;
                        symbol.span = self.span(&field.location);
                        types.get_mut(&key).unwrap().fields.insert(field.name.clone(), symbol);
                    }
                },
                AstStatement::EnumDeclaration(item) =>
                {
                    let key = &self.scopes[scope].symbols[&item.name].ty;
                    types.get_mut(key).unwrap().variants = item.variants.iter()
                        .map(|(name, _)| name.clone()).collect();
                },
                AstStatement::FunctionDefinition(function) =>
                    self.function(home, scope, function, types),
                AstStatement::LetStatement(item) =>
                {
                    let ty = item.annotation.as_ref().map(annotation).unwrap_or_default();
                    let mut symbol = Symbol::new(&item.identifier, Kind::Variable, &ty,
                        home, item.public);
                    if let Some(span) = self.span(&item.expression.location)
                    {
                        let end = self.region(&item.location).end;
                        let text = self.source.get(span.start..end).unwrap_or_default()
                            .trim_end_matches(';').trim();
                        if text.len() <= 16_384 { symbol.initializer = text.into(); }
                    }
                    self.bind(scope, symbol, &item.location);
                    self.expression_scopes(home, scope, &item.expression, types);
                },
                AstStatement::BlockStatement(block) | AstStatement::LoopStatement(block) =>
                {
                    if !enabled(block.condition.as_ref()) { continue; }
                    let inner = if block.condition.is_some() { scope }
                        else { self.child(scope, &block.location, None) };
                    self.statements(home, inner, &block.body, types);
                },
                AstStatement::ForStatement(item) =>
                {
                    let inner = self.child(scope, &item.body.location, None);
                    for name in item.bindings.iter().flatten()
                    {
                        self.scopes[inner].symbols.insert(name.clone(),
                            Symbol::new(name, Kind::Variable, "any", home, false));
                    }
                    self.statements(home, inner, &item.body.body, types);
                },
                AstStatement::ConditionalLoopStatement(item) =>
                {
                    let inner = self.child(scope, &item.body.location, None);
                    self.statements(home, inner, &item.body.body, types);
                },
                AstStatement::AliasStatement(item) =>
                {
                    let mut symbol =
                        Symbol::new(&item.alias, Kind::Alias, "any", home, item.public);
                    symbol.signature = format!("alias {}", item.alias);
                    self.bind(scope, symbol, &item.location);
                },
                AstStatement::ExpressionStatement(item) | AstStatement::DiscardStatement(item) =>
                    self.expression_scopes(home, scope, item, types),
                AstStatement::ReturnStatement(item) =>
                {
                    if let Some(expression) = &item.expression
                    { self.expression_scopes(home, scope, expression, types); }
                },
                _ => {},
            }
        }
    }

    fn expression_scopes(&mut self, home: &str, scope: usize, expression: &AstExpression,
                         types: &mut BTreeMap<String, Type>)
    {
        match &expression.kind
        {
            AstExpressionKind::AnonymousFunction(function) => self.function(home,
                scope, function, types),
            AstExpressionKind::Grouped(inner) => self.expression_scopes(home, scope, inner, types),
            AstExpressionKind::IfExpression(item) =>
            {
                for branch in &item.branches
                {
                    let inner = self.child(scope, &branch.body.location, None);
                    self.statements(home, inner, &branch.body.body, types);
                }
                if let Some(block) = &item.else_body
                {
                    let inner = self.child(scope, &block.location, None);
                    self.statements(home, inner, &block.body, types);
                }
            },
            AstExpressionKind::MatchExpression(item) =>
            {
                for arm in &item.arms
                {
                    let inner = self.child(scope, &arm.body.location, None);
                    self.statements(home, inner, &arm.body.body, types);
                }
            },
            _ => {},
        }
    }
}

pub(super) fn enabled(condition: Option<&AstExpression>) -> bool
{
    condition.is_none_or(|expression| matches!(&expression.kind,
        AstExpressionKind::Literal(item) if item.value == Value::Boolean(true)))
}

impl Project
{
    pub fn baseline(snapshot: &SymbolSnapshot) -> Self
    {
        let mut project = Self
            {
                modules: BTreeMap::new(), types: BTreeMap::new(), native: BTreeMap::new(),
                hidden: BTreeMap::new(),
                    commands: BTreeSet::new(), overlays: BTreeMap::new(), bytes: 0,
                operations: Cell::new(40_000),
            };
        for (name, symbol) in &snapshot.symbols
        {
            if name.starts_with("__") && !snapshot.hidden_symbols.contains(name) { continue; }
            let kind = match symbol.kind
            {
                SymbolKind::Type => Kind::Type, SymbolKind::Variable => Kind::Variable,
                SymbolKind::Alias => Kind::Alias, _ => Kind::Function,
            };
            let ty = symbol.type_id.and_then(|id| snapshot.types.get(id))
                .map_or("any", |ty| ty.name.as_str());
            let mut item = Symbol::new(name, kind, ty, NATIVE, true);
            if kind == Kind::Type { item.ty = format!("{}::{}", NATIVE, name); }
            if kind == Kind::Function { item.signature = format!("native fn {}", name); }
            if snapshot.hidden_symbols.contains(name) { project.hidden.insert(name.clone(), item); }
            else { project.native.insert(name.clone(), item); }
        }
        for (id, descriptor) in snapshot.types.iter().enumerate()
        {
            let key = format!("{}::{}", NATIVE, descriptor.name);
            let symbol = Symbol::new(&descriptor.name, Kind::Type, &key, NATIVE, true);
            let mut ty = Type { symbol: Some(symbol), ..Type::default() };
            for (name, method) in &descriptor.methods
            {
                let result = method.return_type.and_then(|id| snapshot.types.get(id))
                    .map_or("any", |ty| ty.name.as_str());
                let mut symbol = Symbol::new(name, Kind::Method, result, NATIVE, true);
                let count = snapshot.method_arguments.get(&(id, name.clone()))
                    .copied().unwrap_or(0);
                let argument = if descriptor.name == "String" { "String" }
                    else if name == "zip" { "Array" } else { "any" };
                let parameters = (0..count).map(|index| format!("$arg{}: {}", index + 1, argument))
                    .collect::<Vec<_>>().join(", ");
                symbol.signature = format!("fn {}::{}({}): {}",
                    descriptor.name, name, parameters, result);
                ty.methods.push(symbol);
            }
            project.types.entry(key).or_insert(ty);
        }
        for &(name, _, encode) in FACTORIES
        {
            let result = if encode { "String" } else { "any" };
            if let Some(symbol) = project.native.get_mut(name)
            {
                symbol.ty = result.into();
                symbol.signature = format!("fn {}($value: {}): {}", name,
                    if encode { "any" } else { "String" }, result);
            }
            if let Some((receiver, method)) = name.split_once("::")
                && let Some(key) = project.native.get(receiver)
                    .filter(|symbol| symbol.kind == Kind::Type)
                    .map(|symbol| symbol.ty.clone())
                && let Some(symbol) = project.native.get(name)
            {
                let mut method_symbol = symbol.clone();
                method_symbol.name = method.into();
                method_symbol.kind = Kind::Method;
                project.types.get_mut(&key).unwrap().methods.push(method_symbol);
            }
        }
        project
    }

    pub(super) fn spend(&self) -> bool
    {
        let Some(remaining) = self.operations.get().checked_sub(1) else { return false; };
        self.operations.set(remaining);
        true
    }

    pub fn discover_commands(&mut self)
    {
        for directory in split_paths(&var("PATH").unwrap_or_default()).take(64)
        {
            let Ok(entries) = read_dir(directory) else { continue; };
            for entry in entries.flatten().take(4096)
            {
                if self.commands.len() >= 4096 { return; }
                let Ok(metadata) = entry.metadata() else { continue; };
                if !metadata.is_file() { continue; }
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if metadata.permissions().mode() & 0o111 == 0 { continue; }
                }
                if let Some(name) = entry.file_name().to_str()
                { self.commands.insert(name.into()); }
            }
        }
    }

    pub fn load(&mut self, home: &str, source: Option<String>)
    {
        if self.modules.contains_key(home) || self.modules.len() >= 64 { return; }
        let source = source.or_else(|| self.overlays.get(home).cloned()).or_else(||
        {
            if let Some(name) = home.strip_prefix("@std/")
            { bundled_module_source(name).map(str::to_string) }
            else
            {
                let info = metadata(home).ok()?;
                if !info.is_file() || info.len() > MAX_BYTES as u64 { return None; }
                read_to_string(home).ok()
            }
        });
        let Some(source) = source.filter(|source| source.len() <= MAX_BYTES) else { return; };
        if self.bytes + source.len() > 8 * MAX_BYTES { return; }
        self.bytes += source.len();
        let (analysis, ast) = analyze_editor_tree_with_limits(&source, MAX_BYTES, MAX_STEPS);
        let mut module = Module
            {
                source, analysis, imports: Vec::new(), methods: Vec::new(),
                visible: BTreeMap::new(),
                scopes: vec![Scope
                    {
                        span: SourceSpan::new(0, 0), parent: None, receiver: None, factory: false,
                        symbols: BTreeMap::new(), history: BTreeMap::new(),
                    }],
            };
        module.scopes[0].span.end = module.source.len();
        module.statements(home, 0, &ast, &mut self.types);
        for statement in &ast
        {
            let call = match statement
            {
                AstStatement::ExecuteStatement(call) => Some(call.as_ref()),
                AstStatement::ExpressionStatement(expression) =>
                    if let AstExpressionKind::Execute(call) = &expression.kind
                    { Some(call.as_ref()) } else { None },
                _ => None,
            };
            if let Some(call) = call
                && matches!(&call.executable.kind,
                    AstExpressionKind::Symbol(symbol) if symbol.name == "visible")
            {
                for argument in &call.arguments
                {
                    let argument =
                        if let AstExpressionKind::ExecutableReference(inner) = &argument.kind
                        { inner.as_ref() } else { argument };
                    let name = match &argument.kind
                    {
                        AstExpressionKind::Symbol(symbol) => Some(symbol.name.clone()),
                        AstExpressionKind::Literal(item) =>
                            if let Value::String(name, _) = &item.value
                            { Some(name.clone()) } else { None },
                        _ => None,
                    };
                    if let Some(name) = name { module.visible.insert(name, call.public); }
                }
            }
            if let AstStatement::ImportStatement(import) = statement
                && enabled(import.condition.as_ref())
                && let Some(target) = self.resolve_module(home, &import.module)
            {
                module.imports.push(Import
                    {
                        name: import.module.clone(), target, names: import.names.clone(),
                        public: import.public, parent: import.module.starts_with("super"),
                    });
            }
        }
        if let Some(target) = self.resolve_module(home, "std::prelude")
            && target != home
        {
            module.imports.push(Import
                {
                    name: "@prelude".into(), target, names: vec!["*".into()],
                    public: false, parent: false,
                });
        }
        let targets: Vec<_> = module.imports.iter().map(|import| import.target.clone()).collect();
        self.modules.insert(home.into(), module);
        for target in targets { self.load(&target, None); }
    }

    fn candidates(&self, base: &Path) -> Option<String>
    {
        let found: Vec<_> = [base.with_extension("shy"), base.join("init.shy")].into_iter()
            .map(|path| normalize(&path)).filter(|path|
                self.overlays.contains_key(path) || metadata(path).is_ok_and(|info| info.is_file()))
            .collect();
        if found.len() == 1 { Some(found[0].clone()) } else { None }
    }

    fn resolve_module(&self, home: &str, name: &str) -> Option<String>
    {
        if name == "std" || name.ends_with("::init") { return None; }
        if name == "super" || name.starts_with("super::")
        {
            if let Some(virtual_home) = home.strip_prefix("@std/")
            {
                let mut parent = Path::new(virtual_home).parent()?;
                if virtual_home.ends_with("/init") { parent = parent.parent()?; }
                let target = bundled(&parent.to_string_lossy())?;
                return if name == "super" { Some(target) }
                    else { self.resolve_module(&target, &name[7..]) };
            }
            let mut parent = Path::new(home).parent()?;
            if home.ends_with("/init.shy") { parent = parent.parent()?; }
            let target = self.candidates(parent)?;
            return if name == "super" { Some(target) }
                else { self.resolve_module(&target, &name[7..]) };
        }
        let standard = name.strip_prefix("std::");
        let relative = standard.unwrap_or(name).replace("::", "/");
        let variable = if standard.is_some() { "SHELLY_STD_PATH" } else { "SHELLY_MODULE_PATH" };
        let paths = var(variable).unwrap_or_else(|_| if standard.is_some()
            { "/etc/shelly/std:/usr/local/share/shelly/std:/usr/share/shelly/std:bundled".into() }
            else { String::new() });
        let mut directories: Vec<PathBuf> = Vec::new();
        if standard.is_none()
        {
            if let Some(virtual_home) = home.strip_prefix("@std/")
            {
                let parent = Path::new(virtual_home).parent().unwrap_or(Path::new(""));
                let relative = parent.join(&relative).to_string_lossy().into_owned();
                if let Some(target) = bundled(&relative) { return Some(target); }
            }
            else if let Some(parent) = Path::new(home).parent() { directories.push(parent.into()); }
        }
        directories.extend(split_paths(&paths).take(64).filter(|path| !path.as_os_str().is_empty())
            .map(|path| if let Some(tail) = path.to_str().and_then(|path| path.strip_prefix("~/"))
                { PathBuf::from(var("HOME").unwrap_or_default()).join(tail) } else { path }));
        for directory in directories
        {
            if standard.is_some() && directory.as_os_str() == "bundled"
            {
                if let Some(target) = bundled(&relative) { return Some(target); }
            }
            else
            {
                let base = directory.join(&relative);
                // Both layouts in one directory are ambiguous, not a search-path fallback.
                let files = [base.with_extension("shy"), base.join("init.shy")];
                let present = files.iter().filter(|path|
                    self.overlays.contains_key(&normalize(path)) || path.is_file()).count();
                if present > 1 { return None; }
                if let Some(target) = self.candidates(&base) { return Some(target); }
            }
        }
        None
    }
}

fn bundled(relative: &str) -> Option<String>
{
    let candidates = [relative.to_string(), format!("{}/init", relative)];
    let found: Vec<_> = candidates.iter()
        .filter(|name| bundled_module_source(name).is_some()).collect();
    (found.len() == 1).then(|| format!("@std/{}", found[0]))
}

pub(super) fn normalize(path: &Path) -> String
{
    let path = canonicalize(path).unwrap_or_else(|_|
        if let Some(parent) = path.parent().and_then(|parent| canonicalize(parent).ok())
            && let Some(name) = path.file_name()
        { parent.join(name) } else { path.to_path_buf() });
    path.to_string_lossy().into_owned()
}

pub(in crate::server) fn uri_path(uri: &str) -> Option<String>
{
    let text = uri.strip_prefix("file://")?;
    let text = text.strip_prefix("localhost").unwrap_or(text);
    if !text.starts_with('/') { return None; }
    let mut bytes = Vec::new();
    let mut chars = text.as_bytes().iter().copied();
    while let Some(byte) = chars.next()
    {
        if byte == b'%'
        {
            let high = (chars.next()? as char).to_digit(16)?;
            let low = (chars.next()? as char).to_digit(16)?;
            bytes.push((high * 16 + low) as u8);
        }
        else { bytes.push(byte); }
    }
    let path = String::from_utf8(bytes).ok()?;
    if path.contains('\0') { return None; }
    Some(normalize(Path::new(&path)))
}
