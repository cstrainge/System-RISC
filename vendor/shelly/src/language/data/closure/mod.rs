
mod captures;

use std::{ cmp::Ordering, fmt::{ self, Debug, Formatter }, hash::{ Hash, Hasher }, rc::Rc };

use crate::language::{ bytecode::FunctionRef, data::scoped_variables::ScopedVariables };

pub use crate::language::data::closure::captures::free_variables;

pub struct ClosureValue
{
    pub function: FunctionRef,
    pub captures: ScopedVariables,
    // Each evaluation of a literal creates a new callable identity.
    identity: Rc<()>,
}

impl ClosureValue
{
    pub fn new(function: FunctionRef, captures: ScopedVariables) -> Self
    { Self { function, captures, identity: Rc::new(()) } }
}

impl Debug for ClosureValue
{
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result
    { write!(f, "Closure({:p})", Rc::as_ptr(&self.identity)) }
}
impl PartialEq for ClosureValue
{
    fn eq(&self, other: &Self) -> bool { Rc::ptr_eq(&self.identity, &other.identity) }
}
impl Eq for ClosureValue {}
impl PartialOrd for ClosureValue
{
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> { Some(self.cmp(other)) }
}
impl Ord for ClosureValue
{
    fn cmp(&self, other: &Self) -> Ordering
    { Rc::as_ptr(&self.identity).cmp(&Rc::as_ptr(&other.identity)) }
}
impl Hash for ClosureValue
{
    fn hash<H: Hasher>(&self, state: &mut H) { Rc::as_ptr(&self.identity).hash(state); }
}
