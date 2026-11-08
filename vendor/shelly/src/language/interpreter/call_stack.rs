
use crate::language::text::location::Location;

/// A call that was active when an interpreter error occurred, ordered innermost first.
#[derive(Clone, Debug)]
pub struct CallFrame
{
    pub name: String,
    /// Shelly source location at which the call was made.
    pub location: Location,
    pub native: bool,
    /// Rust dispatch boundary for a native call, separate from the failure's origin.
    pub native_location: Option<Location>,
}
