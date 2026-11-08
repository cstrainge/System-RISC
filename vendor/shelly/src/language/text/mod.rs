
/**
 * This module holds the location based struct for keeping track of locations in source code.
 */
pub mod location;
pub mod span;
pub mod diagnostic;
pub mod interpolation;

/**
 * This module holds a simple buffer for reading through source text character by character while
 * keeping track of the current location.
 *
 * The module also defines a trait for other buffer implementations to follow.
 */
pub mod buffer;

/**
 * This module provides a buffer implementation that reads a buffered source stream character by
 * character while keeping track of the current location in the text.
 */
pub mod read_buffer;

pub mod layout;
