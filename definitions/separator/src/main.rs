//! V2 Separator stub. Behavior is deliberately ignored until this family is built.
#![cfg_attr(systemrisc, no_std)]
#![cfg_attr(systemrisc, no_main)]
#![deny(unsafe_code)]
use mac_definition::{Message, Reply, Result, Separator, Services, cdef_main};

#[cdef_main]
fn main<'a>(message: Message<'a, Separator<'a>>, services: Services<'a>) -> Result<Reply>
{
    mac_definition::stub::reply(message, services)
}
