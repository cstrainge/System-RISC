
use std::{ cell::{ Cell, Ref, RefCell, RefMut }, collections::{ HashMap, VecDeque },
           env::vars, rc::Rc };

use crate::language::data::{ methods::BoundMethod, value::Value, types::TypeId };

#[derive(Clone, PartialEq, Eq)]
pub enum ValueVisibility
{
    Private,
    Exported
}


#[derive(Clone)]
pub struct ScopedValue
{
    pub value: Value,
    pub type_id: Option<TypeId>,
    pub exported: ValueVisibility,
    pub reference: Option<ValueReference>,
    // An explicit union conversion can retain its interface on an untyped binding.
    pub dispatch_type: Option<TypeId>,
    pub environment_conversion: Option<Rc<EnvironmentConversion>>,
}


// Retyping preserves the original environment representation and the selected methods.
pub struct EnvironmentConversion
{
    pub original_type: TypeId,
    pub value_type: TypeId,
    pub methods: Value,
    pub active: Cell<bool>,
}


// A stable binding and an evaluated path into it. Capturing a receiver does not turn
// ordinary Value copies into aliases. References can outlive their declaring scope.
#[derive(Clone)]
pub struct ValueReference
{
    pub root: Rc<RefCell<ScopedValue>>,
    pub indexes: Vec<Value>,
    // true selects a data field, false an index; None projects a named payload.
    pub fields: Vec<Value>,
    pub constraints: Vec<(usize, TypeId)>,
    // The explicit interface selected by the compiler, including narrowed members.
    pub dispatch_type: Option<TypeId>,
    pub property: Option<Rc<PropertyReference>>,
}

// A getter projection keeps its evaluated value for one operation and writes
// changes back through the original owner's setter. Saved methods refresh it.
pub struct PropertyReference
{
    pub getter: Rc<BoundMethod>,
    pub setter: Option<Rc<BoundMethod>>,
    pub name: String,
    pub fresh: Cell<bool>,
    pub active: Cell<bool>,
}

impl ValueReference
{
    pub fn temporary(value: Value) -> Self
    {
        Self
            {
                root: Rc::new(RefCell::new(ScopedValue
                    {
                        value,
                        type_id: None,
                        exported: ValueVisibility::Private,
                        reference: None,
                        dispatch_type: None,
                        environment_conversion: None,
                    })),
                indexes: Vec::new(),
                fields: Vec::new(),
                constraints: Vec::new(),
                dispatch_type: None,
                property: None,
            }
    }
}


#[derive(Clone)]
pub struct ScopedVariables
{
    scopes: VecDeque<HashMap<String, Rc<RefCell<ScopedValue>>>>,
}


impl ScopedVariables
{
    pub fn new_from_environment() -> Self
    {
        let mut variables = HashMap::new();

        for (name, value) in vars()
        {
            let scoped_value = ScopedValue
                {
                    value: Value::from_string(value),
                    type_id: None,
                    exported: ValueVisibility::Exported,
                    reference: None,
                    dispatch_type: None,
                    environment_conversion: None,
                };

            variables.insert(format!("${}", name), Rc::new(RefCell::new(scoped_value)));
        }

        let mut scopes = VecDeque::new();

        scopes.push_back(variables);

        Self
            {
                scopes
            }
    }

    pub fn binding(&self, name: &str) -> Option<Rc<RefCell<ScopedValue>>>
    {
        self.scopes.iter().rev().find_map(|scope| scope.get(name).cloned())
    }

    pub fn import(&mut self, name: String, binding: Rc<RefCell<ScopedValue>>)
    {
        self.scopes.back_mut().expect("A variable scope must exist").insert(name, binding);
    }

    pub fn global_binding(&self, name: &str) -> Option<Rc<RefCell<ScopedValue>>>
    {
        self.scopes.front().unwrap().get(name).cloned()
    }

    pub fn import_global(&mut self, name: String, binding: Rc<RefCell<ScopedValue>>)
    {
        self.scopes.front_mut().unwrap().insert(name, binding);
    }

    pub fn remove_global_binding(&mut self, name: &str, binding: &Rc<RefCell<ScopedValue>>)
        -> bool
    {
        let globals = self.scopes.front_mut().unwrap();
        if globals.get(name).is_some_and(|existing| Rc::ptr_eq(existing, binding))
        {
            globals.remove(name);
            return true;
        }
        false
    }

    pub fn global_bindings(&self) -> Self
    {
        // A closure's fallback is its module, excluding the invoking function's locals.
        Self { scopes: VecDeque::from([self.scopes.front().unwrap().clone()]) }
    }

    pub fn empty() -> Self
    { Self { scopes: VecDeque::from([HashMap::new()]) } }

    // Both environments keep the original bindings alive and share their writes.
    pub fn overlay(&mut self, captured: &Self)
    { self.scopes.push_back(captured.scopes.front().unwrap().clone()); }

    pub fn push_scope(&mut self)
    {
        self.scopes.push_back(HashMap::new());
    }

    pub fn pop_scope(&mut self)
    {
        if self.scopes.len() == 1
        {
            panic!("Cannot pop the global scope!");
        }

        self.scopes.pop_back();
    }

    pub fn create(&mut self, name: String, value: ScopedValue) -> Result<(), String>
    {
        if let Some(scope) = self.scopes.back_mut()
        {
            scope.insert(name, Rc::new(RefCell::new(value)));
            Ok(())
        }
        else
        {
            Err("No scope available to create the variable.".to_string())
        }
    }

    pub fn get(&self, name: &str) -> Option<Ref<'_, ScopedValue>>
    {
        for scope in self.scopes.iter().rev()
        {
            if let Some(value) = scope.get(name)
            {
                return Some(value.borrow());
            }
        }

        None
    }

    pub fn get_mut(&mut self, name: &str) -> Option<RefMut<'_, ScopedValue>>
    {
        for scope in self.scopes.iter_mut().rev()
        {
            if let Some(value) = scope.get_mut(name)
            {
                return Some(value.borrow_mut());
            }
        }

        None
    }

    pub fn reference(&self, name: &str) -> Option<ValueReference>
    {
        for scope in self.scopes.iter().rev()
        {
            if let Some(root) = scope.get(name)
            {
                if let Some(reference) = &root.borrow().reference
                { return Some(reference.clone()); }
                return Some(ValueReference
                    {
                        root: root.clone(),
                        indexes: Vec::new(),
                        fields: Vec::new(),
                        constraints: Vec::new(),
                        dispatch_type: None,
                        property: None,
                    });
            }
        }
        None
    }

    pub fn names(&self) -> impl Iterator<Item = &str>
    {
        self.scopes.iter().flat_map(|scope| scope.keys().map(String::as_str))
    }

    pub fn get_all_flattened(&self) -> HashMap<String, ScopedValue>
    {
        let mut flattened = HashMap::new();

        for scope in self.scopes.iter()
        {
            for (name, value) in scope.iter()
            {
                flattened.insert(name.clone(), value.borrow().clone());
            }
        }

        flattened
    }

    pub fn current_scope(&self) -> usize
    {
        self.scopes.len() - 1
    }

    pub fn reset_to_scope(&mut self, scope_index: usize)
    {
        while self.scopes.len() > scope_index + 1
        {
            self.scopes.pop_back();
        }
    }
}
