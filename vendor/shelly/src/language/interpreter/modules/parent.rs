
use std::{ collections::HashSet,
           mem::{ replace, take }, path::{ Path, PathBuf } };

use crate::language::{ ast::{ AstImportStatement, AstStatement, AstTopLevel, AstType },
                       data::{ methods::method_key, types::TypeId },
                       interpreter::{ Interpreter, InterpreterResult,
                                      modules::{ ModuleSource, PendingModule, module_error } },
                       text::location::Location,
                       typecheck::check_protocol };


fn copy_import(import: &AstImportStatement) -> AstImportStatement
{
    AstImportStatement
        {
            public: import.public, location: import.location.clone(), module: import.module.clone(),
            names: import.names.clone(), condition: None, enabled: import.enabled,
        }
}


fn annotation_names(annotation: &AstType, names: &mut HashSet<String>)
{
    match annotation
    {
        AstType::Named(name) => { names.insert(name.clone()); },
        AstType::Array(item) => annotation_names(item, names),
        AstType::Map(key, value) =>
            { annotation_names(key, names); annotation_names(value, names); },
        AstType::FixedArray(items) | AstType::Union(items) =>
            { for item in items { annotation_names(item, names); } },
        AstType::Function(arguments, result) =>
            {
                for argument in arguments { annotation_names(argument, names); }
                annotation_names(result, names);
            },
        AstType::ReceiverMember(_) => {},
    }
}


impl ModuleSource
{
    fn parent(&self, location: &Location) -> InterpreterResult<Option<Self>>
    {
        match self
        {
            Self::File { path, library } =>
                {
                    let mut directory = path.parent().unwrap();
                    if path.file_name().is_some_and(|name| name == "init.shy")
                    {
                        let Some(parent) = directory.parent() else { return Ok(None); };
                        directory = parent;
                    }
                    let mut files = Vec::new();
                    if directory.file_name().is_some()
                    {
                        let mut adjacent = directory.as_os_str().to_os_string();
                        adjacent.push(".shy");
                        files.push(PathBuf::from(adjacent));
                    }
                    files.push(directory.join("init.shy"));
                    Ok(Self::files(&files, library, location)?
                        .filter(|parent| parent.origin() != self.origin()))
                },
            Self::Bundled { name, .. } =>
                {
                    let mut directory = Path::new(name).parent().unwrap_or(Path::new(""));
                    if Path::new(name).file_name().is_some_and(|name| name == "init")
                    { directory = directory.parent().unwrap_or(Path::new("")); }
                    if directory.as_os_str().is_empty() { return Ok(None); }
                    Self::bundled(&directory.to_string_lossy(), Path::new(""), location)
                },
        }
    }

    pub(super) fn contains_child(&self, source: &Self) -> bool
    {
        if self.origin() == source.origin() { return false; }
        match (self, source)
        {
            (Self::File { path, .. }, Self::File { path: child, .. }) =>
                {
                    let directory = if path.file_name().is_some_and(|name| name == "init.shy")
                        { path.parent().unwrap().to_path_buf() }
                        else { path.with_extension("") };
                    child.starts_with(directory)
                },
            (Self::Bundled { name, .. }, Self::Bundled { name: child, .. }) =>
                {
                    let directory = name.strip_suffix("/init").unwrap_or(name);
                    child.starts_with(&format!("{}/", directory))
                },
            _ => false,
        }
    }
}


impl Interpreter
{
    pub(super) fn discard_parent_scopes(&mut self, existing: &HashSet<String>)
    {
        // Successful imports and reloads remain valid after a later runtime error.
        // Remove unfinished headers and then children whose parent binding is gone.
        loop
        {
            let discarded: Vec<_> = self.scopes.iter().filter(|(key, scope)|
                !existing.contains(*key) && (self.pending_modules.contains_key(*key)
                    || self.module_sources.get(*key).is_some_and(|source|
                        self.loading_modules.contains(&source.origin()))
                    || scope.modules.values().any(|module| !self.scopes.contains_key(module))))
                .map(|(key, _)| key.clone()).collect();
            if discarded.is_empty() { break; }
            for key in discarded { self.discard_module_scope(&key); }
        }
    }

    pub(super) fn discard_module_scope(&mut self, key: &str)
    {
        if let Some(source) = self.module_sources.remove(key)
        { self.loading_modules.remove(&source.origin()); }
        self.scopes.remove(key);
        self.pending_modules.remove(key);
        self.type_scopes.retain(|_, home| home != key);
    }

    pub(super) fn stage_parent_types(&mut self, statements: &mut AstTopLevel,
                                    imports: &[AstImportStatement])
    {
        if !self.module_sources.contains_key(&self.current_scope) { return; }
        let mut types = Vec::new();
        let mut body = Vec::new();
        let mut methods = Vec::new();
        for statement in take(statements)
        {
            if matches!(statement, AstStatement::StructDeclaration(_)
                | AstStatement::EnumDeclaration(_) | AstStatement::TypeDeclaration(_))
            { types.push(statement); continue; }
            if let AstStatement::FunctionDefinition(function) = &statement
            {
                if let Some(receiver) = &function.receiver
                { methods.push((receiver.clone(), function.name.clone(), function.public)); }
                else
                {
                    self.scope_mut().announce_function(function.name.clone());
                    if function.public { self.scope_mut().exports.insert(function.name.clone()); }
                    else { self.scope_mut().exports.remove(&function.name); }
                }
            }
            body.push(statement);
        }
        self.pending_modules.insert(self.current_scope.clone(), PendingModule
            {
                types: Some(types), imports: imports.iter().map(copy_import).collect(),
                body: Some(body), methods, preparing: false, deferred: false,
            });
    }

    pub(super) fn prepare_parent_types(&mut self, key: &str) -> InterpreterResult<()>
    {
        let Some(pending) = self.pending_modules.get_mut(key) else { return Ok(()); };
        if pending.preparing
        {
            let origin = self.module_sources[key].origin();
            return Err(module_error(&Location::new(origin.as_str(), 1, 1),
                "Circular parent type dependency"));
        }
        let Some(mut types) = pending.types.take() else { return Ok(()); };
        pending.preparing = true;
        let imports: Vec<_> = pending.imports.iter().map(copy_import).collect();
        let mut body = pending.body.take().unwrap_or_default();
        let methods = pending.methods.clone();
        let source = self.module_sources[key].clone();
        let caller = replace(&mut self.current_scope, key.to_string());
        let result = (||
            {
                self.prepare_visibility(&body)?;
                let mut needed = HashSet::new();
                let mut declared = HashSet::new();
                for statement in &types
                {
                    for (name, _, _) in statement.module_declarations()
                    { declared.insert(name.to_string()); }
                    match statement
                    {
                        AstStatement::StructDeclaration(item) =>
                            {
                                for field in &item.fields
                                { annotation_names(&field.annotation, &mut needed); }
                            },
                        AstStatement::TypeDeclaration(item) =>
                            annotation_names(&item.annotation, &mut needed),
                        _ => {},
                    }
                }
                for statement in &body
                {
                    if let AstStatement::FunctionDefinition(function) = statement
                        && function.receiver.is_some()
                        && (function.name == "new" || function.name.starts_with("get_")
                            || function.name.starts_with("set_"))
                    {
                        for parameter in &function.parameters
                        {
                            if let Some(annotation) = &parameter.annotation
                            { annotation_names(annotation, &mut needed); }
                        }
                        if let Some(annotation) = &function.return_annotation
                        { annotation_names(annotation, &mut needed); }
                    }
                }
                needed.retain(|name| !declared.contains(name)
                    && !self.scope().types.names.contains_key(name));
                // Load only imports needed by the parent's type declarations. Other
                // children wait until these types are available to their super imports.
                for import in &imports
                {
                    if !import.enabled { continue; }
                    let required = needed.iter().any(|name|
                        if import.names.is_empty()
                        { name.starts_with(&format!("{}::", import.module)) }
                        else { import.names.contains(name) });
                    if !required { continue; }
                    let dependency = self.load_import(import, &source.directory())?;
                    self.install_import(import, &dependency)?;
                }
                self.refresh_module_names()?;
                let first = self.scope().types.definition_count();
                self.scope_mut().compile(&mut types)?;
                for index in first..self.scope().types.definition_count()
                { self.type_scopes.insert(TypeId(index), key.to_string()); }
                for statement in &types
                {
                    for (name, _, public) in statement.module_declarations()
                    {
                        if public { self.scope_mut().exports.insert(name.to_string()); }
                        else { self.scope_mut().exports.remove(name); }
                    }
                    if let AstStatement::TypeDeclaration(declaration) = statement
                        && let Some(factory) = &declaration.conversion
                    {
                        let name = method_key(factory.receiver_type.unwrap(), &factory.name);
                        if declaration.public { self.scope_mut().exports.insert(name); }
                    }
                }
                let names = self.scope().types.names.clone();
                for statement in &mut body
                {
                    if let AstStatement::FunctionDefinition(function) = statement
                        && let Some(receiver) = &function.receiver
                        && let Some(id) = names.get(receiver)
                    {
                        function.receiver_type = Some(*id);
                        check_protocol(&mut self.scope_mut().types, function, &names)
                            .map_err(|error| module_error(&function.location, error.to_string()))?;
                    }
                }
                for (receiver, name, public) in methods
                {
                    let Some(id) = self.scope().types.names.get(&receiver).copied()
                        else { continue; };
                    self.scope_mut().types.declare_extension(id, &name);
                    let key = method_key(id, &name);
                    self.scope_mut().mask_implied_factory(&key);
                    if public { self.scope_mut().exports.insert(key); }
                    else { self.scope_mut().exports.remove(&key); }
                }
                Ok(())
            })();
        self.current_scope = caller;
        if let Some(pending) = self.pending_modules.get_mut(key)
        {
            pending.preparing = false;
            pending.body = Some(body);
        }
        result
    }

    pub(super) fn load_parent_import(&mut self, import: &AstImportStatement)
        -> InterpreterResult<String>
    {
        let segments: Vec<_> = import.module.split("::").collect();
        let ancestors = segments.iter().take_while(|name| **name == "super").count();
        let mut source = self.module_sources.get(&self.current_scope).cloned()
            .ok_or_else(|| module_error(&import.location, "This input has no containing module"))?;
        let mut parent = String::new();
        for _ in 0..ancestors
        {
            source = source.parent(&import.location)?.ok_or_else(||
                module_error(&import.location, "The containing module has no parent module"))?;
            let origin = source.origin();
            parent = self.pending_modules.keys().find(|key|
                self.module_sources[*key].origin() == origin).cloned().unwrap_or(origin.clone());
            if !self.scopes.contains_key(&parent)
            {
                let (mut body, imports) =
                    self.open_module_scope(&source, &import.location, &parent)?;
                self.loading_modules.insert(origin);
                let caller = replace(&mut self.current_scope, parent.clone());
                let result = self.install_prelude(&import.location);
                if result.is_ok()
                {
                    self.stage_parent_types(&mut body, &imports);
                    self.pending_modules.get_mut(&parent).unwrap().deferred = true;
                }
                self.current_scope = caller;
                result?;
            }
            self.prepare_parent_types(&parent)?;
        }
        if ancestors < segments.len()
        {
            let mut nested = copy_import(import);
            nested.module = segments[ancestors..].join("::");
            return self.load_import(&nested, &source.directory());
        }
        Ok(parent)
    }

    pub(super) fn finish_parent_modules(&mut self) -> InterpreterResult<()>
    {
        let ready = self.pending_modules.iter().find(|(parent, pending)| pending.deferred
            && !self.pending_modules.iter().any(|(key, pending)| !pending.deferred
                && self.module_sources[*parent].contains_child(&self.module_sources[key])));
        let Some((key, _)) = ready else { return Ok(()); };
        let key = key.clone();
        let mut pending = self.pending_modules.remove(&key).unwrap();
        let source = self.module_sources[&key].clone();
        let mut body = pending.body.take().unwrap_or_default();
        let caller = replace(&mut self.current_scope, key.clone());
        let previous = self.last_result.take();
        let result = self.compile_module(&mut body, pending.imports, &source.directory())
            .and_then(|code|
                {
                    self.refresh_parent_children(&key)?;
                    self.execute_instructions(&code)
                });
        self.last_result = previous;
        self.current_scope = caller;
        self.loading_modules.remove(&source.origin());
        if let Err(error) = result
        {
            self.discard_module_scope(&key);
            return Err(error);
        }
        self.finish_parent_modules()
    }

    pub(super) fn refresh_parent_children(&mut self, parent: &str) -> InterpreterResult<()>
    {
        // Resolve the public extensions that children saw before their bodies existed.
        // Keep the resulting function handles in each child's import snapshot.
        let children: Vec<_> = self.scopes.iter().filter(|(key, scope)| *key != parent
            && scope.parent_namespaces.iter().any(|name|
                scope.modules.get(name).is_some_and(|key| key == parent)))
            .map(|(key, _)| key.clone()).collect();
        let caller = self.current_scope.clone();
        let result = (||
            {
                for child in children
                {
                    self.current_scope = child;
                    self.refresh_module_names()?;
                }
                Ok(())
            })();
        self.current_scope = caller;
        result
    }
}
