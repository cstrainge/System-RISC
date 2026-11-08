
use std::{ collections::HashMap, rc::Rc };

use crate::language::{ data::{ conversion::is_conversion_method,
                               map_key::MapKey, methods::{ BoundMethod, MethodDefinition },
                               types::{ EnumValue, TypeDefinition, TypeKind },
                               value::{ Executable, Value } },
                       interpreter::Interpreter };


impl Interpreter
{
    pub(super) fn add_property_methods(&self, receiver: &Value, method: &BoundMethod,
                                      properties: &mut Value)
    {
        if let Value::Type(definition) = receiver.underlying()
        {
            self.add_type_properties(definition, &method.snapshot, properties);
            return;
        }
        let Value::HashMap(members) = properties else { unreachable!(); };
        let members = Rc::make_mut(members);
        let id = self.scope().types.value_type(receiver);
        members.insert(MapKey::String("type".into()), Value::Type(self.scope().types.get(id)));
        for name in ["key_type", "value_type"]
        {
            if let Some(value) = self.collection_property(
                &Default::default(), &method.receiver, receiver, name)
            { members.insert(MapKey::String(name.into()), value); }
        }
        let mut names = self.scope().types.method_names(id);
        let selected = method.receiver.dispatch_type.or_else(||
            {
                if method.receiver.indexes.is_empty()
                { let root = method.receiver.root.borrow(); root.type_id.or(root.dispatch_type) }
                else { None }
            });
        if let Some(id) = selected.and_then(|id| self.scope().types.union_interface(id))
        { names.extend(self.scope().types.method_names(id)); }
        let mut underlying = receiver;
        while let Value::Named(item) = underlying
        {
            underlying = &item.value;
            names.extend(self.scope().types.method_names(
                self.scope().types.value_type(underlying)));
        }
        if let Value::Struct(item) = underlying
        {
            for (owner, name) in self.scope().types.properties.keys()
            {
                if *owner != item.definition.id { continue; }
                let key = MapKey::String(name.clone());
                members.remove(&key);
                if let Some(bound) = self.resolve_method(receiver, method.receiver.clone(),
                    &format!("get_{}", name), &method.snapshot)
                {
                    members.insert(key, Value::String(
                        format!("{}.{}", receiver.type_name(), name),
                        Executable::Method(Rc::new(bound))));
                }
            }
        }
        // A saved reflector keeps lexical method versions even after a module reload.
        if let Value::HashMap(snapshot) = &method.snapshot
        {
            names.extend(snapshot.keys().filter_map(|key| match key
                {
                    MapKey::String(key) => key.rsplit(':').next().map(str::to_string),
                    _ => None,
                }));
        }
        for name in names
        {
            let key = MapKey::String(name.clone());
            if is_conversion_method(&name) || name == "type" { continue; }
            if let Some(bound) = self.resolve_method(receiver, method.receiver.clone(),
                                                    &name, &method.snapshot)
            {
                if members.contains_key(&key) && !self.is_union_method(&bound) { continue; }
                if self.collection_property(&Default::default(), &method.receiver,
                    receiver, &name).is_some() { continue; }
                members.insert(key, Value::String(format!("{}.{}", receiver.type_name(), name),
                    Executable::Method(Rc::new(bound))));
            }
        }
    }

    fn add_type_properties(&self, definition: &Rc<TypeDefinition>, snapshot: &Value,
                           properties: &mut Value)
    {
        let registry = &self.scope().types;
        let id = definition.id;
        let underlying = registry.get(registry.underlying_type(id));
        let type_value = |id| Value::Type(registry.get(id));
        let any = registry.builtin_id("any").unwrap();
        let mut members: HashMap<String, Value> = HashMap::new();
        if let TypeKind::Struct(fields) = &underlying.kind
        {
            for field in fields.iter().filter(|field| field.public)
            { members.insert(field.name.clone(), type_value(field.type_id)); }
        }
        else
        {
            members.insert("value".into(), Value::Type(definition.clone()));
            if matches!(underlying.kind, TypeKind::Enum(_))
            { members.insert("index".into(),
                type_value(registry.builtin_id("Integer").unwrap())); }
            if underlying.name == "Type"
            { members.insert("name".into(),
                type_value(registry.builtin_id("String").unwrap())); }
        }
        if let TypeKind::Enum(variants) = &definition.kind
        {
            for (variant, item) in variants.iter().enumerate()
            {
                members.entry(item.name.clone()).or_insert_with(||
                    Value::Enum(Rc::new(EnumValue { definition: definition.clone(), variant })));
            }
        }
        let value_type = registry.collection_member_type(id, "value_type");
        let key_type = registry.collection_member_type(id, "key_type");
        for (name, value) in [("value_type", value_type), ("key_type", key_type)]
        {
            if let Some(value) = value
            { members.insert(name.into(), type_value(value)); }
        }
        let owners = registry.method_types(id);
        let base = registry.method_types(underlying.id)[0];
        let lookup = |name: &str| owners.iter().find_map(|owner|
            self.method_definition(*owner,
                if *owner == base { id } else { *owner }, name, snapshot));
        for ((owner, name), field_type) in &registry.properties
        {
            if !owners.contains(owner) { continue; }
            members.remove(name);
            if lookup(&format!("get_{}", name)).is_some()
            { members.insert(name.clone(), type_value(*field_type)); }
        }
        let mut names = registry.method_names(id);
        if let Value::HashMap(snapshot) = snapshot
        {
            names.extend(snapshot.keys().filter_map(|key| match key
                {
                    MapKey::String(key) => key.rsplit(':').next().map(str::to_string),
                    _ => None,
                }));
        }
        for name in names
        {
            if members.contains_key(&name) { continue; }
            let factory = is_conversion_method(&name)
                || matches!(name.as_str(), "new" | "json_codec");
            let method = if factory
                {
                    self.conversion_owners(id).iter().find_map(|owner|
                        self.method_definition(*owner, *owner, &name, snapshot))
                }
                else { lookup(&name) };
            let Some(method) = method else { continue; };
            let specialize = |id| registry.specialize_receiver_type(id, value_type, key_type);
            let (arguments, result) = match method
                {
                    MethodDefinition::Builtin(method) =>
                        (vec![any; method.argument_count], method.return_type),
                    MethodDefinition::User(function) =>
                        {
                            let receiver = usize::from(function.arguments.first()
                                .is_some_and(|name| name == "$self"));
                            (function.argument_types.iter().skip(receiver)
                                .map(|id| id.unwrap_or(any)).collect(),
                             function.return_type.unwrap_or(registry.builtin_id("None").unwrap()))
                        },
                };
            let signature = registry.intern(TypeKind::Function(
                arguments.into_iter().map(specialize).collect(), specialize(result)));
            members.insert(name, type_value(signature));
        }
        members.insert("type".into(), Value::Type(definition.clone()));
        *properties = Value::from_hash_map(members.into_iter()
            .map(|(name, value)| (MapKey::String(name), value)).collect());
    }
}
