
use std::{ cell::RefCell, collections::{ HashMap, HashSet }, rc::Rc };

use crate::runtime::{ color::TtyColorMode, highlighting::SYNTAX_COLORS };
use crate::language::{ ast::AstTopLevel,
                       native::{ NativeFunction, NativeVisibility },
                       bytecode::{ FunctionBlock, FunctionBlockRef, FunctionRef, Instruction },
                       compiler::{ CompileResult, CompileTarget, compile_ast },
                       data::{ scoped_variables::{ ScopedValue, ScopedVariables, ValueVisibility },
                               types::{ EnumValue, TypeDefinition, TypeId, TypeKind, TypeRegistry },
                               value::Value } };


#[derive(Clone)]
pub struct Alias
{
    pub scope: String,
    pub name: String,
    pub arguments: Vec<String>
}


#[derive(Clone)]
pub(super) enum PreludeBinding
{
    Type(TypeId),
    Variable(Rc<RefCell<ScopedValue>>),
    Function(FunctionRef),
    Native(Rc<NativeFunction>),
    Alias(Rc<Alias>),
    Module(String),
}


// Persistent script state. Variables follow the caller's runtime scopes; types and
// functions keep the lexical identities and snapshots established during compilation.
pub(super) struct Scope
{
    pub variables: ScopedVariables,
    pub types: TypeRegistry,
    pub aliases: HashMap<String, Rc<Alias>>,
    pub modules: HashMap<String, String>,
    // Only a direct structural-parent import grants access to private type names.
    pub parent_namespaces: HashSet<String>,
    pub native_functions: HashMap<String, Rc<NativeFunction>>,
    pub exports: HashSet<String>,
    pub prelude_bindings: HashMap<String, PreludeBinding>,
    pub prelude_source: Option<String>,
    pub imported_types: HashMap<String, Rc<TypeDefinition>>,
    pub imported_methods: HashMap<String, FunctionRef>,
    base_function_block: FunctionBlockRef,
    current_function_block: Option<FunctionBlockRef>,
}


impl Scope
{
    pub fn new(natives: impl IntoIterator<Item = Rc<NativeFunction>>,
               scope: String, mut types: TypeRegistry, color_mode: TtyColorMode) -> Self
    {
        types.allow_candidates = false;
        let native_functions: HashMap<_, _> = natives.into_iter()
            .filter(|item| item.visibility != NativeVisibility::Hidden)
            .map(|item| (item.name.to_string(), item)).collect();
        types.qualified_functions.extend(native_functions.keys()
            .filter(|name| name.contains("::")).cloned());
        let builtins = native_functions.values().map(|item| item.name).collect();
        let mut variables = ScopedVariables::new_from_environment();
        if variables.get("$SHELLY_TAB_SIZE").is_none()
        {
            let _ = variables.create("$SHELLY_TAB_SIZE".into(), ScopedValue
                {
                    value: Value::Integer(4), type_id: Some(types.native_types["Integer"].id),
                    exported: ValueVisibility::Private, reference: None,
                    dispatch_type: None, environment_conversion: None,
                });
        }
        // Replace inherited text with the mode detected for this interpreter's terminal.
        let color_type = types.native_types["TtyColorMode"].id;
        let _ = variables.create("$tty_color_mode".to_string(), ScopedValue
            {
                value: Value::Enum(Rc::new(EnumValue
                    {
                        definition: types.get(color_type),
                        variant: color_mode.variant_index(),
                    })),
                type_id: Some(color_type),
                exported: ValueVisibility::Exported,
                reference: None,
                dispatch_type: None,
                environment_conversion: None,
            });
        let history_path = types.intern(TypeKind::Optional(types.native_types["String"].id));
        #[cfg(target_os = "systemrisc")]
        let default_history = Value::None;
        #[cfg(not(target_os = "systemrisc"))]
        let default_history = Value::from_string("~/.shelly_history.jsonl".into());
        for (name, value, type_id) in [
                ("$history_path", default_history, history_path),
                ("$history_limit", Value::Integer(10000), types.native_types["Integer"].id),
            ]
        {
            let _ = variables.create(name.into(), ScopedValue
                {
                    value,
                    type_id: Some(type_id),
                    exported: ValueVisibility::Private,
                    reference: None,
                    dispatch_type: None,
                    environment_conversion: None,
                });
        }
        let _ = variables.create("$repl_syntax".into(), ScopedValue
            {
                value: Value::Boolean(true),
                type_id: Some(types.native_types["Boolean"].id),
                exported: ValueVisibility::Private,
                reference: None,
                dispatch_type: None,
                environment_conversion: None,
            });
        for &(_, name, color) in SYNTAX_COLORS
        {
            let _ = variables.create(name.into(), ScopedValue
                {
                    value: Value::from_string(color.into()),
                    type_id: Some(types.native_types["String"].id),
                    exported: ValueVisibility::Private,
                    reference: None,
                    dispatch_type: None,
                    environment_conversion: None,
                });
        }
        let widget_array = types.intern(TypeKind::Array(types.native_types["WidgetFn"].id));
        let _ = variables.create("$widgets".to_string(), ScopedValue
            {
                value: Value::from_array(Vec::new()),
                type_id: Some(widget_array),
                exported: ValueVisibility::Private,
                reference: None,
                dispatch_type: None,
                environment_conversion: None,
            });
        let widget_color = match color_mode
            {
                TtyColorMode::Tty256 => "\x1b[38;5;80m",
                TtyColorMode::TtyBasic => "\x1b[36m",
                _ => "\x1b[38;2;49;216;224m",
            };
        let _ = variables.create("$widget_color".to_string(), ScopedValue
            {
                value: Value::from_string(widget_color.to_string()),
                type_id: Some(types.native_types["String"].id),
                exported: ValueVisibility::Private,
                reference: None,
                dispatch_type: None,
                environment_conversion: None,
            });
        let prompt_type = types.native_types["Prompt"].id;
        let _ = variables.create("$prompt".to_string(), ScopedValue
            {
                value: types.default_value(prompt_type).expect("Prompt has a native default"),
                type_id: Some(prompt_type),
                exported: ValueVisibility::Private,
                reference: None,
                dispatch_type: None,
                environment_conversion: None,
            });
        Self
            {
                variables,
                types,
                aliases: HashMap::new(),
                modules: HashMap::new(),
                parent_namespaces: HashSet::new(),
                native_functions,
                exports: HashSet::new(),
                imported_types: HashMap::new(),
                imported_methods: HashMap::new(),
                prelude_bindings: HashMap::new(),
                prelude_source: None,
                base_function_block: Rc::new(RefCell::new(FunctionBlock
                    {
                        scope,
                        parent: None,
                        function_name: None,
                        protocol_owner: None,
                        declared_functions: HashSet::new(),
                        builtins: Rc::new(builtins),
                        functions: HashMap::new(),
                    })),
                current_function_block: None,
            }
    }

    pub fn checkpoint(&self) -> Self
    {
        Self
            {
                variables: self.variables.clone(),
                types: self.types.clone(),
                aliases: self.aliases.clone(),
                modules: self.modules.clone(),
                parent_namespaces: self.parent_namespaces.clone(),
                native_functions: self.native_functions.clone(),
                exports: self.exports.clone(),
                imported_types: self.imported_types.clone(),
                imported_methods: self.imported_methods.clone(),
                prelude_bindings: self.prelude_bindings.clone(),
                prelude_source: self.prelude_source.clone(),
                base_function_block: Rc::new(RefCell::new(
                    self.base_function_block.borrow().clone())),
                current_function_block: self.current_function_block.clone(),
            }
    }

    pub fn import_native(&mut self, function: Rc<NativeFunction>)
    {
        let mut block = self.base_function_block.borrow_mut();
        Rc::make_mut(&mut block.builtins).insert(function.name);
        self.native_functions.insert(function.name.to_string(), function);
    }

    pub fn import_function(&mut self, name: String, function: FunctionRef)
    {
        self.types
            .argument_signatures
            .insert(name.clone(), function.argument_types.clone());
        self.base_function_block.borrow_mut().functions.insert(name, function);
    }

    pub fn prelude_binding(&self, name: &str) -> Option<PreludeBinding>
    {
        if name.starts_with('$')
        { return self.variables.global_binding(name).map(PreludeBinding::Variable); }
        if let Some(id) = self.types.names.get(name)
        { return Some(PreludeBinding::Type(*id)); }
        if let Some(function) = self.native_functions.get(name)
        { return Some(PreludeBinding::Native(function.clone())); }
        if let Some(function) = self.base_function(name)
        { return Some(PreludeBinding::Function(function)); }
        if let Some(alias) = self.aliases.get(name)
        { return Some(PreludeBinding::Alias(alias.clone())); }
        self.modules.get(name).cloned().map(PreludeBinding::Module)
    }

    pub fn remove_prelude_binding(&mut self, name: &str, binding: PreludeBinding)
    {
        let removed = match binding
            {
                PreludeBinding::Type(id) if self.types.names.get(name) == Some(&id) =>
                    {
                        self.types.names.remove(name);
                        self.imported_types.remove(name);
                        true
                    },
                PreludeBinding::Variable(binding) =>
                    self.variables.remove_global_binding(name, &binding),
                PreludeBinding::Function(function) =>
                    {
                        let mut block = self.base_function_block.borrow_mut();
                        if block.functions.get(name).is_some_and(|item| Rc::ptr_eq(item, &function))
                        { block.functions.remove(name); true } else { false }
                    },
                PreludeBinding::Native(function) =>
                    {
                        if    self.native_functions.get(name)
                            .is_some_and(|item| Rc::ptr_eq(item, &function))
                        {
                            self.native_functions.remove(name);
                            Rc::make_mut(&mut self.base_function_block.borrow_mut().builtins)
                                .remove(function.name);
                            true
                        }
                        else { false }
                    },
                PreludeBinding::Alias(alias) =>
                    {
                        if self.aliases.get(name).is_some_and(|item| Rc::ptr_eq(item, &alias))
                        { self.aliases.remove(name); true } else { false }
                    },
                PreludeBinding::Module(key) if self.modules.get(name) == Some(&key) =>
                    {
                        self.modules.remove(name);
                        self.parent_namespaces.remove(name);
                        true
                    },
                _ => false,
            };
        if removed { self.exports.remove(name); }
    }

    pub fn refresh_methods(&mut self, methods: HashMap<String, FunctionRef>)
    {
        let mut block = self.base_function_block.borrow_mut();
        // Keep local overrides while replacing only bindings installed by module imports.
        for (name, previous) in &self.imported_methods
        {
            if block.functions.get(name).is_some_and(|item| Rc::ptr_eq(item, previous))
            { block.functions.remove(name); }
        }
        for (name, method) in &methods
        { block.functions.entry(name.clone()).or_insert_with(|| method.clone()); }
        self.imported_methods = methods;
    }

    pub fn compile(&mut self, statements: &mut AstTopLevel) -> CompileResult<Vec<Instruction>>
    {
        // The compiler stages both registries and publishes only a valid submission.
        compile_ast(&mut self.types, &self.base_function_block, &self.variables,
                    statements, CompileTarget::Toplevel)
    }

    pub fn compile_condition(&mut self, statements: &mut AstTopLevel)
        -> CompileResult<Vec<Instruction>>
    {
        // Preserve the final value so the loader can decide whether to import.
        compile_ast(&mut self.types, &self.base_function_block, &self.variables,
                    statements, CompileTarget::Function)
    }

    pub fn base_function(&self, name: &str) -> Option<FunctionRef>
    {
        self.base_function_block.borrow().functions.get(name).cloned()
    }

    pub fn declared_function(&self, name: &str) -> bool
    {
        self.base_function_block.borrow().declared_functions.contains(name)
    }

    pub fn announce_function(&mut self, name: String)
    {
        self.base_function_block.borrow_mut().declared_functions.insert(name);
    }

    pub fn mask_implied_factory(&mut self, name: &str)
    {
        let mut block = self.base_function_block.borrow_mut();
        if block.functions.get(name).is_some_and(|function| function.implied_conversion)
        { block.functions.remove(name); }
        block.declared_functions.insert(name.to_string());
    }

    pub fn function(&self, name: &str) -> Option<FunctionRef>
    {
        self.lexical_function(name).or_else(|| self.base_function(name))
    }

    pub fn lexical_function(&self, name: &str) -> Option<FunctionRef>
    {
        // Method lookup uses only this snapshot; ordinary command lookup may also
        // fall back to definitions published by later top-level submissions.
        let mut block = self.current_function_block.as_ref()
            .unwrap_or(&self.base_function_block).clone();
        loop
        {
            let parent =
                {
                    let block = block.borrow();
                    if let Some(function) = block.functions.get(name)
                    { return Some(function.clone()); }
                    block.parent.clone()
                };
            block = parent?;
        }
    }

    pub(super) fn conversion_snapshot(&self) -> HashMap<String, FunctionRef>
    {
        let mut functions = HashMap::new();
        let mut block = Some(
            self.current_function_block
                .clone()
                .unwrap_or_else(|| self.base_function_block.clone()),
        );
        while let Some(current) = block
        {
            let current = current.borrow();
            for (name, function) in &current.functions
            {
                if name.starts_with("\0method:")
                {
                    functions.entry(name.clone()).or_insert(function.clone());
                }
            }
            block = current.parent.clone();
        }
        for (name, function) in &self.imported_methods
        {
            functions.entry(name.clone()).or_insert(function.clone());
        }
        functions
    }

    // Enumerate names without resolving or invoking their bodies.
    pub fn function_names(&self) -> HashSet<String>
    {
        let mut names: HashSet<String> = self.base_function_block.borrow().functions.keys()
            .cloned().collect();
        let mut block = self.current_function_block.clone();
        while let Some(current) = block
        {
            let current = current.borrow();
            names.extend(current.functions.keys().cloned());
            block = current.parent.clone();
        }
        names
    }

    pub fn in_function(&self) -> bool
    {
        self.current_function_block.is_some()
    }

    pub fn enter_function(&mut self, functions: FunctionBlockRef) -> Option<FunctionBlockRef>
    {
        self.variables.push_scope();
        self.current_function_block.replace(functions)
    }

    pub fn exit_function(&mut self, caller: Option<FunctionBlockRef>)
    {
        self.current_function_block = caller;
        self.variables.pop_scope();
    }
}
