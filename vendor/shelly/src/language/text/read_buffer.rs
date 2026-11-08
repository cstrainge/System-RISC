
use std::{ io::{ BufRead, ErrorKind }, str::from_utf8 };

use crate::language::text::{ buffer::Buffer, location::Location, layout::{ LineLayout, LayoutPosition } };

/**
 * A buffer that can read bytes from a BufRead source.
 */
pub struct ReadBuffer<'a, R: BufRead>
{
    /**
     * A buffered stream reader for reading characters from the underlying source.
     */
    reader: &'a mut R,

    /**
     * The current logical location within the text.
     */
    location: Location,

    /**
     * The width of a tab character in the input stream.
     */
    tab_width: usize,

    /**
     * The current character that has been read but not yet consumed.
     */
    current_char: Option<char>,
    exhausted: bool,
    error: Option<String>,
    byte_offset: usize,
    layout: LineLayout,
}


impl<'a, R: BufRead> ReadBuffer<'a, R>
{
    /**
     * Construct a new read buffer from the given reader.
     */
    pub fn new(origin: &str, reader: &'a mut R, tab_width: Option<usize>) -> Self
    {
        Self
            {
                reader,
                location: Location::new(origin, 1, 1),
                tab_width: tab_width.unwrap_or(4).max(1),
                current_char: None,
                exhausted: false,
                error: None,
                byte_offset: 0,
                layout: LineLayout::default(),
            }
    }

    /**
     * Read the next character from the underlying reader.
     *
     * Returns None at EOF or on error. Errors remain available through Buffer::read_error.
     */
    fn read_char(&mut self) -> Option<char>
    {
        if self.exhausted { return None; }
        let mut buffer = [0; 4];

        // Read only as far as the next character, even across buffer boundaries.
        for length in 1..=buffer.len()
        {
            if let Err(error) = self.reader.read_exact(&mut buffer[length - 1..length])
            {
                self.exhausted = true;
                if error.kind() != ErrorKind::UnexpectedEof || length != 1
                {
                    self.error = Some(format!("Failed to read UTF-8 source: {}", error));
                }
                return None;
            }

            match from_utf8(&buffer[..length])
            {
                Ok(text) => return text.chars().next(),
                Err(error) if error.error_len().is_none() => continue,
                Err(error) =>
                    {
                        self.exhausted = true;
                        self.error = Some(format!("Invalid UTF-8 source: {}", error));
                        return None;
                    }
            }
        }

        None
    }
}


impl<'a, R: BufRead> Buffer for ReadBuffer<'a, R>
{
    fn byte_offset(&self) -> usize { self.byte_offset }
    fn set_tab_width(&mut self, width: usize) { self.tab_width = width; }
    fn layout_position(&self) -> LayoutPosition { self.layout.position(self.tab_width) }
    fn indentation(&self) -> bool { self.layout.indentation() }
    fn read_error(&self) -> Option<&str> { self.error.as_deref() }

    /**
     * Read the buffer's current logical location in the text.
     */
    fn location(&self) -> &Location
    {
        &self.location
    }

    /**
     * Get a mutable reference to the buffer's current logical location in the text.
     */
    fn location_mut(&mut self) -> &mut Location
    {
        &mut self.location
    }

    /**
     * Peek at the next character without advancing the buffer. We only support a single character
     * lookahead.
     *
     * Will return None if we've hit EOF.
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
                let next = self.read_char();

                self.current_char = next;
                next
            }
        }
    }

    /**
     * Advance the buffer and return the next character. Will return None if we've hit EOF.
     */
    fn next(&mut self) -> Option<char>
    {
        // Take the buffered character if it exists, otherwise fetch the next character from the
        // reader.
        let next = self.current_char.take().or_else(|| self.read_char());

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
