
use std::{ cmp::Ordering, hash::{ Hash, Hasher }, rc::Rc };

use crate::language::{ bytecode::Function,
                       data::{ methods::MethodDefinition,
                               types::{ TypeDefinition, TypeId, TypeKind, TypeRegistry },
                               value::{ Executable, Value } } };

// A prototype is a checked view of a callable. Keeping the original value also
// preserves prior contracts, function versions, and live bound receivers.
#[derive(Clone, Debug)]
pub struct CallableValue
{
    pub prototype: Rc<TypeDefinition>,
    pub target: Value,
}

impl CallableValue
{
    fn identity(&self) -> Vec<usize>
    {
        let mut identity = vec![self.prototype.id.0];
        match &self.target
        {
            Value::Callable(value) =>
                { identity.push(0); identity.extend(value.identity()); },
            Value::Closure(value) => identity.extend([4, Rc::as_ptr(value) as usize]),
            Value::String(_, Executable::Function(value)) =>
                identity.extend([1, Rc::as_ptr(value) as usize]),
            Value::String(_, Executable::Native(value)) =>
                identity.extend([2, Rc::as_ptr(value) as usize]),
            Value::String(_, Executable::Method(value)) =>
                identity.extend([3, Rc::as_ptr(value) as usize]),
            _ => unreachable!("Prototype targets are bound callables"),
        }
        identity
    }
}

impl PartialEq for CallableValue
{
    fn eq(&self, other: &Self) -> bool { self.identity() == other.identity() }
}
impl Eq for CallableValue {}
impl PartialOrd for CallableValue
{
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> { Some(self.cmp(other)) }
}
impl Ord for CallableValue
{
    fn cmp(&self, other: &Self) -> Ordering { self.identity().cmp(&other.identity()) }
}
impl Hash for CallableValue
{
    fn hash<H: Hasher>(&self, state: &mut H) { self.identity().hash(state); }
}

impl TypeRegistry
{
    pub fn accepts_result_signature(&self, expected: TypeId, actual: TypeId) -> bool
    {
        self.success_type(actual)
            .is_none_or(|actual| self.accepts_signature_type(expected, actual))
    }

    // Unlike the conservative overlap check, every known alternative must fit.
    // Explicit any on either side opts into call-time validation.
    pub fn accepts_signature_type(&self, expected: TypeId, actual: TypeId) -> bool
    {
        if expected == actual { return true; }
        let any = self.builtin_id("any").unwrap();
        if expected == any || actual == any { return true; }
        let target = self.get(expected);
        let source = self.get(actual);
        match (&target.kind, &source.kind)
        {
            (_, TypeKind::Union(items)) => items.iter()
                .all(|item| self.accepts_signature_type(expected, *item)),
            (_, TypeKind::Optional(inner)) =>
                self.accepts_signature_type(expected, *inner)
                    && self.accepts_signature_type(expected, self.builtin_id("None").unwrap()),
            (TypeKind::Union(items), _) => items.iter()
                .any(|item| self.accepts_signature_type(*item, actual)),
            (TypeKind::Optional(inner), _) =>
                actual == self.builtin_id("None").unwrap()
                    || self.accepts_signature_type(*inner, actual),
            (TypeKind::Named(inner), _) => self.accepts_signature_type(*inner, actual),
            (TypeKind::Array(a), TypeKind::Array(b)) => self.accepts_signature_type(*a, *b),
            (TypeKind::Array(a), TypeKind::FixedArray(items)) => items.iter()
                .all(|item| self.accepts_signature_type(*a, *item)),
            (TypeKind::FixedArray(a), TypeKind::FixedArray(b)) => a.len() == b.len()
                && a.iter().zip(b).all(|(a, b)| self.accepts_signature_type(*a, *b)),
            (TypeKind::Map(a, b), TypeKind::Map(c, d)) =>
                self.accepts_signature_type(*a, *c) && self.accepts_signature_type(*b, *d),
            (TypeKind::Function(a, result), TypeKind::Function(b, returned)) =>
                a.len() == b.len() && a.iter().zip(b)
                    .all(|(a, b)| self.accepts_signature_type(*b, *a))
                    && self.accepts_result_signature(*result, *returned),
            (TypeKind::Function(_, _), _) => false,
            (TypeKind::Builtin, TypeKind::Named(_)) => false,
            _ => self.may_assign(expected, actual),
        }
    }

    pub fn check_callable(
        &self,
        parameters: &[TypeId],
        result: TypeId,
        value: &Value,
    ) -> Result<(), String>
    {
        let mismatch = || "Function signature is incompatible with prototype".to_string();
        let check_result = |actual|
            {
                if self.accepts_result_signature(result, actual) { Ok(()) }
                else { Err(mismatch()) }
            };
        match value
        {
            Value::Callable(value) =>
                {
                    let TypeKind::Function(args, returned) = &value.prototype.kind
                        else { unreachable!(); };
                    if parameters.len() != args.len() || !args.iter().zip(parameters)
                        .all(|(actual, expected)| self.accepts_signature_type(*actual, *expected))
                    { return Err(mismatch()); }
                    check_result(*returned)
                },
            Value::Closure(value) => self.check_function(parameters, result, &value.function, 0, (None, None)),
            Value::String(_, Executable::Function(function)) =>
                self.check_function(parameters, result, function, 0, (None, None)),
            Value::String(_, Executable::Native(_)) => Ok(()),
            Value::String(_, Executable::Method(method)) => match &method.definition
                {
                    MethodDefinition::User(function) =>
                        {
                            let members = self.bound_receiver_types(&method.receiver)?;
                            self.check_function(parameters, result, function, 1, members)
                        },
                    MethodDefinition::Builtin(method) =>
                        {
                            if parameters.len() != method.argument_count
                            { return Err(mismatch()); }
                            check_result(method.return_type)
                        },
                },
            _ => Err("A function prototype requires a bound function or method reference".into()),
        }
    }

    fn check_function(
        &self,
        parameters: &[TypeId],
        result: TypeId,
        function: &Function,
        skip: usize,
        members: (Option<TypeId>, Option<TypeId>),
    ) -> Result<(), String>
    {
        let count = parameters.len() + skip;
        if    count < function.minimum_arguments
           || !function.variadic && count > function.arguments.len()
        { return Err("Function arity is incompatible with prototype".into()); }
        let fixed = function.arguments.len() - usize::from(function.variadic);
        let any = self.builtin_id("any").unwrap();
        let specialize = |id| self.specialize_receiver_type(id, members.0, members.1);
        for (index, expected) in parameters.iter().enumerate()
        {
            let index = index + skip;
            let actual = if index < fixed
                { specialize(function.argument_types[index].unwrap_or(any)) }
                else
                {
                    let rest = specialize(function.argument_types[fixed].unwrap_or(any));
                    match self.get(rest).kind
                    {
                        TypeKind::Array(item) => item,
                        _ => any,
                    }
                };
            if !self.accepts_signature_type(actual, *expected)
            { return Err(format!("Function parameter {} is incompatible with prototype",
                index + 1)); }
        }
        if !self.accepts_result_signature(result, specialize(function.return_type.unwrap_or(any)))
        { return Err("Function return type is incompatible with prototype".into()); }
        Ok(())
    }
}
