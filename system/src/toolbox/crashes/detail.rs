//! Bounded, preformatted recovery detail. Painting never formats an error or
//! borrows the Finder coordinator. The full typed cause remains in its owner.
#![forbid(unsafe_code)]
use core::fmt::{self, Write};

use mac_abi::{BAD_ABI, Status};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct FailureDetail
{
    rows:         [[u8; 256]; 2],
    lengths:      [usize; 2],
    columns_used: [usize; 2],
    columns:      usize,
    current:      usize,
    truncated:    bool
}
impl FailureDetail
{
    /// Use the actual boot-prepared Monaco bitmap layout, outside Desktop.
    pub(crate) fn prepare(cause: &impl fmt::Display) -> Result<Self, Status>
    {
        let columns = super::prepared_layout()?.columns.clamp(4, 256);
        let mut detail = Self { rows: [[0; 256]; 2],
                                lengths: [0; 2],
                                columns_used: [0; 2],
                                columns,
                                current: 0,
                                truncated: false };
        if write!(&mut detail, "{cause}").is_err()
        {
            detail.truncated = true;
        }
        if detail.truncated
        {
            detail.mark_truncated();
        }
        Ok(detail)
    }

    pub(crate) fn line(&self, row: usize) -> Result<&str, Status>
    {
        let bytes = self.rows.get(row).ok_or(BAD_ABI)?;
        let length = *self.lengths.get(row).ok_or(BAD_ABI)?;
        core::str::from_utf8(bytes.get(..length).ok_or(BAD_ABI)?).map_err(|_| BAD_ABI)
    }

    fn mark_truncated(&mut self)
    {
        let row = 1;
        while self.columns_used[row] > self.columns - 3 || self.lengths[row] > 253
        {
            let last =
                core::str::from_utf8(&self.rows[row][..self.lengths[row]]).ok().and_then(|text| {
                                                                                   text.chars()
                                                                                       .next_back()
                                                                               });
            let Some(last) = last
            else
            {
                self.lengths[row] = 0;
                self.columns_used[row] = 0;
                break;
            };
            self.lengths[row] -= last.len_utf8();
            self.columns_used[row] = self.columns_used[row].saturating_sub(1);
        }
        let end = self.lengths[row];
        self.rows[row][end..end + 3].copy_from_slice(b"...");
        self.lengths[row] += 3;
        self.columns_used[row] += 3;
    }
}
impl Write for FailureDetail
{
    fn write_str(&mut self, text: &str) -> fmt::Result
    {
        for character in text.chars()
        {
            if self.truncated
            {
                break;
            }
            let character = if character.is_control() { ' ' } else { character };
            let mut bytes = [0; 4];
            let encoded = character.encode_utf8(&mut bytes).as_bytes();
            if self.columns_used[self.current] >= self.columns
               || self.lengths[self.current] + encoded.len() > 256
            {
                if self.current == 1
                {
                    self.truncated = true;
                    break;
                }
                self.current = 1;
            }
            let start = self.lengths[self.current];
            self.rows[self.current][start..start + encoded.len()].copy_from_slice(encoded);
            self.lengths[self.current] += encoded.len();
            self.columns_used[self.current] += 1;
        }
        Ok(())
    }
}
