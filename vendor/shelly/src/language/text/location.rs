
use std::{ fmt::{ self, Debug, Display, Formatter }, sync::Arc };

/**
 * Allow the caller to take the current location in the Rust source code.
 */
#[macro_export]
macro_rules! location_here
{
    () => {{
        use $crate::language::text::location::Location;

        Location::new(file!(), line!() as usize, column!() as usize)
    }};
}


/**
 * Represents a location in the source code.
 */
#[derive(Clone, PartialEq, PartialOrd, Eq)]
pub struct Location
{
    /**
     * The name of the location the source code originates from. Can be things like a file name/path
     * or a keyword like "<repl>". Used for knowing where in the what source code this location is
     * coming from.
     */
    pub origin: Arc<str>,

    /**
     * A 1-based line number in the source code.
     */
    pub line: usize,

    /**
     * A 1-based column number in the source code.
     */
    pub column: usize
}


impl Location
{
    /**
     * Construct a new location with the given origin, line, and column.
     */
    pub fn new(origin: impl Into<Arc<str>>, line: usize, column: usize) -> Self
    {
        Self { origin: origin.into(), line, column }
    }
}


impl Default for Location
{
    /**
     * Construct a default location with an unspecified origin and the first line and column.
     */
    fn default() -> Self
    {
        Self { origin: Arc::from("<unspecified>"), line: 1, column: 1 }
    }
}


impl Display for Location
{
    /**
     * Format the location as a string in the form "origin: (line, column)".
     */
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result
    {
        write!(formatter, "{}: ({}, {})", self.origin, self.line, self.column)
    }
}


impl Debug for Location
{
    /**
     * Format the location for debugging purposes.
     */
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result
    {
        write!(formatter, "{}", self)
    }
}
