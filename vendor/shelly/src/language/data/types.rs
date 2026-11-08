
use std::{ cell::RefCell, collections::{ HashMap, HashSet },
           rc::Rc,
           cmp::Ordering,
           fmt::{ self, Display, Error, Formatter },
           hash::{ Hash, Hasher } };

use crate::runtime::color::TtyColorMode;
use crate::language::{ text::location::Location,
                       interpreter::Interpreter,
                       native::{ NativeType, NativeVisibility },
                       data::{ callable::CallableValue,
                               conversion::{ conversion_type_name, convert_builtin,
                                             is_conversion_method },
                               methods::{ BuiltinMethod, register_methods },
                               value::{ Value, ExecResult }, map_key::MapKey, range::Range } };

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TypeId(pub usize);


#[derive(Clone, Debug)]
pub struct EnumVariant
{
    pub name: String,
    pub location: Location
}


#[derive(Clone, Debug)]
pub enum TypeKind
{
    Builtin,
    Pending,
    // Resolved from a live collection receiver when a method is called.
    ReceiverMember(&'static str),
    Array(TypeId),
    FixedArray(Vec<TypeId>),
    Map(TypeId, TypeId),
    Union(Vec<TypeId>),
    Function(Vec<TypeId>, TypeId),
    Named(TypeId),
    Optional(TypeId),
    Struct(Vec<FieldDefinition>),
    Enum(Vec<EnumVariant>)
}

#[derive(Clone, Debug)]
pub struct TypeDefinition
{
    pub id: TypeId,
    pub name: String,
    pub kind: TypeKind,
    pub error: bool,
    pub location: Option<Location>,
}


#[derive(Clone, Debug)]
pub struct FieldDefinition
{
    pub public: bool,
    pub name: String,
    pub type_id: TypeId,
    pub optional: bool,
    pub location: Location
}


#[derive(Clone, Debug, PartialEq)]
pub struct StructValue
{
    pub definition: Rc<TypeDefinition>,
    pub fields: Vec<Value>,
    pub constructing: bool,
    pub failure: Option<crate::language::data::failure::FailureRef>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct NamedValue
{
    pub definition: Rc<TypeDefinition>,
    pub value: Value,
}

// Definitions compare by identity, including when embedded in canonical map keys.
impl PartialEq for TypeDefinition
{
    fn eq(&self, other: &Self) -> bool
    {
        self.id == other.id
    }
}
impl Eq for TypeDefinition {}
impl PartialOrd for TypeDefinition
{
    fn partial_cmp(&self, other: &Self) -> Option<Ordering>
    {
        Some(self.cmp(other))
    }
}
impl Ord for TypeDefinition
{
    fn cmp(&self, other: &Self) -> Ordering
    {
        self.id.cmp(&other.id)
    }
}
impl Hash for TypeDefinition
{
    fn hash<H: Hasher>(&self, state: &mut H)
    {
        self.id.hash(state);
    }
}


#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConstructorParameter
{
    pub name: String,
    pub type_id: TypeId,
    pub optional: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConstructorSignature
{
    pub parameters: Vec<ConstructorParameter>,
    pub result: TypeId,
    pub variadic: bool,
}

impl ConstructorSignature
{
    pub fn defaults(&self) -> Vec<Value>
    {
        self.parameters.iter().enumerate().map(|(index, _)|
            if self.variadic && index + 1 == self.parameters.len()
            { Value::from_array(Vec::new()) } else { Value::None }).collect()
    }

    pub fn arguments(&self, mut values: Vec<Value>) -> Result<Vec<Value>, String>
    {
        if self.variadic
        {
            let value = values.pop().unwrap();
            match value.underlying()
            {
                Value::Array(items) | Value::ArgumentExpansion(items) =>
                    values.extend(items.iter().cloned()),
                _ => return Err("Named variadic constructor input must be an array".into()),
            }
        }
        Ok(values)
    }
}

// A value keeps its definition alive even after its lexical name goes out of scope.
#[derive(Clone, Debug)]
pub struct EnumValue
{
    pub definition: Rc<TypeDefinition>,
    pub variant: usize
}


impl PartialEq for EnumValue
{
    fn eq(&self, other: &Self) -> bool
    {
        self.definition.id == other.definition.id && self.variant == other.variant
    }
}

impl Eq for EnumValue {}

impl PartialOrd for EnumValue
{
    fn partial_cmp(&self, other: &Self) -> Option<Ordering>
    {
        Some(self.cmp(other))
    }
}

impl Ord for EnumValue
{
    fn cmp(&self, other: &Self) -> Ordering
    {
        (self.definition.id, self.variant).cmp(&(other.definition.id, other.variant))
    }
}

impl Hash for EnumValue
{
    fn hash<H: Hasher>(&self, state: &mut H)
    {
        self.definition.id.hash(state);
        self.variant.hash(state);
    }
}

impl Display for EnumValue
{
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result
    {
        let TypeKind::Enum(variants) = &self.definition.kind else { return Err(Error); };
        write!(f, "{}::{}", self.definition.name, variants[self.variant].name)
    }
}


// Shared by all submissions to one interpreter. Clone to stage a compilation;
// commit only after checking, code generation, optimization, and linking succeed.
#[derive(Clone)]
pub struct TypeRegistry
{
    definitions: Rc<RefCell<Vec<Rc<TypeDefinition>>>>,
    pub native_types: HashMap<String, NativeType>,
    pub qualified_functions: HashSet<String>,
    pub argument_signatures: HashMap<String, Vec<Option<TypeId>>>,
    pub module_names: HashSet<String>,
    methods: HashMap<TypeId, HashMap<&'static str, Rc<BuiltinMethod>>>,
    extensions: HashSet<(TypeId, String)>,
    implied_conversions: HashSet<(TypeId, String)>,
    pub names: HashMap<String, TypeId>,
    pub constructors: HashMap<TypeId, ConstructorSignature>,
    pub constructor_permissions: HashSet<TypeId>,
    pub properties: HashMap<(TypeId, String), TypeId>,
    pub checking_factory: Option<TypeId>,
    pub checking_receiver: Option<TypeId>,
    pub allow_candidates: bool,
}


impl TypeRegistry
{
    pub fn underlying_type(&self, mut id: TypeId) -> TypeId
    {
        while let TypeKind::Named(inner) = self.get(id).kind { id = inner; }
        id
    }

    pub fn union_type(&self, members: Vec<TypeId>) -> TypeId
    {
        fn collect(registry: &TypeRegistry, id: TypeId, members: &mut Vec<TypeId>,
                   unit: &mut bool)
        {
            match &registry.get(id).kind
            {
                TypeKind::Union(items) =>
                    for item in items { collect(registry, *item, members, unit); },
                TypeKind::Optional(inner) =>
                    { *unit = true; collect(registry, *inner, members, unit); },
                _ if Some(id) == registry.builtin_id("None") => *unit = true,
                _ => members.push(id),
            }
        }
        let mut items = Vec::new();
        let mut unit = false;
        for id in members { collect(self, id, &mut items, &mut unit); }
        items.sort();
        items.dedup();
        let id = match items.len()
            {
                0 => return self.builtin_id("None").unwrap(),
                1 => items[0],
                _ => self.intern(TypeKind::Union(items)),
            };
        if unit { self.intern(TypeKind::Optional(id)) } else { id }
    }

    pub fn declare_extension(&mut self, receiver: TypeId, name: &str)
    {
        self.extensions.insert((receiver, name.to_string()));
    }

    // Optional projections keep their named interface when they contain a value.
    pub fn union_interface(&self, id: TypeId) -> Option<TypeId>
    {
        match self.get(id).kind
        {
            TypeKind::Optional(inner) => self.union_interface(inner),
            TypeKind::Named(_) if matches!(self.get(self.underlying_type(id)).kind,
                TypeKind::Union(_)) => Some(id),
            _ => None,
        }
    }

    pub fn method_types(&self, receiver: TypeId) -> Vec<TypeId>
    {
        let definition = self.get(receiver);
        if let TypeKind::Named(inner) = definition.kind
        {
            let mut types = vec![receiver];
            types.extend(self.method_types(inner));
            return types;
        }
        let receiver = match definition.kind
            {
                TypeKind::Array(_) | TypeKind::FixedArray(_) => self.builtin_id("Array").unwrap(),
                TypeKind::Map(_, _) => self.builtin_id("HashMap").unwrap(),
                _ => receiver,
            };
        let mut types = vec![receiver];
        if    matches!(definition.kind, TypeKind::Builtin)
           && matches!(definition.name.as_str(), "Integer" | "Float")
        { types.push(self.builtin_id("Number").unwrap()); }
        let any = self.builtin_id("any").unwrap();
        if receiver != any { types.push(any); }
        types
    }

    // Mixed and empty collections advertise any. Merge nested containers without
    // promoting or otherwise changing their contents.
    pub fn common_type(&self, types: impl IntoIterator<Item = TypeId>) -> TypeId
    {
        let any = self.builtin_id("any").unwrap();
        types.into_iter().reduce(|left, right|
            {
                if left == right { return left; }
                match (&self.get(left).kind, &self.get(right).kind)
                {
                    (TypeKind::Array(a), TypeKind::Array(b)) =>
                        self.intern(TypeKind::Array(self.common_type([*a, *b]))),
                    (TypeKind::Map(a, b), TypeKind::Map(c, d)) => self.intern(TypeKind::Map(
                        self.common_type([*a, *c]), self.common_type([*b, *d]))),
                    (TypeKind::FixedArray(a), TypeKind::FixedArray(b)) if a.len() == b.len() =>
                        self.intern(TypeKind::FixedArray(a.iter().zip(b)
                            .map(|(a, b)| self.common_type([*a, *b])).collect())),
                    _ => any,
                }
            }).unwrap_or(any)
    }

    pub fn inferred_value_type(&self, value: &Value) -> TypeId
    {
        if let Some(id) = value.collection_type() { return id; }
        match value
        {
            Value::Array(values) =>
                self.intern(TypeKind::Array(self.common_type(values.iter()
                    .map(|value| self.inferred_value_type(value))))),
            Value::HashMap(values) =>
                self.intern(TypeKind::Map(
                    self.common_type(values.keys()
                        .map(|key| self.inferred_value_type(&key.to_value()))),
                    self.common_type(values.values()
                        .map(|value| self.inferred_value_type(value))))),
            _ => self.value_type(value),
        }
    }

    pub fn value_type(&self, value: &Value) -> TypeId
    {
        match value
        {
            Value::Closure(item) =>
                {
                    let function = &item.function;
                    let any = self.builtin_id("any").unwrap();
                    if function.variadic || function.minimum_arguments != function.arguments.len()
                    { return any; }
                    self.intern(TypeKind::Function(function.argument_types.iter()
                        .map(|id| id.unwrap_or(any)).collect(),
                        function.return_type.unwrap_or(any)))
                },
            Value::Callable(item) => item.prototype.id,
            Value::Named(item) => item.definition.id,
            Value::Enum(item) => item.definition.id,
            Value::Struct(item) => item.definition.id,
            value => self.builtin_id(&value.type_name()).unwrap(),
        }
    }

    pub fn collection_member_type(&self, receiver: TypeId, name: &str) -> Option<TypeId>
    {
        let definition = self.get(self.underlying_type(receiver));
        match (&definition.kind, name)
        {
            (TypeKind::Array(item), "value_type") => Some(*item),
            (TypeKind::FixedArray(items), "value_type") =>
                Some(self.common_type(items.iter().copied())),
            (TypeKind::Map(key, _), "key_type") => Some(*key),
            (TypeKind::Map(_, item), "value_type") => Some(*item),
            (TypeKind::Optional(inner), _) => self.collection_member_type(*inner, name),
            (TypeKind::Union(items), _) =>
                {
                    let members: Option<Vec<_>> = items.iter()
                        .map(|id| self.collection_member_type(*id, name)).collect();
                    members.map(|items| self.common_type(items))
                },
            (TypeKind::Builtin, "value_type")
                if matches!(definition.name.as_str(), "Array" | "HashMap") =>
                self.builtin_id("any"),
            (TypeKind::Builtin, "key_type") if definition.name == "HashMap" =>
                self.builtin_id("any"),
            _ => None,
        }
    }

    pub fn import_extensions(&mut self, source: &Self, receiver: TypeId,
                             public: impl Fn(TypeId, &str) -> bool)
    {
        if let Some(signature) = source.constructors.get(&receiver)
        {
            self.constructors.insert(receiver, signature.clone());
            if public(receiver, "new") { self.constructor_permissions.insert(receiver); }
            else { self.constructor_permissions.remove(&receiver); }
        }
        for ((id, name), ty) in &source.properties
        {
            // Keep private property contracts so inaccessible accessors cannot
            // silently fall through to public backing storage.
            if *id == receiver { self.properties.insert((*id, name.clone()), *ty); }
        }
        self.extensions.extend(source.extensions.iter()
            .filter(|(id, name)| *id == receiver && public(*id, name)).cloned());
        self.implied_conversions.extend(source.implied_conversions.iter()
            .filter(|(id, name)| *id == receiver && public(*id, name)).cloned());
    }

    pub fn has_extension(&self, receiver: TypeId, name: &str) -> bool
    {
        self.method_types(receiver).into_iter()
            .any(|id| self.extensions.contains(&(id, name.to_string()))
                || self.implied_conversions.contains(&(id, name.to_string())))
    }

    pub fn has_conversion(&self, target: TypeId, source: TypeId) -> bool
    {
        let name = self.conversion_method_name(source);
        let target_has = self.has_conversion_method(target, &name)
            || matches!(self.get(target).kind,
                TypeKind::Array(_) | TypeKind::FixedArray(_) | TypeKind::Map(_, _))
                && self.has_conversion_method(self.method_types(target)[0], &name);
        target_has || self.method_types(source).into_iter().any(|id|
            self.extensions.contains(&(id, self.output_conversion_method_name(target))))
    }

    pub fn conversion_method_name(&self, source: TypeId) -> String
    {
        format!("from_{}", self.conversion_source_name(source))
    }

    pub fn output_conversion_method_name(&self, target: TypeId) -> String
    {
        format!("to_{}", self.conversion_source_name(target))
    }

    pub fn output_conversion_target(&self, name: &str) -> Option<TypeId>
    {
        let target = name.strip_prefix("to_")?;
        self.definitions.borrow().iter()
            .find(|definition| matches!(definition.kind,
                TypeKind::Array(_) | TypeKind::FixedArray(_) | TypeKind::Map(_, _))
                && self.conversion_source_name(definition.id) == target)
            .map(|definition| definition.id)
    }

    pub fn collection_source_type(&self, declared: TypeId, value: &Value) -> Option<TypeId>
    {
        match (&self.get(declared).kind, value)
        {
            (TypeKind::Array(_) | TypeKind::FixedArray(_), Value::Array(_))
            | (TypeKind::Map(_, _), Value::HashMap(_)) => Some(declared),
            (TypeKind::Optional(inner), _) => self.collection_source_type(*inner, value),
            (TypeKind::Union(members), _) =>
                {
                    let mut matching = members.iter().filter_map(|id|
                        self.collection_source_type(*id, value)
                            .filter(|id| self.validate(*id, value).is_ok()));
                    let source = matching.next()?;
                    matching.next().is_none().then_some(source)
                },
            _ => None,
        }
    }

    fn conversion_source_name(&self, source: TypeId) -> String
    {
        let definition = self.get(source);
        match &definition.kind
        {
            TypeKind::Array(element) =>
                format!("array_{}", self.conversion_source_name(*element)),
            TypeKind::FixedArray(elements) => format!("array_{}",
                self.conversion_source_name(self.common_type(elements.iter().copied()))),
            TypeKind::Map(key, value) => format!("hash_{}_{}",
                self.conversion_source_name(*key), self.conversion_source_name(*value)),
            _ => conversion_type_name(&definition.name),
        }
    }

    pub fn declare_implied_conversion(&mut self, target: TypeId, source: TypeId) -> String
    {
        let name = self.conversion_method_name(source);
        self.implied_conversions.insert((target, name.clone()));
        name
    }

    pub fn has_callable_conversion(&self, target: TypeId, name: &str) -> bool
    {
        self.has_conversion_method(target, name)
            || self.implied_conversions.contains(&(target, name.to_string()))
    }

    pub fn has_conversion_method(&self, target: TypeId, name: &str) -> bool
    {
        is_conversion_method(name) && self.extensions.contains(&(target, name.to_string()))
    }

    pub fn has_conversion_methods(&self, target: TypeId) -> bool
    {
        self.extensions.iter().any(|(id, name)|
            (*id == target && is_conversion_method(name))
                || *name == self.output_conversion_method_name(target))
    }

    pub fn register_method(&mut self, receiver: TypeId, method: BuiltinMethod)
    {
        self.methods.entry(receiver).or_default().insert(method.name, Rc::new(method));
    }

    pub fn method(&self, receiver: TypeId, name: &str) -> Option<Rc<BuiltinMethod>>
    {
        self.direct_method(receiver, name)
            .or_else(|| self.direct_method(self.builtin_id("any")?, name))
    }

    pub fn method_names(&self, receiver: TypeId) -> HashSet<String>
    {
        let types = self.method_types(receiver);
        let mut names = HashSet::new();
        for id in &types
        {
            if let Some(methods) = self.methods.get(id)
            { names.extend(methods.keys().map(|name| name.to_string())); }
        }
        names.extend(self.extensions.iter().filter(|(id, _)| types.contains(id))
            .map(|(_, name)| name.clone()));
        names
    }

    // Tooling needs the registered signature, without specializing iterator results.
    pub fn registered_method(&self, receiver: TypeId, name: &str) -> Option<Rc<BuiltinMethod>>
    {
        let definition = self.get(receiver);
        if let TypeKind::Named(inner) = definition.kind
        { return self.registered_method(inner, name); }
        let receiver = match definition.kind
            {
                TypeKind::Array(_) | TypeKind::FixedArray(_) => self.builtin_id("Array")?,
                TypeKind::Map(_, _) => self.builtin_id("HashMap")?,
                _ => receiver,
            };
        self.methods.get(&receiver)?.get(name).cloned()
    }

    pub fn direct_method(&self, receiver: TypeId, name: &str) -> Option<Rc<BuiltinMethod>>
    {
        let definition = self.get(receiver);
        if let TypeKind::Named(inner) = definition.kind { return self.direct_method(inner, name); }
        let receiver = match definition.kind
            {
                TypeKind::Array(_) | TypeKind::FixedArray(_) => self.builtin_id("Array")?,
                TypeKind::Map(_, _) => self.builtin_id("HashMap")?,
                _ => receiver,
            };
        let method = self.methods.get(&receiver)?.get(name)?.clone();
        if !method.native_iterator { return Some(method); }
        let item = match &definition.kind
            {
                TypeKind::Array(element) => *element,
                TypeKind::FixedArray(items) => self.common_type(items.iter().copied()),
                TypeKind::Map(key, value) => self.intern(TypeKind::FixedArray(vec![*key, *value])),
                _ => return Some(method),
            };
        Some(Rc::new(BuiltinMethod
            { return_type: self.intern(TypeKind::Optional(item)), ..(*method).clone() }))
    }

    const BUILTIN_NAMES: [&'static str; 14] = [
            "None", "ExecResult", "Integer", "Float", "Boolean", "String",
            "Array", "HashMap", "Range", "ArgumentExpansion", "Number", "any", "Terminal",
            "Type",
        ];

    pub fn is_builtin_name(name: &str) -> bool
    {
        Self::BUILTIN_NAMES.contains(&name)
    }

    pub fn builtin_id(&self, name: &str) -> Option<TypeId>
    {
        self.definitions.borrow().iter()
            .find(|item| item.name == name && matches!(item.kind, TypeKind::Builtin))
            .map(|item| item.id)
    }

    pub fn convert(&self, target: TypeId, value: &Value) -> Result<Value, String>
    {
        let definition = self.get(target);
        if matches!(definition.kind, TypeKind::Builtin) && definition.name == "any"
        { return Ok(value.clone()); }
        if let TypeKind::Named(inner) = definition.kind
        {
            if self.validate(target, value).is_ok() { return Ok(value.clone()); }
            let mut source = value;
            loop
            {
                match self.coerce(inner, source.clone())
                {
                    Ok(value) => return self.coerce(target, value),
                    Err(error) if self.may_assign(inner, self.value_type(source)) =>
                        return Err(error),
                    Err(error) => match source
                        {
                            Value::Named(item) => source = &item.value,
                            _ => return Err(error),
                        },
                }
            }
        }
        if let Value::Named(item) = value
        {
            if self.validate(target, &item.value).is_ok() { return Ok(item.value.clone()); }
            return self.convert(target, &item.value);
        }
        if !matches!(definition.kind, TypeKind::Builtin)
        {
            if self.validate(target, value).is_ok() { return Ok(value.clone()); }
            return Err(format!("Type '{}' has no conversion defined", definition.name));
        }
        convert_builtin(&definition.name, value)
    }

    pub fn new() -> Self
    {
        let mut registry = Self { definitions: Rc::new(RefCell::new(Vec::new())),
            native_types: HashMap::new(),
            qualified_functions: HashSet::new(), argument_signatures: HashMap::new(),
            module_names: HashSet::new(),
            methods: HashMap::new(),
            extensions: HashSet::new(),
            implied_conversions: HashSet::new(),
            names: HashMap::new(), constructors: HashMap::new(),
            properties: HashMap::new(), checking_factory: None, checking_receiver: None,
            allow_candidates: false,
            constructor_permissions: HashSet::new() };
        for name in Self::BUILTIN_NAMES
        {
            registry.register_native(name.to_string(), TypeKind::Builtin, NativeVisibility::Visible);
        }
        registry.register_native(
            "ErrorValue".into(),
            TypeKind::Builtin,
            NativeVisibility::Visible,
        );
        registry.register_error_types();
        registry.register_environment_types();
        registry.register_json_types();
        registry.register_io_types();
        #[cfg(not(target_os = "systemrisc"))]
        registry.register_network_types();
        let color_variants = TtyColorMode::VARIANT_NAMES.into_iter().map(|name| EnumVariant
            {
                name: name.to_string(),
                location: Location::new("<native>", 1, 1),
            }).collect();
        registry.register_native("TtyColorMode".to_string(), TypeKind::Enum(color_variants),
                                 NativeVisibility::Visible);
        for name in ["__readline_Editor", "__readline_Text", "__readline_Color"]
        {
            registry.register_native(name.into(), TypeKind::Builtin, NativeVisibility::Hidden);
        }
        registry.register_native(
            "__stream_Reader".into(),
            TypeKind::Builtin,
            NativeVisibility::Hidden,
        );
        let string = registry.builtin_id("String").unwrap();
        let widget_function = registry.intern(TypeKind::Function(Vec::new(), string));
        registry.register_native("WidgetFn".to_string(), TypeKind::Named(widget_function),
                                 NativeVisibility::Visible);
        let prompt = registry.union_type(vec![widget_function, string]);
        registry.register_native("Prompt".to_string(), TypeKind::Named(prompt),
                                 NativeVisibility::Visible);
        register_methods(&mut registry);
        registry
    }

    pub fn module_registry(&self) -> Self
    {
        let mut registry = self.clone();
        registry.names = self.native_types.iter()
            .filter(|(_, item)| item.visibility != NativeVisibility::Hidden)
            .map(|(name, item)| (name.clone(), item.id)).collect();
        registry.extensions.clear();
        registry.implied_conversions.clear();
        registry.qualified_functions.clear();
        registry.argument_signatures.clear();
        registry.module_names.clear();
        registry
    }

    pub fn staged(&self) -> Self
    {
        let mut registry = self.clone();
        registry.definitions = Rc::new(RefCell::new(self.definitions.borrow().clone()));
        registry
    }

    pub fn commit(&mut self, mut staged: Self)
    {
        // Every module shares identities, but failed compilations publish no definitions.
        *self.definitions.borrow_mut() = staged.definitions.borrow().clone();
        staged.definitions = self.definitions.clone();
        *self = staged;
    }

    // Registration preserves the internal identity independently of module lookup names.
    pub fn register_native(&mut self, name: String, kind: TypeKind,
                           visibility: NativeVisibility) -> TypeId
    {
        assert!(!self.native_types.contains_key(&name), "Duplicate native type registration");
        let location = (!matches!(kind, TypeKind::Builtin))
            .then(|| Location::new("<native>", 1, 1));
        let id = self.register(name.clone(), kind, location);
        self.native_types.insert(name.clone(), NativeType { id, visibility });
        if visibility != NativeVisibility::Hidden { self.names.insert(name, id); }
        id
    }

    pub fn register(&self, name: String, kind: TypeKind, location: Option<Location>) -> TypeId
    {
        let mut definitions = self.definitions.borrow_mut();
        let id = TypeId(definitions.len());
        definitions.push(Rc::new(TypeDefinition {
            id,
            name,
            kind,
            error: false,
            location,
        }));
        id
    }

    pub fn mark_error(&self, id: TypeId)
    {
        let mut definitions = self.definitions.borrow_mut();
        definitions[id.0] = Rc::new(TypeDefinition {
            error: true,
            ..(*definitions[id.0]).clone()
        });
    }

    pub fn valid_factory_result(&self, target: TypeId, result: TypeId) -> bool
    {
        let definition = self.get(result);
        if result == target || definition.error || Some(result) == self.builtin_id("ErrorValue")
        {
            return true;
        }
        match &definition.kind
        {
            TypeKind::Optional(inner) => self.valid_factory_result(target, *inner),
            TypeKind::Union(items) => items
                .iter()
                .all(|id| self.get(*id).name == "None" || self.valid_factory_result(target, *id)),
            TypeKind::Named(inner) => self.valid_factory_result(target, *inner),
            _ => false,
        }
    }

    pub fn valid_conversion_result(&self, target: TypeId, result: TypeId) -> bool
    {
        if result == target
            || self.get(result).error
            || Some(result) == self.builtin_id("ErrorValue")
        {
            return true;
        }
        match &self.get(result).kind
        {
            TypeKind::Array(_) | TypeKind::FixedArray(_)
                if Some(target) == self.builtin_id("Array") => true,
            TypeKind::Map(_, _) if Some(target) == self.builtin_id("HashMap") => true,
            TypeKind::Union(items) => items
                .iter()
                .all(|id| self.valid_conversion_result(target, *id)),
            TypeKind::Named(inner) => self.valid_conversion_result(target, *inner),
            _ => false,
        }
    }

    pub fn has_error_type(&self, id: TypeId) -> bool
    {
        let definition = self.get(id);
        if definition.error || Some(id) == self.builtin_id("ErrorValue")
        {
            return true;
        }
        match &definition.kind
        {
            TypeKind::Named(inner) | TypeKind::Optional(inner) => self.has_error_type(*inner),
            TypeKind::Union(items) => items.iter().any(|id| self.has_error_type(*id)),
            _ => false,
        }
    }

    pub fn catch_matches(&self, filter: TypeId, concrete: TypeId) -> bool
    {
        let definition = self.get(filter);
        if Some(filter) == self.builtin_id("ErrorValue")
        {
            return self.get(concrete).error;
        }
        match &definition.kind
        {
            TypeKind::Named(inner) | TypeKind::Optional(inner) =>
            {
                self.catch_matches(*inner, concrete)
            }
            TypeKind::Union(items) => items.iter().any(|id| self.catch_matches(*id, concrete)),
            _ => definition.error && filter == concrete,
        }
    }

    pub fn catch_type(&self, filter: TypeId) -> TypeId
    {
        match &self.get(filter).kind
        {
            TypeKind::Named(_) if self.success_type(filter).is_none() => filter,
            TypeKind::Named(inner) | TypeKind::Optional(inner) => self.catch_type(*inner),
            TypeKind::Union(items) => self.union_type(
                items
                    .iter()
                    .filter(|id| self.has_error_type(**id))
                    .map(|id| self.catch_type(*id))
                    .collect(),
            ),
            _ => filter,
        }
    }

    pub fn success_type(&self, id: TypeId) -> Option<TypeId>
    {
        let definition = self.get(id);
        if definition.error || Some(id) == self.builtin_id("ErrorValue")
        {
            return None;
        }
        match &definition.kind
        {
            TypeKind::Named(inner) if self.has_error_type(id) => self.success_type(*inner),
            TypeKind::Union(items) =>
            {
                let members: Vec<_> = items
                    .iter()
                    .filter_map(|id| self.success_type(*id))
                    .collect();
                (!members.is_empty()).then(|| self.union_type(members))
            }
            TypeKind::Optional(inner) => Some(
                self.success_type(*inner)
                    .map_or(self.builtin_id("None").unwrap(), |inner| {
                        self.intern(TypeKind::Optional(inner))
                    }),
            ),
            _ => Some(id),
        }
    }

    pub fn definition_count(&self) -> usize
    {
        self.definitions.borrow().len()
    }

    pub fn get(&self, id: TypeId) -> Rc<TypeDefinition>
    {
        self.definitions.borrow()[id.0].clone()
    }
}


impl TypeRegistry
{
    pub fn default_value(&self, id: TypeId) -> Result<Value, String>
    {
        if self.native_types.get("Prompt").is_some_and(|item| item.id == id)
        { return self.coerce(id, Interpreter::default_prompt_value()); }
        let definition = self.get(id);
        match &definition.kind
        {
            TypeKind::Named(inner) => self.coerce(id, self.default_value(*inner)?),
            TypeKind::Optional(_) => Ok(Value::None),
            TypeKind::Array(_) => Ok(Value::from_array(Vec::new())),
            TypeKind::Map(_, _) => Ok(Value::from_hash_map(HashMap::new())),
            TypeKind::Builtin => match definition.name.as_str()
                {
                    "Number" | "Integer" => Ok(Value::Integer(0)),
                    "Float" => Ok(Value::Float(0.0, None)),
                    "Boolean" => Ok(Value::Boolean(false)),
                    "String" => Ok(Value::from_string(String::new())),
                    "None" | "any" => Ok(Value::None),
                    "Array" => Ok(Value::from_array(Vec::new())),
                    "HashMap" => Ok(Value::from_hash_map(HashMap::new())),
                    "ArgumentExpansion" => Ok(Value::from_argument_expansion(Vec::new())),
                    "ExecResult" => Ok(Value::ExecResult(ExecResult::Value(0))),
                    "Range" => Ok(Value::Range(Range
                        {
                            start: Some(0),
                            end: Some(0),
                            inclusive: false,
                        })),
                    _ => Err(format!("Type '{}' requires an initializer", definition.name))
                },
            _ => Err(format!("Type '{}' requires an initializer", definition.name))
        }
    }

    // Conservative overlap test for annotated variables: a mismatch is proven
    // only if the two constraints cannot accept any common value. Empty arrays
    // and maps overlap even when their element constraints differ.
    pub fn may_overlap(&self, left: TypeId, right: TypeId) -> bool
    {
        if left == right { return true; }
        let a = self.get(left);
        let b = self.get(right);
        if    matches!(a.kind, TypeKind::ReceiverMember(_))
           || matches!(b.kind, TypeKind::ReceiverMember(_)) { return true; }
        if let TypeKind::Union(items) = &a.kind
        { return items.iter().any(|item| self.may_overlap(*item, right)); }
        if let TypeKind::Union(items) = &b.kind
        { return items.iter().any(|item| self.may_overlap(left, *item)); }
        if    matches!(a.kind, TypeKind::Builtin)
           && a.name == "any"
           || matches!(b.kind, TypeKind::Builtin)
           && b.name == "any" { return true; }
        if let TypeKind::Optional(inner) = a.kind
        { return self.validate(right, &Value::None).is_ok() || self.may_overlap(inner, right); }
        if let TypeKind::Optional(inner) = b.kind
        { return self.validate(left, &Value::None).is_ok() || self.may_overlap(left, inner); }
        let category = |definition: &TypeDefinition| match definition.kind
            {
                TypeKind::Array(_) | TypeKind::FixedArray(_) => "Array".to_string(),
                TypeKind::Map(_, _) => "HashMap".to_string(),
                TypeKind::Builtin => definition.name.clone(),
                _ => format!("nominal {}", definition.id.0)
            };
        let (a, b) = (category(&a), category(&b));
        a == b || a == "Number" && matches!(b.as_str(), "Integer" | "Float")
            || b == "Number" && matches!(a.as_str(), "Integer" | "Float")
    }

    // Assignment compatibility is directional: an Integer may widen to Float.
    pub fn may_assign(&self, expected: TypeId, actual: TypeId) -> bool
    {
        if !self.has_error_type(expected) && self.has_error_type(actual)
        {
            return self
                .success_type(actual)
                .is_none_or(|id| self.may_assign(expected, id));
        }
        if Some(expected) == self.builtin_id("ErrorValue") && self.get(actual).error
        {
            return true;
        }
        if self.native_types.get("Prompt").is_some_and(|item| item.id == expected)
        {
            let underlying = self.underlying_type(actual);
            if matches!(self.get(underlying).kind, TypeKind::Function(_, _))
            {
                let string = self.builtin_id("String").unwrap();
                let function = self.intern(TypeKind::Function(Vec::new(), string));
                return self.accepts_signature_type(function, underlying);
            }
        }
        let target = self.get(expected);
        let source = self.get(actual);
        if let TypeKind::Function(_, _) = target.kind
        {
            if matches!(source.kind, TypeKind::Function(_, _))
            { return self.accepts_signature_type(expected, actual); }
            // Legacy bound references retain their string representation until coerced.
            if source.name == "String" { return true; }
        }
        if let TypeKind::Named(inner) = target.kind
        { return expected == actual || self.may_assign(inner, actual); }
        if let TypeKind::Union(items) = &target.kind
        { return items.iter().any(|item| self.may_assign(*item, actual)); }
        if let TypeKind::Union(items) = &source.kind
        { return items.iter().any(|item| self.may_assign(expected, *item)); }
        if let TypeKind::Optional(inner) = target.kind
        { return self.may_assign(inner, actual) || self.may_overlap(expected, actual); }
        if let TypeKind::Optional(inner) = source.kind
        { return self.may_assign(expected, inner) || self.may_overlap(expected, actual); }
        target.name == "Float" && source.name == "Integer" || self.may_overlap(expected, actual)
    }

    // Only uncommitted placeholders are completed; old definitions are immutable.
    pub fn finish(&mut self, id: TypeId, kind: TypeKind)
    {
        let mut definitions = self.definitions.borrow_mut();
        let old = &definitions[id.0];
        definitions[id.0] = Rc::new(TypeDefinition
            { id, name: old.name.clone(), location: old.location.clone(), error: old.error, kind });
    }

    pub fn intern(&self, kind: TypeKind) -> TypeId
    {
        if let Some(existing) =
            self.definitions.borrow()
                .iter()
                .find(|definition| match (&definition.kind, &kind)
                {
                    (TypeKind::ReceiverMember(a), TypeKind::ReceiverMember(b)) => a == b,
                    (TypeKind::Array(a), TypeKind::Array(b))
                    | (TypeKind::Optional(a), TypeKind::Optional(b)) => a == b,
                    (TypeKind::FixedArray(a), TypeKind::FixedArray(b)) => a == b,
                    (TypeKind::Union(a), TypeKind::Union(b)) => a == b,
                    (TypeKind::Function(a, r), TypeKind::Function(b, s)) => a == b && r == s,
                    (TypeKind::Map(a, b), TypeKind::Map(c, d)) => a == c && b == d,
                    _ => false,
                })
        {
            return existing.id;
        }
        let union_member = |id|
            {
                let definition = self.get(id);
                if matches!(definition.kind, TypeKind::Function(_, _))
                { format!("({})", definition.name) } else { definition.name.clone() }
            };
        let name = match &kind
            {
                TypeKind::ReceiverMember(name) => format!("$self.{}", name),
                TypeKind::Function(args, result) => format!("fn({}): {}", args.iter()
                    .map(|id| self.get(*id).name.clone()).collect::<Vec<_>>().join(", "),
                    self.get(*result).name),
                TypeKind::Array(id) => format!("[{}]", self.get(*id).name),
                TypeKind::FixedArray(items) => format!("[{}{}]", items.iter()
                    .map(|id| self.get(*id).name.clone()).collect::<Vec<_>>().join(", "),
                    if items.len() == 1 { "," } else { "" }),
                TypeKind::Map(key, value) =>
                    format!("[{}: {}]", self.get(*key).name, self.get(*value).name),
                TypeKind::Optional(id) => format!("{} | ()", union_member(*id)),
                TypeKind::Union(items) => items.iter()
                    .map(|id| union_member(*id)).collect::<Vec<_>>().join(" | "),
                _ => unreachable!("Only anonymous container constraints are interned")
            };
        self.register(name, kind, None)
    }

    // Apply numeric widening and named wrapping, then enforce the stored type.
    // Callers stage the value so failed conversions cannot mutate existing bindings.
    pub fn coerce(&self, id: TypeId, mut value: Value) -> Result<Value, String>
    {
        if let TypeKind::Named(inner) = self.get(id).kind
            && self.has_error_type(id)
            && crate::language::data::failure::marked(&value)
        {
            self.validate(inner, value.underlying())?;
            return Ok(value.underlying().clone());
        }
        // Function references use String storage, but Prompt must enforce their prototype.
        if    self.native_types.get("Prompt").is_some_and(|item| item.id == id)
           && value.is_callable()
        {
            let string = self.builtin_id("String").unwrap();
            let function = self.intern(TypeKind::Function(Vec::new(), string));
            let value = self.coerce(function, value.underlying().clone())?;
            return Ok(Value::Named(Rc::new(NamedValue
                { definition: self.get(id), value })));
        }
        if self.validate(id, &value).is_ok()
        {
            if matches!(self.get(id).kind, TypeKind::Array(_) | TypeKind::Map(_, _))
            { value = value.with_collection_type(id); }
            return Ok(value);
        }
        let definition = self.get(id);
        if let TypeKind::Named(inner) = definition.kind
        {
            let value = if let Value::Named(item) = &value
                && item.definition.id == id { item.value.clone() } else { value };
            return Ok(Value::Named(Rc::new(NamedValue
                { definition, value: self.coerce(inner, value)? })));
        }
        if let TypeKind::Function(parameters, result) = &definition.kind
        {
            self.check_callable(parameters, *result, &value)?;
            return Ok(Value::Callable(Rc::new(CallableValue
                { prototype: definition, target: value })));
        }
        if let TypeKind::Union(items) = &definition.kind
        {
            let mut result = None;
            for member in items
            {
                if let Ok(candidate) = self.coerce(*member, value.clone())
                {
                    if result.as_ref().is_some_and(|previous| previous != &candidate)
                    { return Err(format!("Ambiguous conversion to {}; convert explicitly",
                        definition.name)); }
                    result = Some(candidate);
                }
            }
            return result.ok_or_else(|| format!("Expected {}, got {}",
                definition.name, value.type_name()));
        }
        match (&definition.kind, &mut value)
        {
            (TypeKind::Builtin, Value::Integer(number)) if definition.name == "Float" =>
                value = Value::Float(*number as f64, None),
            (TypeKind::Optional(inner), value) if !matches!(value, Value::None) =>
                *value = self.coerce(*inner, value.clone())?,
            (TypeKind::Array(element), Value::Array(values)) =>
                for (index, value) in Rc::make_mut(values).iter_mut().enumerate()
                {
                    *value = self.coerce(*element, value.clone())
                        .map_err(|error| format!("Array element {}: {}", index, error))?;
                },
            (TypeKind::FixedArray(items), Value::Array(values)) if items.len() == values.len() =>
                for (index, (id, value)) in items.iter().zip(Rc::make_mut(values)).enumerate()
                {
                    *value = self.coerce(*id, value.clone())
                        .map_err(|error| format!("Array element {}: {}", index, error))?;
                },
            (TypeKind::Map(key_type, value_type), Value::HashMap(values)) =>
                {
                    let mut converted = HashMap::new();
                    for (key, value) in values.iter()
                    {
                        let key = self.coerce_key(*key_type, key)
                            .map_err(|error| format!("Map key: {}", error))?;
                        let value = self.coerce(*value_type, value.clone())
                            .map_err(|error| format!("Map value: {}", error))?;
                        if converted.insert(key, value).is_some()
                        { return Err("Map keys collide after conversion; convert keys explicitly"
                            .into()); }
                    }
                    Rc::make_mut(values).values = converted;
                },
            (TypeKind::Struct(fields), Value::Struct(item)) if item.definition.id == id =>
            {
                let constructing = item.constructing;
                for (field, value) in fields.iter().zip(&mut Rc::make_mut(item).fields)
                {
                    if    constructing && self.allow_candidates
                       && matches!(value, Value::None) { continue; }
                    *value = self.coerce(field.type_id, value.clone()).map_err(|error|
                        format!("Field '{}.{}' (declared at {}): {}", definition.name,
                                field.name, field.location, error))?;
                }
            },
            _ => {}
        }
        self.validate(id, &value)?;
        if value.collection_type().is_some()
            && matches!(definition.kind, TypeKind::Array(_) | TypeKind::Map(_, _))
        { value = value.with_collection_type(id); }
        Ok(value)
    }

    pub fn coerce_value(&self, mut value: Value) -> Result<Value, String>
    {
        if self.validate_value(&value).is_ok() { return Ok(value); }
        if let Some(id) = value.collection_type() { return self.coerce(id, value); }
        if let Value::Struct(item) = &value
        { value = self.coerce(item.definition.id, value)?; }
        if let Value::Named(item) = &value
        { return self.coerce(item.definition.id, value); }
        match &mut value
        {
            Value::Struct(item) =>
                for value in &mut Rc::make_mut(item).fields
                { *value = self.coerce_value(value.clone())?; },
            Value::Array(values) | Value::ArgumentExpansion(values) =>
                for value in Rc::make_mut(values) { *value = self.coerce_value(value.clone())?; },
            Value::HashMap(values) =>
                for value in Rc::make_mut(values).values_mut()
                { *value = self.coerce_value(value.clone())?; },
            _ => {}
        }
        Ok(value)
    }

    pub fn validate(&self, id: TypeId, value: &Value) -> Result<(), String>
    {
        let definition = self.get(id);
        let valid = match &definition.kind
            {
                TypeKind::Named(inner) if self.has_error_type(id)
                    && crate::language::data::failure::marked(value) =>
                        self.validate(*inner, value.underlying()).is_ok(),
                TypeKind::Named(inner) => if let Value::Named(item) = value
                    { item.definition.id == id && self.validate(*inner, &item.value).is_ok() }
                    else { false },
                TypeKind::Union(items) => items.iter().any(|id| self.validate(*id, value).is_ok()),
                TypeKind::Builtin => match definition.name.as_str()
                    {
                        "any" => true,
                        "ErrorValue" => matches!(value.underlying(),
                            Value::Struct(item) if item.definition.error),
                        "Number" => matches!(value, Value::Integer(_) | Value::Float(_, _)),
                        "Integer" => matches!(value, Value::Integer(_)),
                        "Float" => matches!(value, Value::Float(_, _)),
                        "String" => matches!(value, Value::String(_, _)),
                        "Type" => matches!(value, Value::Type(_)),
                        "Terminal" => matches!(value, Value::Terminal(_)),
                        "Boolean" => matches!(value, Value::Boolean(_)),
                        "None" => matches!(value, Value::None),
                        "ExecResult" => matches!(value, Value::ExecResult(_)),
                        "Array" => matches!(value, Value::Array(_)),
                        "HashMap" => matches!(value, Value::HashMap(_)),
                        "Range" => matches!(value, Value::Range(_)),
                        "ArgumentExpansion" => matches!(value, Value::ArgumentExpansion(_)),
                        _ => matches!(value.underlying(), Value::Native(item)
                            if item.0.type_name() == definition.name)
                    },
                TypeKind::Enum(_) => matches!(value, Value::Enum(item) if item.definition.id == id),
                TypeKind::Struct(fields) => if let Value::Struct(item) = value
                    {
                        if item.definition.id != id { false }
                        else
                        {
                            for (field, value) in fields.iter().zip(&item.fields)
                            {
                                if    item.constructing && self.allow_candidates
                                   && matches!(value, Value::None) { continue; }
                                self.validate(field.type_id, value).map_err(|error|
                                    format!("Field '{}.{}' (declared at {}): {}", definition.name,
                                            field.name, field.location, error))?;
                            }
                            fields.len() == item.fields.len()
                        }
                    } else { false },
                TypeKind::Optional(inner) =>
                    {
                        if matches!(value, Value::None) { true }
                        else { return self.validate(*inner, value); }
                    },
                TypeKind::Array(element) => if let Value::Array(values) = value
                    {
                        for (index, value) in values.iter().enumerate()
                        {
                            self.validate(*element, value)
                                .map_err(|error| format!("Array element {}: {}", index, error))?;
                        }
                        true
                    } else { false },
                TypeKind::FixedArray(items) => if let Value::Array(values) = value
                    {
                        if items.len() != values.len()
                        { return Err(format!("Expected {} array elements, got {}",
                            items.len(), values.len())); }
                        for (index, (id, value)) in items.iter().zip(values.iter()).enumerate()
                        {
                            self.validate(*id, value)
                                .map_err(|error| format!("Array element {}: {}", index, error))?;
                        }
                        true
                    } else { false },
                TypeKind::Map(key_type, value_type) => if let Value::HashMap(values) = value
                    {
                        for (key, value) in values.iter()
                        {
                            self.validate_key(*key_type, key)
                                .map_err(|error| format!("Map key: {}", error))?;
                            self.validate(*value_type, value)
                                .map_err(|error| format!("Map value: {}", error))?;
                        }
                        true
                    } else { false },
                TypeKind::Function(_, _) => matches!(value, Value::Callable(item)
                    if item.prototype.id == id),
                TypeKind::Pending | TypeKind::ReceiverMember(_) => false
            };
        if valid
        {
            Ok(())
        }
        else
        {
            Err(format!(
                "Expected {}, got {}",
                definition.name,
                value.type_name()
            ))
        }
    }

    // Validate every struct touched by an update, including structs inside untyped containers.
    pub fn validate_value(&self, value: &Value) -> Result<(), String>
    {
        if let Some(id) = value.collection_type() { self.validate(id, value)?; }
        match value
        {
            Value::Named(item) =>
                { self.validate(item.definition.id, value)?; self.validate_value(&item.value)?; },
            Value::Struct(item) =>
                {
                    self.validate(item.definition.id, value)?;
                    for value in &item.fields { self.validate_value(value)?; }
                },
            Value::Array(values) | Value::ArgumentExpansion(values) =>
                { for value in values.iter() { self.validate_value(value)?; } },
            Value::HashMap(values) =>
                { for value in values.values() { self.validate_value(value)?; } },
            _ => {}
        }
        Ok(())
    }

    pub fn check_required_cycles(&self, roots: &[TypeId]) -> Result<(), String>
    {
        fn visit(
            registry: &TypeRegistry, id: TypeId, path: &mut Vec<TypeId>, done: &mut HashSet<TypeId>,
        ) -> Result<(), String>
        {
            if let Some(start) = path.iter().position(|item| *item == id)
            {
                let mut names: Vec<_> = path[start..]
                    .iter()
                    .map(|id| registry.get(*id).name.clone())
                    .collect();
                names.push(registry.get(id).name.clone());
                return Err(format!(
                    "Required struct field cycle: {}. Use T | () or a container to terminate \
                        recursion",
                    names.join(" -> ")
                ));
            }
            if done.contains(&id) { return Ok(()); }
            path.push(id);
            match &registry.get(id).kind
            {
                TypeKind::Struct(fields) => for field in fields
                    { visit(registry, field.type_id, path, done)?; },
                TypeKind::Named(inner) => visit(registry, *inner, path, done)?,
                TypeKind::Union(items) =>
                {
                    let mut failure = None;
                    let mut terminated = false;
                    for item in items
                    {
                        match visit(registry, *item, &mut path.clone(), &mut done.clone())
                        {
                            Ok(()) => { terminated = true; break; },
                            Err(error) => failure = Some(error),
                        }
                    }
                    if !terminated { return Err(failure.unwrap()); }
                },
                _ => {},
            }
            path.pop();
            done.insert(id);
            Ok(())
        }
        let mut done = HashSet::new();
        for root in roots { visit(self, *root, &mut Vec::new(), &mut done)?; }
        Ok(())
    }

    pub fn check_named_cycles(&self, roots: &[TypeId]) -> Result<(), (TypeId, String)>
    {
        fn visit(registry: &TypeRegistry, id: TypeId, path: &mut Vec<TypeId>,
                 done: &mut HashSet<TypeId>) -> bool
        {
            if path.contains(&id) { return false; }
            if done.contains(&id) { return true; }
            path.push(id);
            let valid = match &registry.get(id).kind
                {
                    TypeKind::Named(inner) | TypeKind::Optional(inner)
                    | TypeKind::Array(inner) => visit(registry, *inner, path, done),
                    TypeKind::Map(key, value) => visit(registry, *key, path, done)
                        && visit(registry, *value, path, done),
                    TypeKind::Function(items, result) =>
                        items.iter().all(|id| visit(registry, *id, path, done))
                            && visit(registry, *result, path, done),
                    TypeKind::Union(items) | TypeKind::FixedArray(items) =>
                        items.iter().all(|id| visit(registry, *id, path, done)),
                    _ => true,
                };
            path.pop();
            if valid { done.insert(id); }
            valid
        }
        let mut done = HashSet::new();
        for id in roots
        {
            if !visit(self, *id, &mut Vec::new(), &mut done)
            { return Err((*id, format!("Recursive type definition '{}'", self.get(*id).name))); }
        }
        Ok(())
    }
}


impl TypeRegistry
{
    // Hash identity is canonical (1 == 1.0), but iteration must restore the
    // declared key representation, including metadata on nested collections.
    pub fn key_value(&self, id: TypeId, key: &MapKey) -> Result<Value, String>
    {
        let definition = self.get(id);
        let value = match (&definition.kind, key)
        {
            (TypeKind::Array(element), MapKey::Array(items)) => Value::from_array(
                items.iter().map(|key| self.key_value(*element, key))
                    .collect::<Result<Vec<_>, _>>()?).with_collection_type(id),
            (TypeKind::FixedArray(types), MapKey::Array(items)) if types.len() == items.len() =>
                Value::from_array(types.iter().zip(items.iter())
                    .map(|(id, key)| self.key_value(*id, key))
                    .collect::<Result<Vec<_>, _>>()?).with_collection_type(id),
            (TypeKind::Map(_, value_type), MapKey::HashMap(items)) => Value::from_hash_map(
                items.iter().map(|(key, value)|
                    Ok((key.clone(), self.key_value(*value_type, value)?)))
                    .collect::<Result<HashMap<_, _>, String>>()?).with_collection_type(id),
            (TypeKind::Optional(inner), _) if !matches!(key, MapKey::None) =>
                self.key_value(*inner, key)?,
            (TypeKind::Union(items), _) =>
                {
                    let member = items.iter().find(|id| self.validate_key(**id, key).is_ok())
                        .ok_or_else(|| format!("Invalid key for {}", definition.name))?;
                    self.key_value(*member, key)?
                },
            _ => self.coerce(id, key.to_value())?,
        };
        Ok(value)
    }


    fn coerce_key(&self, id: TypeId, key: &MapKey) -> Result<MapKey, String>
    {
        if self.validate_key(id, key).is_ok() { return Ok(key.clone()); }
        match &self.get(id).kind
        {
            TypeKind::Array(element) if matches!(key, MapKey::Array(_)) =>
                {
                    let MapKey::Array(values) = key else { unreachable!(); };
                    Ok(MapKey::Array(Rc::new(values.iter()
                        .map(|value| self.coerce_key(*element, value))
                        .collect::<Result<Vec<_>, _>>()?)))
                },
            TypeKind::FixedArray(items) if matches!(key, MapKey::Array(_)) =>
                {
                    let MapKey::Array(values) = key else { unreachable!(); };
                    if items.len() != values.len()
                    { return Err(format!("Expected {} array elements, got {}",
                        items.len(), values.len())); }
                    Ok(MapKey::Array(Rc::new(items.iter().zip(values.iter())
                        .map(|(id, value)| self.coerce_key(*id, value))
                        .collect::<Result<Vec<_>, _>>()?)))
                },
            TypeKind::Named(inner) =>
                {
                    let key = self.coerce_key(*inner, key)?;
                    Ok(MapKey::from_value(&self.coerce(id, key.to_value())?))
                },
            TypeKind::Map(key_type, value_type) if matches!(key, MapKey::HashMap(_)) =>
                {
                    let MapKey::HashMap(entries) = key else { unreachable!(); };
                    let mut converted = HashMap::new();
                    for (key, value) in entries.iter()
                    {
                        let key = self.coerce_key(*key_type, key)?;
                        let value = self.coerce_key(*value_type, value)?;
                        if converted.insert(key, value).is_some()
                        {
                            return Err("Map keys collide after conversion; \
                                convert keys explicitly".into());
                        }
                    }
                    let mut entries: Vec<_> = converted.into_iter().collect();
                    entries.sort();
                    Ok(MapKey::HashMap(Rc::new(entries)))
                },
            TypeKind::Optional(inner) => self.coerce_key(*inner, key),
            TypeKind::Union(items) =>
                {
                    let mut result = None;
                    for item in items
                    {
                        if let Ok(key) = self.coerce_key(*item, key)
                        {
                            if result.as_ref().is_some_and(|previous| previous != &key)
                            { return Err("Ambiguous map key conversion; \
                                convert explicitly".into()); }
                            result = Some(key);
                        }
                    }
                    result.ok_or_else(|| format!("Expected {}, got {}",
                        self.get(id).name, key.to_value().type_name()))
                },
            _ => { self.validate_key(id, key)?; Ok(key.clone()) },
        }
    }

    // Key annotations describe equivalence classes: integral floats and integers
    // already share one canonical key, including within collection keys.
    fn validate_key(&self, id: TypeId, key: &MapKey) -> Result<(), String>
    {
        let definition = self.get(id);
        match (&definition.kind, key)
        {
            (TypeKind::Union(items), _) =>
                {
                    if items.iter().any(|id| self.validate_key(*id, key).is_ok()) { return Ok(()); }
                },
            (TypeKind::Builtin, MapKey::Integer(integer)) if definition.name == "Float" =>
                {
                    let float = Value::Float(*integer as f64, None);
                    if float.equals(&Value::Integer(*integer)) { return Ok(()); }
                },
            (TypeKind::Optional(inner), _) if !matches!(key, MapKey::None) =>
                return self.validate_key(*inner, key),
            (TypeKind::Array(element), MapKey::Array(values)) =>
                {
                    for value in values.iter() { self.validate_key(*element, value)?; }
                    return Ok(());
                },
            (TypeKind::FixedArray(items), MapKey::Array(values)) if items.len() == values.len() =>
                {
                    for (id, value) in items.iter().zip(values.iter())
                    { self.validate_key(*id, value)?; }
                    return Ok(());
                },
            (TypeKind::Map(key_type, value_type), MapKey::HashMap(entries)) =>
                {
                    for (key, value) in entries.iter()
                    { self.validate_key(*key_type, key)?; self.validate_key(*value_type, value)?; }
                    return Ok(());
                },
            _ => {}
        }
        self.validate(id, &key.to_value())
    }
}
