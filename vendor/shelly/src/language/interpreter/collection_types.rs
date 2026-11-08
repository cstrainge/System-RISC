
use crate::language::{ data::{ scoped_variables::ValueReference, value::Value },
                       interpreter::Interpreter, text::location::Location };

impl Interpreter
{
    pub(super) fn collection_property(&self, location: &Location, reference: &ValueReference,
                                      value: &Value, name: &str) -> Option<Value>
    {
        let registry = &self.scope().types;
        registry.receiver_member_type(location, reference, value, name)
            .map(|id| Value::Type(registry.get(id)))
    }
}
