//! V2 ListEdit stub. Behavior is deliberately ignored until this family is built.
#![cfg_attr(systemrisc, no_std)]
#![cfg_attr(systemrisc, no_main)]
#![deny(unsafe_code)]
use mac_definition::{ListEdit, Message, Reply, Result, Services, cdef_main};

#[cdef_main]
fn main<'a>(message: Message<'a, ListEdit<'a>>, services: Services<'a>) -> Result<Reply>
{
    mac_definition::stub::reply(message, services)
}
