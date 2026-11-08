
use std::str::Chars;

use crate::language::text::{ location::Location, layout::{ LineLayout, LayoutPosition } };

/**
 * A trait representing a buffer of characters with location tracking. Used by the parser to read
 * input while keeping track of the current logical text location.
 */
pub trait Buffer
{
    /**
     * Bytes consumed logically. Peeking must not advance this offset.
     */
    fn byte_offset(&self) -> usize;
    fn set_tab_width(&mut self, width: usize);
    fn layout_position(&self) -> LayoutPosition;
    fn indentation(&self) -> bool;

    /**
     * A persistent read/decoding error, distinct from normal end of input.
     */
    fn read_error(&self) -> Option<&str> { None }

    /**
     * Read the buffer's current logical location in the text.
     */
    fn location(&self) -> &Location;

    /**
     * Get a mutable reference to the buffer's current logical location in the text.
     */
    fn location_mut(&mut self) -> &mut Location;

    /**
     * Peek at the next character without advancing the buffer. We only support a single character
     * lookahead.
     *
     * Will return None if we've hit EOF.
     */
    fn peek_next(&mut self) -> Option<char>;

    /**
     * Advance the buffer and return the next character. Will return None if we've hit EOF.
     */
    fn next(&mut self) -> Option<char>;

    /**
     * Logically increment the current text location based on the next character.
     */
    fn increment_location(&mut self, next: char, tab_width: usize)
    {
        let location = &mut self.location_mut();

        // Advance the location based on the encountered character.
        match next
        {
            // Handle tab character by advancing the column to the next tab stop.
            '\t' =>
                {
                    let mut column = location.column;

                    column += tab_width - ((column - 1) % tab_width);
                    location.column = column;
                }

            // Handle newline character by advancing the line and resetting the column.
            '\n' =>
                {
                    location.line += 1;
                    location.column = 1;
                },

            // Handle any other character by just advancing the column.
            _ => location.column += 1
        }
    }
}


/**
 * String iterator based buffer. Start with a loaded string source and allow a user to parse through
 * it character by character while keeping track of the current location.
 */
pub struct SimpleBuffer<'a>
{
    /**
     * The underlying iterator to the characters of the source string.
     */
    text: Chars<'a>,

    /**
     * The current logical location in the source text.
     */
    location: Location,

    /**
     * The width of a tab character in terms of spaces.
     */
    tab_width: usize,

    /**
     * Buffered copy of the last character read from the source iterator. Peeking will populate this
     * value. Getting the next character will drain it if it's present.
     */
    current_char: Option<char>,
    byte_offset: usize,
    layout: LineLayout,
}


impl<'a> SimpleBuffer<'a>
{
    /**
     * Construct a new buffer from the given source string.
     */
    pub fn new(origin: &str, source: &'a str, tab_width: Option<usize>) -> Self
    {
        Self
            {
                text: source.chars(),
                location: Location::new(origin, 1, 1),
                tab_width: tab_width.unwrap_or(4).max(1),
                current_char: None,
                byte_offset: 0,
                layout: LineLayout::default(),
            }
    }
}


impl<'a> Buffer for SimpleBuffer<'a>
{
    fn byte_offset(&self) -> usize { self.byte_offset }
    fn set_tab_width(&mut self, width: usize) { self.tab_width = width; }
    fn layout_position(&self) -> LayoutPosition { self.layout.position(self.tab_width) }
    fn indentation(&self) -> bool { self.layout.indentation() }
    /**
     * Return the current logical location in the text.
     */
    fn location(&self) -> &Location
    {
        &self.location
    }

    /**
     * Get the mutable reference to the current logical location in the text.
     */
    fn location_mut(&mut self) -> &mut Location
    {
        &mut self.location
    }

    /**
     * Peek at the next character without advancing the buffer.
     */
    fn peek_next(&mut self) -> Option<char>
    {
        // Check to see if we've already buffered a character. If so, return it directly. Otherwise,
        // fetch the next character from the source iterator.
        match self.current_char
        {
            Some(_) => self.current_char,

            None =>
            {
                let next = self.text.next();

                self.current_char = next;
                next
            }
        }
    }

    /**
     * Advance the buffer and return the next character.
     */
    fn next(&mut self) -> Option<char>
    {
        // Take the buffered character if it exists, otherwise fetch the next character from the
        // source iterator.
        let next = self.current_char.take().or_else(|| self.text.next());

        // If we hadn't hit EOF yet then increment the logical location based on the next character.
        if let Some(next) = next
        {
            self.layout.advance(next);
            self.byte_offset += next.len_utf8();
            self.increment_location(next, self.tab_width);
        }

        next
    }
}
