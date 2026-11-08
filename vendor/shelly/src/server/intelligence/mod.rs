
mod model;
mod project;
mod queries;

use crate::language::analysis::SymbolSnapshot;

pub(super) use crate::server::intelligence::project::{ Project, uri_path };
pub(super) use crate::server::intelligence::queries::offset;

pub(super) const MAX_BYTES: usize = 1024 * 1024;
pub(super) const MAX_STEPS: usize = 640_000;

impl Project
{
    pub fn new(snapshot: &SymbolSnapshot) -> Self
    { Self::baseline(snapshot) }
}
