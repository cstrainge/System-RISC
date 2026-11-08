
use std::{ collections::{ BTreeMap, BTreeSet }, path::Path };

use serde_json::{ Value, json };

use crate::{ language::{ ast::{ AstExpression, AstExpressionKind, AstStatement },
                        parser::editor::analyze_editor_tree_with_limits,
                        text::{ interpolation::interpolation_references, span::SourceSpan },
                        tokenizer::{ TokenKind, source::{ StringPartKind, TriviaKind } } },
             server::intelligence::{ MAX_BYTES, MAX_STEPS,
                 model::{ Kind, Symbol }, project::{ NATIVE, Project } } };

impl Project
{
    fn resolve(&self, home: &str, scope: usize, name: &str, offset: usize,
               exported: bool, depth: usize) -> Option<Symbol>
    {
        if depth >= 32 { return None; }
        self.resolve_inner(home, scope, name, offset, exported, &mut BTreeSet::new())
    }

    fn resolve_inner(&self, home: &str, scope: usize, name: &str, offset: usize,
                     exported: bool, visited: &mut BTreeSet<String>) -> Option<Symbol>
    {
        if !self.spend() || visited.len() >= 128 || !visited.insert(format!("{}:{}:{}:{}", home,
            scope, name, exported))
        { return None; }
        let module = self.modules.get(home)?;
        let mut current = Some(scope);
        while let Some(index) = current
        {
            let scope = &module.scopes[index];
            let version = scope.history.get(name).and_then(|versions| versions.iter().rev()
                .find(|symbol| symbol.span.is_none_or(|span| span.start <= offset)));
            if let Some(symbol) = version.or_else(|| scope.symbols.get(name))
                && (!exported || symbol.public)
                && (symbol.kind != Kind::Variable || exported
                    || symbol.span.is_none_or(|span| span.start <= offset))
            { return Some(symbol.clone()); }
            current = if exported { None } else { scope.parent };
        }
        if module.visible.get(name).is_some_and(|public| !exported || *public)
            && let Some(symbol) = self.hidden.get(name).or_else(|| self.native.get(name))
        { return Some(symbol.clone()); }
        let plain = name.strip_prefix('$').unwrap_or(name);
        for import in &module.imports
        {
            if exported && !import.public { continue; }
            if import.name == "@prelude" && !exported
                && let Some(symbol) = self.native.get(name)
            { return Some(symbol.clone()); }
            if import.names.is_empty()
            {
                if import.name.starts_with(&format!("{}::", plain))
                {
                    let mut symbol = Symbol::new(plain, Kind::Module, "", home, import.public);
                    symbol.initializer = plain.into();
                    return Some(symbol);
                }
                if plain == import.name
                {
                    return Some(Symbol::new(name, Kind::Module, &import.target,
                        home, import.public));
                }
                if let Some(tail) = plain.strip_prefix(&format!("{}::", import.name))
                {
                    let member =
                        if name.starts_with('$') { format!("${}", tail) } else { tail.into() };
                    return self.resolve_inner(&import.target, 0, &member, usize::MAX,
                        !import.parent, visited);
                }
            }
            else if import.names.iter().any(|item| item == name || item == "*")
                && let Some(symbol) = self.resolve_inner(&import.target, 0, name, usize::MAX,
                    !import.parent, visited)
            { return Some(symbol); }
        }
        if exported { return None; }
        self.native.get(name).cloned().or_else(||
        {
            self.native.keys().any(|key| key.starts_with(&format!("{}::", name))).then(||
            {
                let mut symbol = Symbol::new(name, Kind::Module, "", NATIVE, true);
                symbol.initializer = name.into();
                symbol
            })
        })
    }

    fn type_key(&self, home: &str, scope: usize, ty: &str, depth: usize) -> Option<String>
    {
        if depth >= 32 || !self.spend() { return None; }
        if self.types.contains_key(ty) { return Some(ty.into()); }
        let ty = ty.strip_suffix(" | ()").unwrap_or(ty).trim();
        if ty.starts_with('[')
        { return self.native.get(if ty.contains(':') { "HashMap" } else { "Array" })
            .map(|symbol| symbol.ty.clone()); }
        if ty.contains(" | ") || ty == "any" || ty.is_empty() { return None; }
        if home == NATIVE { return self.native.get(ty).or_else(|| self.hidden.get(ty))
            .map(|symbol| symbol.ty.clone()); }
        self.resolve(home, scope, ty, usize::MAX, false, depth + 1)
            .filter(|symbol| symbol.kind == Kind::Type).map(|symbol| symbol.ty)
    }

    fn field_name_type(&self, key: &str, depth: usize) -> bool
    {
        if key == "@native::String" { return true; }
        if depth >= 16 { return false; }
        self.types.get(key).is_some_and(|ty| ty.symbol.as_ref().is_some_and(|symbol|
            self.type_key(&symbol.owner, symbol.scope, &ty.parent, 0)
                .is_some_and(|key| self.field_name_type(&key, depth + 1))))
    }

    fn symbol_type(&self, symbol: &Symbol, depth: usize) -> Option<String>
    {
        if depth >= 24 { return None; }
        if !symbol.ty.is_empty()
        { return self.type_key(&symbol.owner, symbol.scope, &symbol.ty, depth + 1); }
        if symbol.initializer.is_empty() { return None; }
        let offset = symbol.span.map_or(usize::MAX, |span| span.start);
        self.infer_text(&symbol.owner, symbol.scope, &symbol.initializer, offset, depth + 1)
    }

    fn infer_text(&self, home: &str, scope: usize, text: &str,
                  offset: usize, depth: usize) -> Option<String>
    {
        if depth >= 24 || text.len() > MAX_BYTES { return None; }
        let (_, statements) = analyze_editor_tree_with_limits(text, MAX_BYTES, MAX_STEPS);
        let statement = statements.first()?;
        match statement
        {
            AstStatement::ExpressionStatement(expression)
                | AstStatement::DiscardStatement(expression) =>
                self.infer(home, scope, expression, offset, depth + 1),
            AstStatement::ExecuteStatement(call) => self.infer(home, scope,
                &call.executable, offset, depth + 1),
            _ => None,
        }
    }

    fn infer(&self, home: &str, scope: usize, expression: &AstExpression,
             offset: usize, depth: usize) -> Option<String>
    {
        if depth >= 24 { return None; }
        match &expression.kind
        {
            AstExpressionKind::Variable(item) | AstExpressionKind::Symbol(item) =>
            {
                let symbol = self.resolve(home, scope, &item.name, offset, false, 0)?;
                if symbol.kind == Kind::Type { return self.type_key(NATIVE, 0, "Type", depth + 1); }
                self.symbol_type(&symbol, depth + 1)
            },
            AstExpressionKind::StructConstructor(item) => self.type_key(home,
                scope, &item.name, depth),
            AstExpressionKind::SpacedEmptyCall(name) =>
            {
                let symbol = self.resolve(home, scope, name, offset, false, 0)?;
                self.symbol_type(&symbol, depth + 1)
            },
            AstExpressionKind::TypeConversion(name, _, _, _) => self.type_key(home,
                scope, name, depth),
            AstExpressionKind::EnumVariant(owner, name) =>
            {
                if let Some(symbol) = self.resolve(home, scope, &format!("{}::{}", owner, name),
                    offset, false, 0)
                { return self.symbol_type(&symbol, depth + 1); }
                let key = self.type_key(home, scope, owner, depth + 1)?;
                let symbol = self.members(&key, home, scope, false, false)
                    .into_iter().find(|symbol| symbol.name == *name);
                symbol.as_ref().and_then(|symbol| self.symbol_type(symbol, depth + 1)).or(Some(key))
            },
            AstExpressionKind::Field(inner, name, _) =>
            {
                if name.starts_with('$') || name.starts_with("::$") { return None; }
                let key = self.infer(home, scope, inner, offset, depth + 1)?;
                let raw = name.starts_with("::");
                let name = name.strip_prefix("::").unwrap_or(name);
                self.members(&key, home, scope, raw, false).into_iter()
                    .find(|symbol| symbol.name == name)
                    .and_then(|symbol| self.symbol_type(&symbol, depth + 1))
            },
            AstExpressionKind::Grouped(inner) | AstExpressionKind::TryExecute(inner)
            | AstExpressionKind::ExecutableReference(inner) =>
                self.infer(home, scope, inner, offset, depth + 1),
            AstExpressionKind::Execute(call) =>
                self.infer(home, scope, &call.executable, offset, depth + 1),
            AstExpressionKind::Literal(item) =>
                self.type_key(NATIVE, 0, &item.value.type_name(), depth),
            AstExpressionKind::Array(_) => self.type_key(NATIVE, 0, "Array", depth),
            AstExpressionKind::HashMap(_) => self.type_key(NATIVE, 0, "HashMap", depth),
            AstExpressionKind::Index(_, _) => self.typed_expression(home, scope, expression,
                offset, depth + 1).and_then(|symbol| self.symbol_type(&symbol, depth + 1)),
            _ => None,
        }
    }

    fn typed_expression(&self, home: &str, scope: usize, expression: &AstExpression,
                        offset: usize, depth: usize) -> Option<Symbol>
    {
        if depth >= 24 || !self.spend() { return None; }
        match &expression.kind
        {
            AstExpressionKind::Variable(variable) | AstExpressionKind::Symbol(variable) =>
            {
                let symbol = self.resolve(home, scope, &variable.name, offset, false, 0)?;
                if !symbol.ty.is_empty() { return Some(symbol); }
                let (_, statements) = analyze_editor_tree_with_limits(&symbol.initializer,
                    MAX_BYTES, MAX_STEPS);
                let expression = match statements.first()?
                {
                    AstStatement::ExpressionStatement(expression) => expression,
                    AstStatement::ExecuteStatement(call) => &call.executable,
                    _ => return None,
                };
                self.typed_expression(&symbol.owner, symbol.scope, expression, offset, depth + 1)
            },
            AstExpressionKind::Field(inner, name, _) =>
            {
                let key = self.infer(home, scope, inner, offset, depth + 1)?;
                self.members(&key, home, scope, name.starts_with("::"), false).into_iter()
                    .find(|symbol| symbol.name == name.trim_start_matches("::"))
            },
            AstExpressionKind::Grouped(inner) | AstExpressionKind::ExecutableReference(inner) =>
                self.typed_expression(home, scope, inner, offset, depth + 1),
            AstExpressionKind::Execute(call) =>
                self.typed_expression(home, scope, &call.executable, offset, depth + 1),
            AstExpressionKind::Index(inner, index) =>
            {
                let mut symbol = self.typed_expression(home, scope, inner, offset, depth + 1)?;
                for _ in 0..16
                {
                    if symbol.ty.starts_with('[') { break; }
                    let key = self.type_key(&symbol.owner, symbol.scope, &symbol.ty, depth + 1)?;
                    let ty = self.types.get(&key)?;
                    symbol.ty = ty.parent.clone();
                    if let Some(owner) = &ty.symbol
                    { symbol.owner = owner.owner.clone(); symbol.scope = owner.scope; }
                }
                let inner = symbol.ty.strip_prefix('[')?.strip_suffix(']')?;
                if inner.starts_with("fn(") { return None; }
                let mut nesting = 0;
                let mut separator = None;
                let mut items = vec![0];
                let bytes = inner.as_bytes();
                for (offset, character) in inner.char_indices()
                {
                    match character
                    {
                        '[' | '(' => nesting += 1,
                        ']' | ')' => nesting -= 1,
                        ':' if nesting == 0 && bytes.get(offset + 1) != Some(&b':')
                            && offset.checked_sub(1)
                                .and_then(|offset| bytes.get(offset)) != Some(&b':') =>
                                { separator = Some(offset); },
                        ',' if nesting == 0 => items.push(offset + 1),
                        _ => {},
                    }
                }
                let value = if let Some(separator) = separator { &inner[separator + 1..] }
                    else if items.len() > 1
                    {
                        let AstExpressionKind::Literal(literal) = &index.kind else { return None; };
                        if literal.value.type_name() != "Integer" { return None; }
                        let index = usize::try_from(literal.value.as_integer()).ok()
                            .filter(|index| *index < items.len())?;
                        let end = items.get(index + 1).map_or(inner.len(), |end| end - 1);
                        &inner[items[index]..end]
                    }
                    else { inner };
                symbol.ty = value.trim().into();
                Some(symbol)
            },
            _ => self.infer(home, scope, expression, offset, depth + 1)
                .map(|key| Symbol::new("", Kind::Variable, &key, home, false)),
        }
    }

    fn reachable(&self, home: &str, owner: &str, depth: usize) -> bool
    {
        if home == owner || owner == NATIVE { return true; }
        if depth >= 32 || !self.spend() { return false; }
        self.modules.get(home).is_some_and(|module| module.imports.iter().any(|import|
            import.target == owner || self.exported_path(&import.target, owner, depth + 1)))
    }

    fn exported_path(&self, home: &str, owner: &str, depth: usize) -> bool
    {
        if home == owner { return true; }
        if depth >= 32 || !self.spend() { return false; }
        self.modules.get(home).is_some_and(|module| module.imports.iter().any(|import|
            import.public && self.exported_path(&import.target, owner, depth + 1)))
    }

    fn method_visible(&self, method: &Symbol, type_home: &str, current: &str, scope: usize) -> bool
    {
        if method.owner == current
        {
            let Some(module) = self.modules.get(current) else { return false; };
            let mut current = Some(scope);
            while let Some(index) = current
            {
                if index == method.scope { return true; }
                current = module.scopes[index].parent;
            }
            return false;
        }
        if method.scope != 0 { return false; }
        if method.owner == NATIVE { return true; }
        if method.public && (method.owner == type_home || self.reachable(current, &method.owner, 0))
        { return true; }
        if method.name == "new" && !method.public
        {
            let owner = Path::new(&method.owner);
            let directory = if owner.file_name().is_some_and(|name| name == "init.shy")
                { owner.parent().unwrap().to_path_buf() } else { owner.with_extension("") };
            return Path::new(current).starts_with(directory);
        }
        false
    }

    fn members(&self, key: &str, home: &str, scope: usize, raw: bool, static_only: bool)
        -> Vec<Symbol>
    { self.members_inner(key, home, scope, raw, static_only, 0) }

    fn members_inner(&self, key: &str, home: &str, scope: usize, raw: bool,
                     static_only: bool, depth: usize) -> Vec<Symbol>
    {
        if depth >= 32 || !self.spend() { return Vec::new(); }
        let Some(ty) = self.types.get(key) else { return Vec::new(); };
        let owner = ty.symbol.as_ref().map_or(NATIVE, |symbol| symbol.owner.as_str());
        let mut methods = ty.methods.clone();
        for (module_home, module) in &self.modules
        {
            for (receiver, method) in &module.methods
            {
                if self.type_key(module_home, method.scope, receiver, 0).as_deref() == Some(key)
                { methods.push(method.clone()); }
            }
        }
        methods.sort_by_key(|method|
            if method.owner == home { 3 } else if method.owner == NATIVE { 0 }
            else if method.owner == owner { 1 } else { 2 });
        let privileged = home == owner && self.modules.get(home).is_some_and(|module|
        {
            let mut current = Some(scope);
            while let Some(index) = current
            {
                let scope = &module.scopes[index];
                if let Some(receiver) = &scope.receiver
                    && self.type_key(home, index, receiver, 0).as_deref() == Some(key)
                { return true; }
                current = scope.parent;
            }
            false
        });
        let mut result: BTreeMap<String, Symbol> = self.type_key(owner,
            ty.symbol.as_ref().map_or(0, |symbol| symbol.scope), &ty.parent, 0)
            .filter(|parent| parent != key).map_or(Vec::new(), |parent|
                self.members_inner(&parent, home, scope, raw, static_only, depth + 1))
            .into_iter().map(|symbol| (symbol.name.clone(), symbol)).collect();
        if !static_only
        {
            for (name, field) in &ty.fields
            {
                if field.public || privileged
                { result.insert(name.clone(), field.clone()); }
            }
        }
        let mut properties = BTreeMap::new();
        for method in &methods
        {
            if let Some(name) = method.name.strip_prefix("get_").or_else(||
                method.name.strip_prefix("set_"))
            {
                if !raw { result.remove(name); }
                if !self.method_visible(method, owner, home, scope) { continue; }
                let getter = method.name.starts_with("get_");
                let value = if getter { method.ty.clone() }
                    else { method.parameters.first().map_or("any", |(_, ty)| ty.as_str()).into() };
                let mut symbol = Symbol::new(name, Kind::Field, &value, 
                    &method.owner, method.public);
                symbol.scope = method.scope;
                symbol.signature = format!("{}: {}", name, value);
                let previous: Option<&Symbol> = properties.get(name);
                let (read, write) = previous.and_then(|symbol| symbol.property).unwrap_or_default();
                symbol.property = Some((read || getter, write || !getter));
                if !getter && read
                {
                    let current = properties.get_mut(name).unwrap();
                    current.property = symbol.property;
                }
                else { properties.insert(name.into(), symbol); }
            }
        }
        if !raw && !static_only { result.extend(properties); }
        if !raw
        {
            for method in methods
            {
                if self.method_visible(&method, owner, home, scope)
                    && (static_only || method.name != "new" && !method.name.starts_with("from_"))
                    && result.get(&method.name).is_none_or(|symbol| symbol.kind == Kind::Method)
                { result.insert(method.name.clone(), method); }
            }
            if static_only
            {
                for name in &ty.variants
                {
                    result.entry(name.clone()).or_insert_with(||
                        Symbol::new(name, Kind::Variant, key, owner, true));
                }
            }
            else
            {
                let type_symbol = Symbol::new("type", Kind::Field, "Type", NATIVE, true);
                result.entry("type".into()).or_insert(type_symbol);
                if !ty.variants.is_empty()
                { result.entry("index".into()).or_insert_with(||
                    Symbol::new("index", Kind::Field, "Integer", NATIVE, true)); }
                if key == "@native::Type"
                { result.entry("name".into()).or_insert_with(||
                    Symbol::new("name", Kind::Field, "String", NATIVE, true)); }
                if key == "@native::Array" || key == "@native::HashMap"
                { result.entry("value_type".into()).or_insert_with(||
                    Symbol::new("value_type", Kind::Field, "Type", NATIVE, true)); }
                if key == "@native::HashMap"
                { result.entry("key_type".into()).or_insert_with(||
                    Symbol::new("key_type", Kind::Field, "Type", NATIVE, true)); }
            }
        }
        result.into_values().collect()
    }

    fn exported_symbols(&self, home: &str, private: bool, depth: usize) -> BTreeMap<String, Symbol>
    {
        self.exported_inner(home, private, depth, &mut BTreeSet::new())
    }

    fn exported_inner(&self, home: &str, private: bool, depth: usize,
                      visited: &mut BTreeSet<String>) -> BTreeMap<String, Symbol>
    {
        if !self.spend() || !visited.insert(format!("{}:{}", home, private))
        { return BTreeMap::new(); }
        if depth >= 32 { return BTreeMap::new(); }
        let Some(module) = self.modules.get(home) else { return BTreeMap::new(); };
        let mut result: BTreeMap<_, _> = module.scopes[0].symbols.iter()
            .filter(|(_, symbol)| private || symbol.public).map(|(name, symbol)|
                (name.clone(), symbol.clone())).collect();
        for (name, public) in &module.visible
        {
            if (private || *public)
                && let Some(symbol) = self.hidden.get(name).or_else(|| self.native.get(name))
            { result.insert(name.clone(), symbol.clone()); }
        }
        for import in &module.imports
        {
            if !private && !import.public { continue; }
            if import.names.is_empty()
            { result.insert(import.name.clone(), Symbol::new(&import.name, Kind::Module,
                &import.target, home, import.public)); }
            else
            {
                for (name, symbol) in self.exported_inner(&import.target, import.parent, depth + 1,
                    visited)
                {
                    if import.names.iter().any(|item| item == "*" || item == &name)
                    { result.entry(name).or_insert(symbol); }
                }
            }
        }
        result
    }

    fn namespace_members(&self, symbol: &Symbol, home: &str) -> Vec<Symbol>
    {
        if symbol.initializer.is_empty()
        { return self.exported_symbols(&symbol.ty, symbol.name == "super", 0)
            .into_values().collect(); }
        let prefix = format!("{}::", symbol.initializer);
        if symbol.owner == NATIVE
        {
            let mut result = BTreeMap::new();
            for (key, symbol) in &self.native
            {
                if let Some(tail) = key.strip_prefix(&prefix)
                {
                    let mut symbol = symbol.clone();
                    symbol.name = tail.into();
                    result.insert(tail.to_string(), symbol);
                }
            }
            return result.into_values().collect();
        }
        let Some(module) = self.modules.get(&symbol.owner) else { return Vec::new(); };
        let mut result: BTreeMap<String, Symbol> = BTreeMap::new();
        for import in &module.imports
        {
            if !import.names.is_empty() || symbol.owner != home && !import.public { continue; }
            let Some(tail) = import.name.strip_prefix(&prefix) else { continue; };
            let name = tail.split("::").next().unwrap();
            let mut member = Symbol::new(name, Kind::Module, &import.target,
                &symbol.owner, import.public);
            if tail.contains("::") { member.initializer = format!("{}{}", prefix, name); }
            result.insert(name.into(), member);
        }
        result.into_values().collect()
    }

    fn locals(&self, home: &str, scope: usize, offset: usize) -> BTreeMap<String, Symbol>
    {
        let Some(module) = self.modules.get(home) else { return self.native.clone(); };
        let mut result = self.native.clone();
        for name in module.visible.keys()
        {
            if let Some(symbol) = self.hidden.get(name)
            { result.insert(name.clone(), symbol.clone()); }
        }
        for import in &module.imports
        {
            if import.names.is_empty()
            { result.insert(import.name.clone(), Symbol::new(&import.name, Kind::Module,
                &import.target, home, import.public)); }
            else
            {
                for (name, symbol) in self.exported_symbols(&import.target, import.parent, 0)
                {
                    if import.names.iter().any(|item| item == "*" || item == &name)
                    { result.insert(name, symbol); }
                }
            }
        }
        let mut scopes = vec![scope];
        while let Some(parent) = module.scopes[*scopes.last().unwrap()].parent
        { scopes.push(parent); }
        for index in scopes.into_iter().rev()
        {
            for (name, symbol) in &module.scopes[index].symbols
            {
                let symbol = module.scopes[index].history.get(name).and_then(|versions|
                    versions.iter().rev().find(|symbol|
                        symbol.span.is_none_or(|span| span.start <= offset))).unwrap_or(symbol);
                if symbol.kind != Kind::Variable || symbol.span
                    .is_none_or(|span| span.start <= offset)
                { result.insert(name.clone(), symbol.clone()); }
            }
        }
        result
    }

    fn description(&self, symbol: &Symbol, home: &str, scope: usize) -> String
    {
        if symbol.kind == Kind::Type
        {
            let mut text = symbol.signature.clone();
            if text.is_empty() { text = format!("type {}", symbol.name); }
            let members = self.members(&symbol.ty, home, scope, false, false);
            let fields: Vec<_> = members.iter().filter(|member| member.kind == Kind::Field)
                .filter(|member| member.name != "type").map(|member|
                    format!("    {}: {}", member.name, self.type_label(member))).collect();
            if !fields.is_empty() { text.push_str(&format!("\n{{\n{}\n}}", fields.join("\n"))); }
            let methods = self.members(&symbol.ty, home, scope, false, true);
            for method in methods.iter().filter(|method| method.kind == Kind::Method)
            { text.push('\n'); text.push_str(&method.signature); }
            return text;
        }
        if !symbol.signature.is_empty() { return symbol.signature.clone(); }
        match symbol.kind
        {
            Kind::External => format!("fn {}($args: any...): any", symbol.name),
            Kind::Module => format!("module {}", symbol.name),
            Kind::Variant => format!("{}::{}", self.type_label(symbol), symbol.name),
            Kind::Variable => format!("let {}: {}", symbol.name, self.type_label(symbol)),
            _ => format!("{}: {}", symbol.name, self.type_label(symbol)),
        }
    }

    fn markdown(&self, symbol: &Symbol, home: &str, scope: usize) -> String
    {
        if symbol.kind != Kind::Type
        {
            let heading = match symbol.kind
            {
                Kind::Method => "Method", Kind::Function => "Function",
                Kind::External => "External command", Kind::Variable => "Variable",
                Kind::Field if symbol.property.is_some() => "Property",
                Kind::Field => "Field", Kind::Module => "Module",
                Kind::Variant => "Enum variant", Kind::Alias => "Alias", _ => "Type",
            };
            let mut text = format!("**{}**\n\n{}", heading,
                code_block(&self.description(symbol, home, scope)));
            if let Some((read, write)) = symbol.property
            {
                text.push_str(match (read, write)
                {
                    (true, true) => "\n\nReadable and writable.",
                    (true, false) => "\n\nRead-only property.",
                    _ => "\n\nWrite-only property.",
                });
            }
            return text;
        }
        let Some(ty) = self.types.get(&symbol.ty) else
            { return code_block(&self.description(symbol, home, scope)); };
        let mut definition = if symbol.signature.is_empty() { format!("type {}", symbol.name) }
            else { symbol.signature.clone() };
        if !ty.fields.is_empty()
        {
            let fields: Vec<_> = ty.fields.values().filter(|field| field.public)
                .map(|field| format!("    {}", field.signature)).collect();
            definition.push_str(&format!("\n{{\n{}\n}}", fields.join("\n")));
        }
        if !ty.variants.is_empty()
        { definition.push_str(&format!("\n{{ {} }}", ty.variants.join(", "))); }
        let mut text = format!("**{}**\n\n{}", symbol.name, code_block(&definition));
        let properties: Vec<_> = self.members(&symbol.ty, home, scope, false, false)
            .into_iter().filter(|member| member.property.is_some()).collect();
        if !properties.is_empty()
        {
            text.push_str("\n\n**Properties**\n");
            for member in properties
            {
                let access = match member.property.unwrap()
                { (true, true) => "read/write", (true, false) => "read-only", _ => "write-only" };
                text.push_str(&format!("\n- {} · {}",
                    inline_code(&format!("{}: {}", member.name, self.type_label(&member))),
                    access));
            }
        }
        let methods: Vec<_> = self.members(&symbol.ty, home, scope, false, true)
            .into_iter().filter(|member| member.kind == Kind::Method).collect();
        if !methods.is_empty()
        {
            text.push_str("\n\n**Methods**\n");
            for member in methods
            { text.push_str(&format!("\n- {}", inline_code(&member.signature))); }
        }
        text
    }

    fn type_label(&self, symbol: &Symbol) -> String
    {
        if let Some(ty) = self.types.get(&symbol.ty)
        { return ty.symbol.as_ref().map_or("any", |symbol| symbol.name.as_str()).into(); }
        if !symbol.ty.is_empty() { return symbol.ty.clone(); }
        if !symbol.initializer.is_empty()
        {
            let (_, statements) = analyze_editor_tree_with_limits(&symbol.initializer,
                MAX_BYTES, MAX_STEPS);
            let expression = statements.first().and_then(|statement| match statement
            {
                AstStatement::ExpressionStatement(expression) => Some(expression),
                _ => None,
            });
            let constructor = expression.and_then(|expression| match &expression.kind
            {
                AstExpressionKind::StructConstructor(item) => Some(item.name.as_str()),
                AstExpressionKind::SpacedEmptyCall(name) => Some(name.as_str()),
                _ => None,
            });
            if let Some(name) = constructor
                && let Some(key) = self.type_key(&symbol.owner, symbol.scope, name, 0)
                && let Some(method) = self.members(&key, &symbol.owner, symbol.scope, false, true)
                    .into_iter().find(|method| method.name == "new" && method.ty.contains(" | ()"))
            { return method.ty; }
        }
        self.symbol_type(symbol, 0).and_then(|key| self.types.get(&key))
            .and_then(|ty| ty.symbol.as_ref()).map_or("any", |symbol| symbol.name.as_str()).into()
    }

    fn word_symbol(&self, home: &str, scope: usize, word: &str, offset: usize) -> Option<Symbol>
    {
        if let Some(name) = word.strip_prefix("::")
        { return self.commands.contains(name)
            .then(|| Symbol::new(name, Kind::External, "any", NATIVE, true)); }
        if let Some(symbol) = self.resolve(home, scope, word, offset, false, 0)
        { return Some(symbol); }
        if let Some((receiver, name)) = word.rsplit_once('.')
        {
            if name.starts_with('$') || name.starts_with("::$")
            { return self.resolve(home, scope, name.trim_start_matches("::"), offset, false, 0); }
            let key = self.infer_text(home, scope, receiver, offset, 0)
                .or_else(|| self.infer_text(home, scope, &format!("${}", receiver), offset, 0))?;
            return self.members(&key, home, scope, name.starts_with("::"), false)
                .into_iter().find(|symbol| symbol.name == name.trim_start_matches("::"));
        }
        if let Some((receiver, name)) = word.rsplit_once("::")
        {
            let key = self.type_key(home, scope, receiver, 0)?;
            return self.members(&key, home, scope, false, true).into_iter()
                .find(|symbol| symbol.name == name);
        }
        if self.commands.contains(word)
        { return Some(Symbol::new(word, Kind::External, "any", NATIVE, true)); }
        None
    }

    fn ignored(&self, home: &str, offset: usize) -> bool
    {
        let Some(module) = self.modules.get(home) else { return false; };
        if module.analysis.lexical.trivia.iter().any(|trivia|
            trivia.kind == TriviaKind::Comment && trivia.span.start <= offset
                && offset < trivia.span.end)
        { return true; }
        for string in &module.analysis.lexical.strings
        {
            if string.span.start <= offset && offset < string.span.end
            {
                if !string.interpolated { return true; }
                let escaped: Vec<_> = string.parts.iter().filter(|part|
                    part.kind == StringPartKind::Escape
                        && part.decoded.slice(&string.decoded) == Some("$"))
                    .map(|part| part.decoded.start).collect();
                return !interpolation_references(&string.decoded, &escaped).iter().any(|reference|
                    string.source_span(reference.span).is_some_and(|span|
                        span.start <= offset && offset <= span.end));
            }
        }
        false
    }

    pub fn hover(&self, home: &str, offset: usize) -> Value
    {
        self.operations.set(40_000);
        let Some(module) = self.modules.get(home) else { return Value::Null; };
        if self.ignored(home, offset) { return Value::Null; }
        let scope = module.scope_at(offset);
        let member = module.analysis.tokens.iter().find(|token|
            token.kind == TokenKind::Member && token.span.start <= offset 
                && offset < token.span.end);
        let (span, symbol) = if let Some(token) = member
        {
            let name = token.token_value_text();
            let raw = name.starts_with("::");
            let field = name.trim_start_matches("::");
            let symbol = if field.starts_with('$')
                { self.resolve(home, scope, field, offset, false, 0) }
                else
                {
                    let receiver = receiver_text(&module.source, token.span.start);
                    self.infer_text(home, scope, receiver, offset, 0).and_then(|key|
                        self.members(&key, home, scope, raw, false).into_iter()
                            .find(|symbol| symbol.name == field))
                };
            (token.span, symbol)
        }
        else
        {
            let Some((mut span, full)) = word_at(&module.source, offset) 
                else { return Value::Null; };
            let relative = offset - span.start;
            let end = full.char_indices().find(|(index, character)|
                *index > relative && (*character == '.' || *character == ':'))
                .map_or(full.len(), |(index, _)| index);
            let word = full[..end].trim_end_matches(':');
            span.end = span.start + word.len();
            let declaration = self.types.values().flat_map(|ty| ty.fields.values()).find(|field|
                field.owner == home && field.span.is_some_and(|span|
                    span.start <= offset && offset < span.end)).cloned();
            let word = if module.analysis.lexical.strings.iter().any(|string|
                string.span.start <= offset && offset < string.span.end) && !word.starts_with('$')
                { format!("${}", word) } else { word.into() };
            (span, declaration.or_else(|| self.word_symbol(home, scope, &word, offset)))
        };
        let Some(symbol) = symbol else { return Value::Null; };
        json!({ "contents": { "kind": "markdown", "value":
            self.markdown(&symbol, home, scope) }, "range": range(&module.source, span) })
    }

    fn constructor_arguments(&self, home: &str, scope: usize, source: &str,
                             start: usize) -> Option<Vec<Symbol>>
    {
        let before = &source[..start];
        let (analysis, _) = analyze_editor_tree_with_limits(before, MAX_BYTES, MAX_STEPS);
        let mut depth = 0;
        let mut opening = None;
        for token in analysis.tokens.iter().rev()
        {
            match token.kind
            {
                TokenKind::ParenClose | TokenKind::SquareClose | TokenKind::BlockClose =>
                    depth += 1,
                TokenKind::ParenOpen if depth == 0 => { opening = Some(token.span); break; },
                TokenKind::ParenOpen | TokenKind::SquareOpen | TokenKind::IndexOpen
                | TokenKind::BlockOpen if depth > 0 => depth -= 1,
                _ => {},
            }
        }
        let opening = opening?;
        let tail = before[opening.end..].trim();
        if tail.contains(':') && !tail.ends_with(',') { return None; }
        let name = receiver_text(source, opening.start).trim();
        let symbol = self.resolve(home, scope, name, start, false, 0)?;
        if symbol.kind != Kind::Type { return None; }
        let ty = self.types.get(&symbol.ty)?;
        let constructor = self.members(&symbol.ty, home, scope, false, true)
            .into_iter().find(|method| method.name == "new");
        let raw = self.modules.get(home).is_some_and(|module|
        {
            let mut current = Some(scope);
            while let Some(index) = current
            {
                let scope = &module.scopes[index];
                if scope.factory && scope.receiver.as_ref().and_then(|receiver|
                    self.type_key(home, index, receiver, 0)).as_deref() == Some(&symbol.ty)
                { return true; }
                current = scope.parent;
            }
            false
        });
        let has_factory = self.modules.iter().any(|(module_home, module)|
            module.methods.iter().any(|(receiver, method)| method.name == "new"
                && self.type_key(module_home, method.scope, receiver, 0).as_deref()
                    == Some(&symbol.ty)));
        if has_factory && constructor.is_none() && !raw { return Some(Vec::new()); }
        let mut arguments = if let Some(constructor) = constructor.filter(|_| !raw)
        {
            constructor.parameters.into_iter().map(|(name, ty)|
                Symbol::new(&name, Kind::Field, &ty, home, true)).collect::<Vec<_>>()
        }
        else { ty.fields.values().filter(|field| field.public || raw).cloned().collect() };
        arguments.retain(|argument|
            !before[opening.end..].contains(&format!("{}:", argument.name)));
        for argument in &mut arguments
        {
            argument.argument = true;
            argument.signature = format!("{}: {}", argument.name, argument.ty);
        }
        Some(arguments)
    }

    pub fn completion(&self, home: &str, offset: usize) -> Value
    {
        self.operations.set(40_000);
        let Some(module) = self.modules.get(home) else { return json!([]); };
        let source = &module.source;
        if self.ignored(home, offset) { return json!([]); }
        let scope = module.scope_at(offset);
        let start = source[..offset].char_indices().rev().find(|(_, character)|
            !character.is_alphanumeric() && *character != '_' && *character != '$')
            .map_or(0, |(index, character)| index + character.len_utf8());
        let prefix = &source[start..offset];
        let before = &source[..start];
        let arguments = self.constructor_arguments(home, scope, source, start);
        let mut symbols = if let Some(arguments) = arguments { arguments }
        else if prefix.starts_with('$')
        { self.locals(home, scope, offset).into_values().filter(|symbol|
            symbol.kind == Kind::Variable && (!(before.ends_with('.') || before.ends_with(".::"))
                || self.symbol_type(symbol, 0)
                    .is_none_or(|key| self.field_name_type(&key, 0)))).collect() }
        else if before.ends_with('.') || before.ends_with(".::")
        {
            let raw = before.ends_with(".::");
            let end = start - if raw { 3 } else { 1 };
            let text = interpolation_receiver(source, end)
                .unwrap_or_else(|| receiver_text(source, end).into());
            self.infer_text(home, scope, &text, offset, 0).map_or(Vec::new(), |key|
                self.members(&key, home, scope, raw, false))
        }
        else if before.ends_with("::")
        {
            let end = start - 2;
            let text = receiver_text(source, end);
            if let Some(symbol) = self.resolve(home, scope, text, offset, false, 0)
            {
                if symbol.kind == Kind::Module
                {
                    self.namespace_members(&symbol, home).into_iter().filter_map(|mut symbol|
                    {
                        if text.starts_with('$')
                        {
                            if symbol.kind == Kind::Variable
                            {
                                symbol.signature = format!("let {}: {}", symbol.name,
                                    self.type_label(&symbol));
                                symbol.name = symbol.name.trim_start_matches('$').into();
                            }
                            else if symbol.kind != Kind::Module { return None; }
                        }
                        Some(symbol)
                    }).collect()
                }
                else if symbol.kind == Kind::Type
                { self.members(&symbol.ty, home, scope, false, true) }
                else { Vec::new() }
            }
            else { Vec::new() }
        }
        else
        {
            let mut symbols: Vec<_> = self.locals(home, scope, offset).into_values().collect();
            symbols.extend(self.commands.iter().filter(|name| !self.native.contains_key(*name))
                .map(|name| Symbol::new(name, Kind::External, "any", NATIVE, true)));
            symbols
        };
        symbols.sort_by(|left, right| left.name.cmp(&right.name));
        symbols.dedup_by(|left, right| left.name == right.name);
        let matching: Vec<_> = symbols.into_iter().filter(|symbol| symbol.name.starts_with(prefix)
            && !symbol.name.is_empty()).collect();
        let incomplete = matching.len() > 512;
        let items: Vec<_> = matching.into_iter().take(512).map(|symbol|
            {
                let kind = match symbol.kind
                {
                    Kind::Method => 2, Kind::Function | Kind::External => 3,
                    Kind::Field => 10, Kind::Variable => 6, Kind::Type => 7,
                    Kind::Module => 9, Kind::Variant => 20, Kind::Alias => 3,
                };
                json!({ "label": symbol.name, "kind": kind,
                    "detail": if symbol.kind == Kind::Type { symbol.signature.clone() }
                        else { self.description(&symbol, home, scope) },
                    "documentation": { "kind": "markdown",
                                       "value": self.markdown(&symbol, home, scope) },
                    "textEdit": { "range": range(source, SourceSpan::new(start, offset)),
                                  "newText": if symbol.argument
                                      { format!("{}: ", symbol.name) }
                                       else { symbol.name.clone() } } })
            }).collect();
        if incomplete { json!({ "isIncomplete": true, "items": items }) } else { json!(items) }
    }
}

fn word_at(source: &str, offset: usize) -> Option<(SourceSpan, &str)>
{
    let name = |character: char| character.is_alphanumeric()
        || matches!(character, '_' | '$' | ':' | '.');
    let start = source[..offset].char_indices().rev().find(|(_, character)| !name(*character))
        .map_or(0, |(index, character)| index + character.len_utf8());
    let end = source[offset..].char_indices().find(|(_, character)| !name(*character))
        .map_or(source.len(), |(index, _)| offset + index);
    (end > start).then(|| (SourceSpan::new(start, end), &source[start..end]))
}

fn receiver_text(source: &str, end: usize) -> &str
{
    let (analysis, _) = analyze_editor_tree_with_limits(&source[..end], MAX_BYTES, MAX_STEPS);
    let mut depth = 0;
    let mut start = end;
    for (index, token) in analysis.tokens.iter().enumerate().rev().take(256)
    {
        let previous = index.checked_sub(1).and_then(|index| analysis.tokens.get(index));
        match token.kind
        {
            TokenKind::ParenClose | TokenKind::SquareClose => depth += 1,
            TokenKind::ParenOpen | TokenKind::SquareOpen | TokenKind::IndexOpen if depth > 0 =>
            {
                depth -= 1;
                start = token.span.start;
                if depth == 0 && (token.kind == TokenKind::SquareOpen
                    || previous.is_none_or(|previous| previous.span.end != token.span.start))
                { break; }
            },
            _ if depth > 0 => {},
            TokenKind::Member | TokenKind::Scope => {},
            TokenKind::Identifier | TokenKind::Symbol | TokenKind::Literal =>
            {
                start = token.span.start;
                if previous.is_none_or(|previous| previous.kind != TokenKind::Scope) { break; }
            },
            _ => break,
        }
        start = token.span.start;
    }
    source.get(start..end).unwrap_or_default().trim()
}


pub(in crate::server) fn offset(source: &str, line: u64, character: u64) -> Option<usize>
{
    let mut row = 0;
    let mut column = 0;
    let mut carriage = false;
    for (index, ch) in source.char_indices()
    {
        if ch == '\n' && carriage { carriage = false; continue; }
        if row == line && column == character { return Some(index); }
        if row == line && (column > character || matches!(ch, '\r' | '\n')) { return None; }
        match ch
        {
            '\r' => { row += 1; column = 0; },
            '\n' if carriage => {},
            '\n' => { row += 1; column = 0; },
            _ => column += ch.len_utf16() as u64,
        }
        carriage = ch == '\r';
    }
    (row == line && column == character).then_some(source.len())
}

fn position(source: &str, offset: usize) -> Value
{
    let mut line = 0;
    let mut character = 0;
    let mut carriage = false;
    for ch in source[..offset].chars()
    {
        match ch
        {
            '\r' => { line += 1; character = 0; },
            '\n' if carriage => {},
            '\n' => { line += 1; character = 0; },
            _ => character += ch.len_utf16(),
        }
        carriage = ch == '\r';
    }
    json!({ "line": line, "character": character })
}

fn range(source: &str, span: SourceSpan) -> Value
{ json!({ "start": position(source, span.start), "end": position(source, span.end) }) }

fn fence(text: &str, minimum: usize) -> String
{
    let longest = text.split(|character| character != '`').map(str::len).max().unwrap_or(0);
    "`".repeat(minimum.max(longest + 1))
}

fn code_block(text: &str) -> String
{
    let fence = fence(text, 3);
    format!("{}shy\n{}\n{}", fence, text, fence)
}

fn inline_code(text: &str) -> String
{
    let fence = fence(text, 1);
    format!("{}{}{}", fence, text, fence)
}

fn interpolation_receiver(source: &str, end: usize) -> Option<String>
{
    let beginning = source[..end].rfind("${")? + 2;
    let name = &source[beginning..end];
    if !name.is_empty() && name.chars().all(|character|
        character.is_alphanumeric() || matches!(character, '_' | '$' | ':' | '.'))
    { Some(format!("${}", name)) } else { None }
}
