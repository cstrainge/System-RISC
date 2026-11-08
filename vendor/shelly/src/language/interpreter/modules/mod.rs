use crate::runtime::paths::split_paths;

use std::{ cell::RefCell,
           collections::{ HashMap, HashSet },
           env::{ current_dir },
           fs::{ canonicalize, File },
           io::{ BufReader, ErrorKind },
           mem::{ replace, take },
           path::{ Path, PathBuf },
           rc::Rc };

use crate::language::{
    ast::{AstExpression, AstExpressionKind, AstImportStatement, AstStatement, AstTopLevel},
    bytecode::{FunctionRef, Instruction},
    data::{
        scoped_variables::{ScopedValue, ValueReference, ValueVisibility},
        types::TypeId,
        value::Value,
        methods::{method_key, conversion_key},
        conversion::is_conversion_method,
    },
    native::NativeFunction,
    interpreter::{
        bundled_std::{source as bundled_source, has_children as bundled_has_children},
        ErrorWhat, Interpreter, InterpreterError, InterpreterResult,
        scope::{Alias, Scope},
    },
    exclusions::exclude_statements,
    text::{buffer::SimpleBuffer, location::Location, read_buffer::ReadBuffer},
};

mod parent;


// Each prelude generation retains its own scope and type handles. Changing the
// search path alone does not replace it; prelude_reload publishes a new generation.
#[derive(Clone)]
pub(super) struct Prelude
{
    scope: String,
    source: String,
}


#[derive(Default)]
struct ModuleBindings
{
    types: HashMap<String, TypeId>,
    functions: HashSet<String>,
    methods: HashMap<String, FunctionRef>,
    forward_methods: HashSet<String>,
}


#[derive(Clone)]
pub(super) enum ModuleSource
{
    File { path: PathBuf, library: PathBuf },
    Bundled { name: String, source: &'static str },
}


pub(super) struct PendingModule
{
    types: Option<AstTopLevel>,
    imports: Vec<AstImportStatement>,
    body: Option<AstTopLevel>,
    methods: Vec<(String, String, bool)>,
    preparing: bool,
    deferred: bool,
}


enum ModuleDirectory
{
    File(PathBuf),
    Bundled(PathBuf),
}


impl ModuleSource
{
    fn library_location(&self) -> String
    {
        match self
        {
            Self::File { library, .. } => library.to_string_lossy().into_owned(),
            Self::Bundled { .. } => "embedded".into(),
        }
    }

    fn origin(&self) -> String
    {
        match self
        {
            Self::File { path, .. } => path.to_string_lossy().into_owned(),
            Self::Bundled { name, .. } => format!("<bundled:std::{}>", name.replace('/', "::")),
        }
    }

    fn directory(&self) -> ModuleDirectory
    {
        match self
        {
            Self::File { path, .. } => ModuleDirectory::File(
                path.parent().unwrap_or_else(|| Path::new(".")).to_path_buf()),
            Self::Bundled { name, .. } => ModuleDirectory::Bundled(
                Path::new(name).parent().unwrap_or_else(|| Path::new("")).to_path_buf()),
        }
    }

    fn files(
        files: &[PathBuf],
        library: &Path,
        location: &Location,
    ) -> InterpreterResult<Option<Self>>
    {
        if let [file, entry] = files
            && file.is_file()
            && entry.parent().is_some_and(Path::is_dir)
        {
            return Err(module_error(
                location,
                format!(
                    "Ambiguous module: both '{}' and directory '{}' exist; use its init.shy root",
                    file.display(),
                    entry.parent().unwrap().display()
                ),
            ));
        }
        let mut found: Option<(PathBuf, &PathBuf)> = None;
        for file in files
        {
            match canonicalize(file)
            {
                Ok(path) if path.is_file() =>
                    {
                        if let Some((_, previous)) = &found
                        {
                            return Err(module_error(location, format!(
                                "Ambiguous module: both '{}' and '{}' exist",
                                previous.display(), file.display())));
                        }
                        found = Some((path, file));
                    },
                Ok(_) => {},
                Err(error) if matches!(error.kind(),
                    ErrorKind::NotFound | ErrorKind::NotADirectory) => {},
                Err(error) => return Err(module_error(location,
                    format!("Cannot resolve '{}': {}", file.display(), error))),
            }
        }
        Ok(found.map(|(path, _)| Self::File
            {
                path, library: canonicalize(library).unwrap_or_else(|_| library.to_path_buf()),
            }))
    }

    fn bundled(name: &str, directory: &Path, location: &Location)
    -> InterpreterResult<Option<Self>>
    {
        let path = directory.join(name.replace("::", "/"));
        let prefix = path.to_string_lossy().replace('\\', "/");
        if bundled_source(&prefix).is_some() && bundled_has_children(&prefix)
        {
            return Err(module_error(
                location,
                format!(
                    "Ambiguous module: bundled '{}.shy' conflicts with directory '{}'",
                    prefix, prefix
                ),
            ));
        }
        let mut found: Option<Self> = None;
        for path in [path.clone(), path.join("init")]
        {
            let name = path.components().map(|component| component.as_os_str().to_str())
                .collect::<Option<Vec<_>>>().ok_or_else(|| module_error(location,
                    "Invalid bundled module path"))?.join("/");
            if let Some(source) = bundled_source(&name)
            {
                if let Some(Self::Bundled { name: previous, .. }) = &found
                {
                    return Err(module_error(location, format!(
                        "Ambiguous module: both bundled '{}.shy' and '{}.shy' exist",
                        previous, name)));
                }
                found = Some(Self::Bundled { name, source });
            }
        }
        Ok(found)
    }


}


#[track_caller]
fn module_error(location: &Location, message: impl Into<String>) -> InterpreterError
{
    InterpreterError::new(location.clone(), ErrorWhat::ModuleError(message.into()))
}


impl Interpreter
{
    // Return the original module and member, never a copied declaration.
    fn module_member<'a>(&'a self, name: &'a str) -> Option<(&'a Scope, &'a str)>
    {
        self.module_member_in(self.scope(), name)
    }

    fn module_member_in<'a>(&'a self, mut scope: &'a Scope, name: &'a str)
        -> Option<(&'a Scope, &'a str)>
    {
        let mut member = name;
        let mut local = true;
        let mut parent_types = false;
        while member.contains("::")
        {
            // Imports may bind a compound namespace such as std::net. Prefer the
            // longest bound prefix, then continue through the target module's imports.
            let (namespace, key) = scope.modules.iter()
                .filter(|(namespace, _)| member.strip_prefix(namespace.as_str())
                    .is_some_and(|tail| tail.starts_with("::"))
                    && (local || scope.exports.contains(namespace.as_str())))
                .max_by_key(|(namespace, _)| namespace.len())?;
            parent_types = local && scope.parent_namespaces.contains(namespace);
            member = &member[namespace.len() + 2..];
            scope = self.scopes.get(key)?;
            local = false;
        }
        (scope.exports.contains(member) || (parent_types && scope.types.names.contains_key(member)))
            .then_some((scope, member))
    }

    pub(super) fn module_alias_in(&self, home: &str, name: &str) -> Option<Rc<Alias>>
    {
        let scope = self.scopes.get(home)?;
        if !name.contains("::") { return scope.aliases.get(name).cloned(); }
        let (scope, name) = self.module_member_in(scope, name)?;
        if !scope.exports.contains(name) { return None; }
        scope.aliases.get(name).cloned()
    }

    pub(super) fn module_function(&self, name: &str) -> Option<FunctionRef>
    {
        if !name.contains("::") { return self.scope().function(name); }
        let (scope, name) = self.module_member(name)?;
        if !scope.exports.contains(name) { return None; }
        scope.base_function(name)
    }

    pub(super) fn module_native_function(&self, name: &str) -> Option<Rc<NativeFunction>>
    {
        if let Some(function) = self.scope().native_functions.get(name)
        { return Some(function.clone()); }
        if !name.contains("::") { return self.scope().native_functions.get(name).cloned(); }
        let (scope, name) = self.module_member(name)?;
        if !scope.exports.contains(name) { return None; }
        scope.native_functions.get(name).cloned()
    }

    pub(super) fn variable_binding(&self, name: &str) -> Option<Rc<RefCell<ScopedValue>>>
    {
        if let Some(binding) = self.scope().variables.binding(name) { return Some(binding); }
        if !name.contains("::") { return None; }
        let (namespace, member) = name.strip_prefix('$')?.rsplit_once("::")?;
        let qualified = format!("{}::${}", namespace, member);
        let (scope, name) = self.module_member(&qualified)?;
        scope.variables.binding(name)
    }

    pub(super) fn variable_reference(&self, name: &str) -> Option<ValueReference>
    {
        if let Some(reference) = self.scope().variables.reference(name) { return Some(reference); }
        let root = self.variable_binding(name)?;
        if let Some(reference) = &root.borrow().reference { return Some(reference.clone()); }
        Some(ValueReference
            {
                root, indexes: Vec::new(), fields: Vec::new(), constraints: Vec::new(),
                dispatch_type: None, property: None,
            })
    }

    pub(super) fn module_method(&self, id: TypeId, key: &str) -> Option<FunctionRef>
    {
        let home = self.type_scopes.get(&id)?;
        let scope = self.scopes.get(home)?;
        let child = key.ends_with(":new")
            && self.module_sources.get(home).zip(self.module_sources.get(&self.current_scope))
                .is_some_and(|(parent, child)| parent.contains_child(child));
        if *home != self.current_scope && !scope.exports.contains(key) && !child { return None; }
        scope.base_function(key)
    }

    fn evaluate_condition(&mut self, condition: AstExpression) -> InterpreterResult<bool>
    {
        let location = condition.location.clone();
        let expression = AstExpression
            {
                interpolation_members: Default::default(),
                dispatch_type: Default::default(),
                location: location.clone(), string_flag: None,
                kind: AstExpressionKind::BooleanNot(Box::new(AstExpression
                    {
                        interpolation_members: Default::default(),
                        dispatch_type: Default::default(),
                        location, string_flag: None,
                        kind: AstExpressionKind::BooleanNot(Box::new(condition)),
                    })),
            };
        let previous = self.last_result.take();
        let result = (||
            {
                self.refresh_module_names()?;
                let mut condition = vec![AstStatement::ExpressionStatement(expression)];
                let code = self.scope_mut().compile_condition(&mut condition)?;
                self.execute_instructions(&code)?;
                Ok(self.last_result.as_ref().is_some_and(Value::as_bool))
            })();
        self.last_result = previous;
        result
    }

    fn import_conditions(&mut self, statements: &mut AstTopLevel)
        -> InterpreterResult<Vec<AstImportStatement>>
    {
        exclude_statements(statements, &mut |condition| self.evaluate_condition(condition))?;
        let mut imports = Vec::new();
        let mut body = Vec::new();
        for statement in take(statements)
        {
            if let AstStatement::ImportStatement(import) = statement { imports.push(*import); }
            else { body.push(statement); }
        }
        *statements = body;
        // None of the containing file's imports or declarations have been published yet.
        let mut enabled = Vec::new();
        for mut import in imports
        {
            let accepted = if let Some(condition) = import.condition.take()
                {
                    self.evaluate_condition(condition)?
                }
                else { true };
            import.enabled = accepted;
            enabled.push(import);
        }
        Ok(enabled)
    }

    fn source_directory(&self, origin: &str) -> ModuleDirectory
    {
        let path = if origin.starts_with('<')
            { current_dir().unwrap_or_else(|_| ".".into()) }
            else
            {
                let expanded = self.eval_path_from(origin);
                canonicalize(&expanded).unwrap_or_else(|_| PathBuf::from(expanded))
                    .parent().unwrap_or_else(|| Path::new(".")).to_path_buf()
            };
        ModuleDirectory::File(path)
    }

    fn module_directories(&self, standard: bool, directory: &ModuleDirectory, location: &Location)
        -> InterpreterResult<Vec<PathBuf>>
    {
        let variable = if standard { "$SHELLY_STD_PATH" } else { "$SHELLY_MODULE_PATH" };
        let mut directories = Vec::new();
        if !standard && let ModuleDirectory::File(directory) = directory
        { directories.push(directory.clone()); }
        let paths = if let Some(binding) = self.variable_binding(variable)
            {
                let value = binding.borrow().value.clone();
                let Value::String(paths, _) = value else
                { return Err(module_error(location, format!("{} must be a String", variable))); };
                paths
            }
            else if standard
            {
                #[cfg(target_os = "systemrisc")]
                { "bundled".into() }
                #[cfg(not(target_os = "systemrisc"))]
                { "/etc/shelly/std:/usr/local/share/shelly/std:/usr/share/shelly/std:bundled".into() }
            }
            else { String::new() };
        directories.extend(split_paths(&paths).filter(|path| !path.as_os_str().is_empty())
            .map(|path| PathBuf::from(self.eval_path_from(&path.to_string_lossy()))));
        Ok(directories)
    }

    fn find_module_source(&self, import: &AstImportStatement, directory: &ModuleDirectory)
        -> InterpreterResult<Option<ModuleSource>>
    {
        let standard = import.module.strip_prefix("std::");
        let name = standard.unwrap_or(&import.module);
        let directories = self.module_directories(standard.is_some(), directory, &import.location)?;
        // Embedded imports have a real virtual parent, rather than borrowing the
        // caller's cwd. Ordinary imports first search beside their embedded source.
        if    standard.is_none()
           && let ModuleDirectory::Bundled(directory) = directory
           && let Some(source) = ModuleSource::bundled(name, directory, &import.location)?
        { return Ok(Some(source)); }
        for directory in directories
        {
            if standard.is_some() && directory.as_os_str() == "bundled"
            {
                if let Some(source) = ModuleSource::bundled(name, Path::new(""), &import.location)?
                { return Ok(Some(source)); }
                continue;
            }
            let relative = name.replace("::", "/");
            let files = [
                    directory.join(format!("{}.shy", relative)),
                    directory.join(&relative).join("init.shy"),
                ];
            if let Some(source) = ModuleSource::files(&files, &directory, &import.location)?
            { return Ok(Some(source)); }
        }
        Ok(None)
    }

    fn load_import(&mut self, import: &AstImportStatement, directory: &ModuleDirectory)
        -> InterpreterResult<String>
    {
        if import.module.rsplit("::").next() == Some("init")
        {
            return Err(module_error(&import.location,
                "init.shy is a module entry point; import its containing module instead"));
        }
        if import.module.split("::").next() == Some("super")
        { return self.load_parent_import(import); }
        if import.module == "std::prelude" && !self.prelude_loading
        {
            // An existing module keeps its own prelude generation after a reload.
            if let Some(key) = self.scope().modules.get("std::prelude")
            { return Ok(key.clone()); }
            if let Some(prelude) = &self.prelude
            { return Ok(prelude.scope.clone()); }
        }
        let source = self.find_module_source(import, directory)?
            .ok_or_else(|| module_error(&import.location,
                format!("Module '{}' was not found", import.module)))?;
        self.load_module(&source, &import.location)
    }

    pub(super) fn initialize_prelude(&mut self) -> InterpreterResult<()>
    {
        if self.prelude.is_some() { return Ok(()); }
        let location = Location::new("<prelude>", 1, 1);
        let import = AstImportStatement
            {
                public: false,
                location: location.clone(), module: "std::prelude".into(), names: Vec::new(),
                condition: None, enabled: true,
            };
        let directory = ModuleDirectory::File(".".into());
        let Some(source) = self.find_module_source(&import, &directory)?
            else { return Ok(()); };
        // No implicit prelude is installed while bootstrapping the prelude itself
        // and its dependencies. Explicit circular imports still fail normally.
        self.prelude_loading = true;
        let loaded = self.load_module(&source, &location);
        self.prelude_loading = false;
        let key = loaded?;
        if self.halted { return Ok(()); }
        let source = source.library_location();
        self.scopes.get_mut(&key).unwrap().prelude_source = Some(source.clone());
        self.prelude = Some(Prelude { scope: key.clone(), source });
        let loaded: Vec<_> = self.scopes.keys().filter(|name| **name != key).cloned().collect();
        for name in loaded
        {
            let caller = replace(&mut self.current_scope, name);
            let result = self.install_prelude(&location);
            self.current_scope = caller;
            result?;
        }
        self.refresh_module_names()?;
        Ok(())
    }

    pub(super) fn handle_prelude_reload(&mut self, location: &Location, args: &[Value])
        -> InterpreterResult<()>
    {
        if !args.is_empty()
        {
            return Err(InterpreterError::new(
                location.clone(),
                ErrorWhat::ArgumentMismatch("prelude_reload expects no arguments.".into())
            ));
        }
        if self.prelude_loading
        { return Err(module_error(location, "Cannot reload the prelude while it is loading")); }
        let import = AstImportStatement
            {
                public: false,
                location: location.clone(), module: "std::prelude".into(), names: Vec::new(),
                condition: None, enabled: true,
            };
        let directory = ModuleDirectory::File(".".into());
        let source = self.find_module_source(&import, &directory)?
            .ok_or_else(|| module_error(location, "Module 'std::prelude' was not found"))?;
        self.prelude_generation += 1;
        let key = format!("<prelude:{}:{}>", self.prelude_generation, source.origin());
        self.prelude_loading = true;
        let loaded = self.load_module_as(&source, location, key);
        self.prelude_loading = false;
        let key = loaded?;
        if self.halted { return Ok(()); }
        let source = source.library_location();
        self.scopes.get_mut(&key).unwrap().prelude_source = Some(source.clone());
        let previous = self.prelude.replace(Prelude { scope: key, source });
        let checkpoint = self.scope().checkpoint();
        // Only remove bindings still belonging to this scope's previous implicit prelude.
        // A declaration that shadows one of those bindings must not be overwritten.
        for (name, binding) in take(&mut self.scope_mut().prelude_bindings)
        {
            self.scope_mut().remove_prelude_binding(&name, binding);
        }
        if let Err(error) = self.install_prelude(location)
            .and_then(|()| self.refresh_module_names())
        {
            self.scopes.insert(self.current_scope.clone(), checkpoint);
            self.prelude = previous;
            return Err(error);
        }
        self.last_result = Some(Value::None);
        Ok(())
    }

    fn install_prelude(&mut self, location: &Location) -> InterpreterResult<()>
    {
        if self.prelude_loading { return Ok(()); }
        let Some(prelude) = self.prelude.clone() else { return Ok(()); };
        if self.current_scope == prelude.scope { return Ok(()); }
        let mut names: Vec<_> = self.scopes[&prelude.scope].exports.iter()
            .filter(|name| !name.starts_with("\0method:")).cloned().collect();
        names.sort();
        for name in &names
        {
            if    let Some(id) = self.scopes[&prelude.scope].types.names.get(name)
               && self.scope().types.names.get(name).is_some_and(|existing| existing != id)
            {
                return Err(module_error(location,
                    format!("Prelude type '{}' conflicts with an existing type", name)));
            }
        }
        let new_names: Vec<_> = names.iter()
            .filter(|name| self.scope().prelude_binding(name).is_none()).cloned().collect();
        self.scope_mut().modules.insert("std::prelude".into(), prelude.scope.clone());
        let import = AstImportStatement
            {
                public: false, location: location.clone(), module: "std::prelude".into(),
                names, condition: None, enabled: true,
            };
        self.install_import_bindings(&import, &prelude.scope, true)?;
        for name in new_names
        {
            let binding = self.scope().prelude_binding(&name).unwrap();
            self.scope_mut().prelude_bindings.insert(name, binding);
        }
        self.scope_mut().prelude_source = Some(prelude.source);
        self.scope_mut().types.module_names.insert("std".into());
        Ok(())
    }

    fn load_module(&mut self, source: &ModuleSource, location: &Location)
        -> InterpreterResult<String>
    {
        // A child entered as a script has a scope key distinct from its filename.
        // Its deferred parent may import that already compiled child without loading
        // a second copy or confusing the remaining execution with a circular import.
        if self.module_sources.get(&self.current_scope)
            .is_some_and(|parent| parent.contains_child(source))
        {
            let origin = source.origin();
            if let Some((key, _)) = self.module_sources.iter().find(|(key, module)|
                module.origin() == origin && !self.pending_modules.contains_key(*key))
            { return Ok(key.clone()); }
        }
        self.load_module_as(source, location, source.origin())
    }

    fn load_module_as(&mut self, source: &ModuleSource, location: &Location, key: String)
        -> InterpreterResult<String>
    {
        // Diagnostics and circular-import detection use the source path, while
        // functions retain the distinct scope key of their loaded generation.
        let origin = source.origin();
        if self.loading_modules.contains(&origin)
        { return Err(module_error(location, format!("Circular import of '{}'", origin))); }
        if self.scopes.contains_key(&key) { return Ok(key); }
        let existing: HashSet<_> = self.scopes.keys().cloned().collect();
        self.loading_modules.insert(origin.clone());
        let result = (||
            {
                let (mut statements, imports) = self.open_module_scope(source, location, &key)?;
                let caller = replace(&mut self.current_scope, key.clone());
                let previous = self.last_result.take();
                let directory = source.directory();
                let module_location = Location::new(origin.as_str(), 1, 1);
                let loaded = self.install_prelude(&module_location)
                    .and_then(|()| self.compile_module(&mut statements, imports, &directory))
                    .and_then(|code| self.execute_instructions(&code));
                self.last_result = previous;
                self.current_scope = caller;
                if loaded.is_err()
                { self.discard_module_scope(&key); }
                loaded
            })();
        self.loading_modules.remove(&origin);
        if let Err(error) = result
        {
            self.discard_parent_scopes(&existing);
            return Err(error);
        }
        if let Err(error) = self.finish_parent_modules()
        {
            self.discard_parent_scopes(&existing);
            return Err(error);
        }
        Ok(key)
    }

    fn open_module_scope(&mut self, source: &ModuleSource, location: &Location, key: &str)
        -> InterpreterResult<(AstTopLevel, Vec<AstImportStatement>)>
    {
        let origin = source.origin();
        let mut statements = match source
        {
            // Embedded source is already a valid UTF-8 string. Use the same
            // string buffer as other in-memory submissions, without boxing a
            // byte reader and decoding each character a second time.
            ModuleSource::Bundled { source, .. } =>
                self.parse_source(&mut SimpleBuffer::new(&origin, source, None))?,
            ModuleSource::File { path, .. } =>
            {
                let file = File::open(path).map_err(|error| module_error(location,
                    format!("Cannot open '{}': {}", path.display(), error)))?;
                let mut reader = BufReader::new(file);
                self.parse_source(&mut ReadBuffer::new(&origin, &mut reader, None))?
            }
        };
        // Conditions see the caller before the new module's scope exists.
        let imports = self.import_conditions(&mut statements)?;
        let mut scope = Scope::new(self.native_functions.values().cloned(), key.to_string(),
            self.scope().types.module_registry(), self.color_mode);
        for (name, mut binding) in self.scope().variables.get_all_flattened()
        {
            if binding.exported == ValueVisibility::Exported || matches!(name.as_str(),
                "$OS" | "$os" | "$shelly" | "$version" | "$build_date" | "$build_time"
                | "$interactive" | "$login" | "$SHELLY_MODULE_PATH" | "$SHELLY_STD_PATH")
            {
                binding.value = self.read_raw_variable(&name, location)?;
                binding.reference = None;
                let _ = scope.variables.create(name, binding);
            }
        }
        let _ = scope.variables.create("$args".into(), ScopedValue
            {
                value: Value::from_array(Vec::new()), type_id: None,
                exported: ValueVisibility::Private, reference: None,
                dispatch_type: None,
                environment_conversion: None,
            });
        self.scopes.insert(key.to_string(), scope);
        self.module_sources.insert(key.to_string(), source.clone());
        Ok((statements, imports))
    }

    fn install_import(&mut self, import: &AstImportStatement, key: &str) -> InterpreterResult<()>
    {
        self.install_import_bindings(import, key, false)
    }

    fn install_import_bindings(&mut self, import: &AstImportStatement, key: &str,
                               global_variables: bool) -> InterpreterResult<()>
    {
        if self.scope().modules.get(&import.module).is_some_and(|existing| existing != key)
        {
            return Err(module_error(&import.location,
                format!("Module '{}' is already bound", import.module)));
        }
        for name in &import.names
        {
            let module = &self.scopes[key];
            let parent_type = import.module.split("::").all(|name| name == "super")
                && module.types.names.contains_key(name);
            if !module.exports.contains(name) && !parent_type
            {
                return Err(module_error(&import.location,
                    format!("Module '{}' has no member '{}'", import.module, name)));
            }
            if name.starts_with('$')
            {
                let binding = module.variables.binding(name).ok_or_else(|| module_error(
                    &import.location, format!("Module variable '{}' is unavailable", name)))?;
                let existing = if global_variables { self.scope().variables.global_binding(name) }
                    else { self.scope().variables.binding(name) };
                if    let Some(existing) = existing
                   && !Rc::ptr_eq(&existing, &binding)
                {
                    return Err(module_error(&import.location,
                        format!("Variable '{}' is already bound", name)));
                }
                if global_variables
                { self.scope_mut().variables.import_global(name.clone(), binding); }
                else { self.scope_mut().variables.import(name.clone(), binding); }
            }
            else if let Some(id) = module.types.names.get(name)
            {
                let definition = module.types.get(*id);
                if self.scope().types.names.get(name)
                    .is_some_and(|existing| *existing != definition.id)
                {
                    return Err(module_error(&import.location,
                        format!("Type '{}' is already bound", name)));
                }
                self.scope_mut().types.names.insert(name.clone(), definition.id);
                self.scope_mut().imported_types.insert(name.clone(), definition);
            }
            else if let Some(function) = module.native_functions.get(name).cloned()
            {
                self.publish_native(&import.location, function, false)?;
            }
            else if let Some(function) = module.base_function(name)
            {
                if self.scope().native_functions.contains_key(name)
                {
                    return Err(module_error(&import.location,
                        format!("Function '{}' is already bound", name)));
                }
                if    let Some(existing) = self.scope().base_function(name)
                   && !Rc::ptr_eq(&existing, &function)
                {
                    return Err(module_error(&import.location,
                        format!("Function '{}' is already bound", name)));
                }
                self.scope_mut().import_function(name.clone(), function);
            }
            else if let Some(alias) = module.aliases.get(name).cloned()
            {
                if self.scope().aliases.get(name).is_some_and(|item| !Rc::ptr_eq(item, &alias))
                { return Err(module_error(&import.location,
                    format!("Alias '{}' is already bound", name))); }
                self.scope_mut().aliases.insert(name.clone(), alias);
            }
            else if let Some(key) = module.modules.get(name).cloned()
            {
                if self.scope().modules.get(name).is_some_and(|item| *item != key)
                { return Err(module_error(&import.location,
                    format!("Module '{}' is already bound", name))); }
                self.scope_mut().modules.insert(name.clone(), key);
                self.scope_mut().types.module_names
                    .insert(name.split("::").next().unwrap().to_string());
            }
            else
            {
                return Err(module_error(&import.location,
                    format!("Member '{}' cannot be imported", name)));
            }
            if import.public { self.scope_mut().exports.insert(name.clone()); }
        }
        if import.public
        {
            // Extensions are part of a module's public interface even when only
            // selected names are re-exported. Keep their original function handles.
            let methods: Vec<_> = self.scopes[key].exports.iter()
                .filter(|name| name.starts_with("\0method:")).cloned().collect();
            self.scope_mut().exports.extend(methods);
        }
        self.scope_mut().modules.insert(import.module.clone(), key.to_string());
        if import.module.split("::").all(|name| name == "super")
        { self.scope_mut().parent_namespaces.insert(import.module.clone()); }
        if import.public && import.names.is_empty()
        { self.scope_mut().exports.insert(import.module.clone()); }
        Ok(())
    }

    fn refresh_module_names(&mut self) -> InterpreterResult<()>
    {
        fn collect(
            scopes: &HashMap<String, Scope>,
            scope: &Scope,
            prefix: &str,
            bindings: &mut ModuleBindings,
            visiting: &mut HashSet<String>,
            local: bool,
        ) -> InterpreterResult<()>
        {
            for (name, key) in &scope.modules
            {
                if !local && !scope.exports.contains(name) { continue; }
                if !visiting.insert(key.clone()) { continue; }
                let module = &scopes[key];
                let prefix = format!("{}{}::", prefix, name);
                let mut members = module.exports.clone();
                if local && scope.parent_namespaces.contains(name)
                {
                    members.extend(module.types.names.keys().filter(|name|
                        !name.contains("::")).cloned());
                    members.extend(module.types.constructors.keys()
                        .map(|id| method_key(*id, "new")));
                }
                for name in &members
                {
                    if name.starts_with("\0method:")
                    {
                        if let Some(function) = module.base_function(name)
                        {
                            if    let Some(previous) = bindings.methods.get(name)
                               && !Rc::ptr_eq(previous, &function)
                            {
                                let member = name.rsplit(':').next().unwrap();
                                if is_conversion_method(member)
                                    && let (Some(source), Some(previous_source)) =
                                        (function.argument_types.first().copied().flatten(),
                                         previous.argument_types.first().copied().flatten())
                                    && source != previous_source
                                {
                                    let receiver = function.receiver_type.unwrap();
                                    let previous = previous.clone();
                                    bindings.methods.insert(conversion_key(receiver, member,
                                        previous_source), previous);
                                    bindings.methods.insert(conversion_key(receiver, member,
                                        source), function);
                                    continue;
                                }
                                // Scripted factories override typedef defaults regardless
                                // of import order. Distinct scripted methods still conflict.
                                if function.implied_conversion { continue; }
                                if !previous.implied_conversion
                                {
                                    let receiver = module.types
                                        .get(function.receiver_type.unwrap());
                                    let member = name.rsplit(':').next().unwrap();
                                    let location = function.code.iter()
                                        .find_map(|instruction| instruction.location.as_ref())
                                        .cloned().unwrap_or_default();
                                    return Err(module_error(&location, format!(
                                        "Public method '{}::{}' is already bound",
                                        receiver.name, member)));
                                }
                            }
                            bindings.methods.insert(name.clone(), function);
                        }
                        else if module.declared_function(name)
                        { bindings.forward_methods.insert(name.clone()); }
                        continue;
                    }
                    if let Some(id) = module.types.names.get(name)
                    { bindings.types.insert(format!("{}{}", prefix, name), *id); }
                    if module.base_function(name).is_some() || module.declared_function(name)
                        || module.native_functions.contains_key(name)
                        || module.aliases.contains_key(name)
                    { bindings.functions.insert(format!("{}{}", prefix, name)); }
                }
                collect(scopes, module, &prefix, bindings, visiting, false)?;
                visiting.remove(key);
            }
            Ok(())
        }
        let mut bindings = ModuleBindings::default();
        let mut visiting = HashSet::from([self.current_scope.clone()]);
        collect(&self.scopes, self.scope(), "", &mut bindings, &mut visiting, true)?;
        // A re-exported type retains its original public conversion interface,
        // even when the defining module's namespace is private in the intermediary.
        // Install factories before compilation so calls capture their versions.
        for id in bindings.types.values()
        {
            let Some(source) = self.type_scopes.get(id).and_then(|home| self.scopes.get(home))
                else { continue; };
            for key in &source.exports
            {
                if    key.starts_with("\0method:")
                   && (is_conversion_method(key.rsplit(':').next().unwrap())
                       || key.ends_with(":new"))
                   && let Some(function) = source.base_function(key)
                   && function.receiver_type == Some(*id)
                {
                    bindings.methods.entry(key.clone()).or_insert(function);
                }
            }
        }
        let ModuleBindings { types: names, functions, methods, forward_methods } = bindings;
        let extensions: Vec<_> = names.values().filter_map(|id|
            {
                let source = self.scopes.get(self.type_scopes.get(id)?)?;
                let mut exports = source.exports.clone();
                if self.type_scopes.get(id).and_then(|home| self.module_sources.get(home))
                    .zip(self.module_sources.get(&self.current_scope))
                    .is_some_and(|(parent, child)| parent.contains_child(child))
                { exports.insert(method_key(*id, "new")); }
                Some((*id, source.types.clone(), exports))
            }).collect();
        let native_factories: Vec<_> = self.scope().native_functions.keys()
            .filter(|name| name.contains("::")).cloned().collect();
        let signatures: Vec<_> = functions
            .iter()
            .filter_map(|name| {
                self.module_function(name)
                    .map(|function| (name.clone(), function.argument_types.clone()))
            })
            .collect();
        let types = &mut self.scope_mut().types;
        types.argument_signatures.extend(signatures);
        types.argument_signatures.extend(
            methods
                .iter()
                .map(|(name, function)| (name.clone(), function.argument_types.clone())),
        );
        for (id, source, exports) in extensions
        {
            types.import_extensions(&source, id,
                |id, name| exports.contains(&method_key(id, name)));
        }
        types.names.retain(|name, _| !name.contains("::"));
        types.names.extend(names);
        types.qualified_functions = functions;
        types.qualified_functions.extend(native_factories);
        for (key, function) in &methods
        {
            if let Some(receiver) = function.receiver_type
            {
                let name = key.rsplit(':').next().unwrap();
                if function.implied_conversion
                { types.declare_implied_conversion(receiver, function.argument_types[0].unwrap()); }
                else { types.declare_extension(receiver, name); }
            }
        }
        self.scope_mut().refresh_methods(methods);
        for key in forward_methods { self.scope_mut().announce_function(key); }
        Ok(())
    }

    fn compile_module(&mut self, statements: &mut AstTopLevel, imports: Vec<AstImportStatement>,
                      directory: &ModuleDirectory) -> InterpreterResult<Vec<Instruction>>
    {
        let checkpoint = self.scope().checkpoint();
        let result = (||
            {
                self.stage_parent_types(statements, &imports);
                for import in imports
                {
                    self.scope_mut().types.module_names
                        .insert(import.module.split("::").next().unwrap().to_string());
                    if !import.enabled { continue; }
                    let key = self.load_import(&import, directory)?;
                    self.install_import(&import, &key)?;
                }
                self.refresh_module_names()?;
                let mut exposed_types = false;
                if let Some(pending) = self.pending_modules.get_mut(&self.current_scope)
                {
                    exposed_types = pending.types.is_none();
                    *statements = pending.types.take().unwrap_or_default();
                    statements.extend(pending.body.take().unwrap_or_default());
                }
                self.prepare_visibility(statements)?;
                let first_type = self.scope().types.definition_count();
                let code = self.scope_mut().compile(statements)?;
                for index in first_type..self.scope().types.definition_count()
                { self.type_scopes.insert(TypeId(index), self.current_scope.clone()); }
                // A public typedef publishes its default factory. Explicit factories
                // below determine their own visibility, independent of declaration order.
                for statement in statements.iter()
                {
                    if    let AstStatement::TypeDeclaration(declaration) = statement
                       && let Some(factory) = &declaration.conversion
                    {
                        let key = method_key(factory.receiver_type.unwrap(), &factory.name);
                        if declaration.public { self.scope_mut().exports.insert(key); }
                        else { self.scope_mut().exports.remove(&key); }
                    }
                }
                for statement in statements
                {
                    // pub visible publishes the referenced name, not the builtin's name.
                    if matches!(statement, AstStatement::ExecuteStatement(_)) { continue; }
                    for (name, _, public) in statement.module_declarations()
                    {
                        let name = match &*statement
                            {
                                AstStatement::FunctionDefinition(item)
                                    if item.receiver_type.is_some()
                                    => method_key(item.receiver_type.unwrap(), &item.name),
                                _ => name.to_string(),
                            };
                        if public { self.scope_mut().exports.insert(name); }
                        else { self.scope_mut().exports.remove(&name); }
                    }
                }
                if exposed_types { self.refresh_parent_children(&self.current_scope.clone())?; }
                Ok(code)
            })();
        if result.is_err() { self.scopes.insert(self.current_scope.clone(), checkpoint); }
        self.pending_modules.remove(&self.current_scope);
        result
    }

    pub(super) fn execute_submission(&mut self, mut statements: AstTopLevel, origin: &str)
        -> InterpreterResult<()>
    {
        let existing: HashSet<_> = self.scopes.keys().cloned().collect();
        let directory = self.source_directory(origin);
        let key = canonicalize(self.eval_path_from(origin)).ok()
            .map(|path| path.to_string_lossy().into_owned());
        if let Some(path) = &key
        {
            self.module_sources.insert(self.current_scope.clone(), ModuleSource::File
                {
                    path: PathBuf::from(path),
                    library: Path::new(path).parent().unwrap().to_path_buf(),
                });
        }
        else { self.module_sources.remove(&self.current_scope); }
        if let Some(key) = &key { self.loading_modules.insert(key.clone()); }
        let result = (||
            {
                let imports = self.import_conditions(&mut statements)?;
                let code = self.compile_module(&mut statements, imports, &directory)?;
                self.finish_parent_modules()?;
                self.execute_instructions(&code)
            })();
        if let Some(key) = key { self.loading_modules.remove(&key); }
        if result.is_err() { self.discard_parent_scopes(&existing); }
        result
    }
}
