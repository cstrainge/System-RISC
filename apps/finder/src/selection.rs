//! Fixed, allocation-free sets over a folder snapshot. IDs remain in the cache;
//! no set survives replacement of that snapshot without identity reconciliation.
#![forbid(unsafe_code)]
pub const CAPACITY: usize = 1024;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection([u64; CAPACITY / 64]);
impl Default for Selection
{
    fn default() -> Self
    {
        Self::EMPTY
    }
}
impl Selection
{
    pub const EMPTY: Self = Self([0; CAPACITY / 64]);

    pub fn contains(self, index: usize) -> bool
    {
        index < CAPACITY && self.0[index / 64] & (1 << (index % 64)) != 0
    }

    pub fn set(&mut self, index: usize, selected: bool)
    {
        assert!(index < CAPACITY);
        let bit = 1 << (index % 64);
        if selected
        {
            self.0[index / 64] |= bit;
        }
        else
        {
            self.0[index / 64] &= !bit;
        }
    }

    pub fn singleton(index: usize) -> Self
    {
        let mut set = Self::EMPTY;
        set.set(index, true);
        set
    }

    pub fn toggle(&mut self, index: usize)
    {
        self.set(index, !self.contains(index));
    }

    pub fn all(count: usize) -> Self
    {
        let mut set = Self::EMPTY;
        for index in 0..count
        {
            set.set(index, true);
        }
        set
    }

    pub fn is_empty(self) -> bool
    {
        self.0.iter().all(|word| *word == 0)
    }

    pub fn len(self) -> usize
    {
        self.0.iter().map(|word| word.count_ones() as usize).sum()
    }

    pub fn difference(self, other: Self) -> Self
    {
        Self(core::array::from_fn(|i| self.0[i] ^ other.0[i]))
    }

    pub fn iter(self) -> impl Iterator<Item = usize>
    {
        self.0.into_iter().enumerate().flat_map(|(word_index, mut word)| {
                                          core::iter::from_fn(move || {
                                              if word == 0
                                              {
                                                  return None;
                                              }
                                              let bit = word.trailing_zeros() as usize;
                                              word &= word - 1;
                                              Some(word_index * 64 + bit)
                                          })
                                      })
    }
}
