
use std::slice::from_ref;

use crate::language::{ data::{ scoped_variables::ValueReference,
                               types::{ TypeId, TypeKind, TypeRegistry }, value::Value },
                       interpreter::Interpreter, text::location::Location };

fn declared_member_type(registry: &TypeRegistry, id: TypeId, value: &Value,
                        name: &str) -> Option<TypeId>
{
    let definition = registry.get(registry.underlying_type(id));
    match &definition.kind
    {
        TypeKind::Builtin => None,
        TypeKind::Optional(inner) => declared_member_type(registry, *inner, value, name),
        TypeKind::Union(items) =>
            {
                let members: Option<Vec<_>> = items.iter()
                    .filter(|id| registry.validate(**id, value).is_ok())
                    .map(|id| declared_member_type(registry, *id, value, name)).collect();
                members.filter(|items| !items.is_empty()).map(|items| registry.common_type(items))
            },
        _ => registry.collection_member_type(definition.id, name),
    }
}

fn intrinsic_type(registry: &TypeRegistry, value: &Value) -> Option<TypeId>
{
    value.collection_type().or_else(||
        matches!(value, Value::Named(_) | Value::Struct(_)).then(|| registry.value_type(value)))
}


// Project annotations through the same evaluated path used by live receivers.
fn project_type(registry: &TypeRegistry, id: TypeId, value: &Value,
                index: &Value, field: &Value) -> Option<TypeId>
{
    let id = registry.underlying_type(id);
    let definition = registry.get(id);
    match &definition.kind
    {
        TypeKind::Optional(inner) => project_type(registry, *inner, value, index, field),
        TypeKind::Union(items) =>
            {
                let ids: Option<Vec<_>> = items.iter()
                    .filter(|id| registry.validate(**id, value).is_ok())
                    .map(|id| project_type(registry, *id, value, index, field)).collect();
                ids.filter(|items| !items.is_empty()).map(|items| registry.union_type(items))
            },
        _ if matches!(field, Value::None) => Some(id),
        TypeKind::Struct(fields) if matches!(field, Value::Boolean(true)) =>
            fields.iter().find(|item| item.name == index.as_text()).map(|item| item.type_id),
        TypeKind::Array(item) if matches!(field, Value::Boolean(false)) =>
            Some(if matches!(index, Value::Range(_)) { id } else { *item }),
        TypeKind::FixedArray(items) if matches!(field, Value::Boolean(false)) => match index
            {
                Value::Integer(index) => usize::try_from(*index).ok()
                    .and_then(|index| items.get(index).copied()),
                Value::Range(range) => Some(registry.intern(TypeKind::FixedArray(items.iter()
                    .enumerate().filter(|(index, _)| range.contains(*index as i64))
                    .map(|(_, id)| *id).collect()))),
                _ => None,
            },
        TypeKind::Map(_, item) if matches!(field, Value::Boolean(false)) => Some(*item),
        _ => None,
    }
}


impl TypeRegistry
{
    pub fn receiver_member_type(&self, location: &Location, reference: &ValueReference,
                                value: &Value, name: &str) -> Option<TypeId>
    {
        match (value.underlying(), name)
        {
            (Value::Array(_), "value_type")
            | (Value::HashMap(_), "key_type" | "value_type") => {},
            _ => return None,
        }
        let registry = self;
        let root = reference.root.borrow();
        let candidates = [(0, root.type_id)].into_iter()
            .chain(reference.constraints.iter().map(|(depth, id)| (*depth, Some(*id))));
        for (depth, id) in candidates
        {
            let mut current = Interpreter::read_projection(location, root.value.clone(),
                &reference.indexes[..depth], &reference.fields[..depth]).ok()?;
            let mut declared = id;
            for (index, field) in reference.indexes[depth..].iter()
                .zip(&reference.fields[depth..])
            {
                declared = declared.or_else(|| intrinsic_type(registry, &current))
                    .and_then(|id| project_type(registry, id, &current, index, field));
                current = Interpreter::read_projection(location, current,
                    from_ref(index), from_ref(field)).ok()?;
            }
            if let Some(id) = declared.or_else(|| intrinsic_type(registry, &current))
            {
                // Generic Array/HashMap/any constraints specify no contained type.
                if let Some(item) = declared_member_type(registry, id, &current, name)
                { return Some(item); }
            }
        }
        if let Some(id) = intrinsic_type(registry, value)
            && let Some(item) = declared_member_type(registry, id, value, name)
        { return Some(item); }
        let id = registry.inferred_value_type(value.underlying());
        registry.collection_member_type(id, name)
    }

    pub fn bound_receiver_types(&self, reference: &ValueReference)
        -> Result<(Option<TypeId>, Option<TypeId>), String>
    {
        let location = Location::default();
        let value = Interpreter::read_projection(&location, reference.root.borrow().value.clone(),
            &reference.indexes, &reference.fields).map_err(|error| error.to_string())?;
        Ok((self.receiver_member_type(&location, reference, &value, "value_type"),
            self.receiver_member_type(&location, reference, &value, "key_type")))
    }

    pub fn has_receiver_type(&self, id: TypeId) -> bool
    {
        match &self.get(id).kind
        {
            TypeKind::ReceiverMember(_) => true,
            TypeKind::Array(inner) | TypeKind::Optional(inner) => self.has_receiver_type(*inner),
            TypeKind::FixedArray(items) | TypeKind::Union(items) =>
                items.iter().any(|id| self.has_receiver_type(*id)),
            TypeKind::Map(key, value) => self.has_receiver_type(*key)
                || self.has_receiver_type(*value),
            TypeKind::Function(args, result) => args.iter().any(|id| self.has_receiver_type(*id))
                || self.has_receiver_type(*result),
            _ => false,
        }
    }

    // Callers snapshot the receiver metadata before any parameter or body can mutate it.
    // During static analysis a missing member remains dynamic.
    pub fn specialize_receiver_type(&self, id: TypeId, value: Option<TypeId>,
                                    key: Option<TypeId>) -> TypeId
    {
        if !self.has_receiver_type(id) { return id; }
        let specialize = |id| self.specialize_receiver_type(id, value, key);
        let kind = match &self.get(id).kind
        {
            TypeKind::ReceiverMember(name) => return
                (if *name == "key_type" { key } else { value })
                    .unwrap_or_else(|| self.builtin_id("any").unwrap()),
            TypeKind::Array(inner) => TypeKind::Array(specialize(*inner)),
            TypeKind::Optional(inner) => TypeKind::Optional(specialize(*inner)),
            TypeKind::FixedArray(items) =>
                TypeKind::FixedArray(items.iter().map(|id| specialize(*id)).collect()),
            TypeKind::Union(items) => return
                self.union_type(items.iter().map(|id| specialize(*id)).collect()),
            TypeKind::Map(key, value) => TypeKind::Map(specialize(*key), specialize(*value)),
            TypeKind::Function(args, result) => TypeKind::Function(
                args.iter().map(|id| specialize(*id)).collect(), specialize(*result)),
            _ => return id,
        };
        self.intern(kind)
    }
}
