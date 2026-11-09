//! V2 Tree stub. Behavior is deliberately ignored until this family is built.
#![cfg_attr(systemrisc, no_std)]
#![cfg_attr(systemrisc, no_main)]
#![deny(unsafe_code)]
use mac_definition::{Message, Reply, Result, Services, Tree, cdef_main};

#[cdef_main]
fn main<'a>(message: Message<'a, Tree<'a>>, services: Services<'a>) -> Result<Reply>
{
    mac_definition::stub::reply(message, services)
}
