//! TE2 storage skeleton, not its public Toolbox API. Nodes are immutable and
//! snapshots share roots; only constructors can publish augmented counts.
use alloc::{string::String, sync::Arc};

use crate::policy::ModelError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HardLines
{
    Lf,
    Universal
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Summary
{
    bytes:       u64,
    hard_breaks: u64,
    begins_lf:   bool,
    ends_cr:     bool
}
impl Summary
{
    pub const fn bytes(self) -> u64
    {
        self.bytes
    }

    /// Delimiter count, not display rows. Universal mode treats CRLF as one
    /// delimiter even when the two bytes straddle chunk boundaries.
    pub const fn hard_breaks(self) -> u64
    {
        self.hard_breaks
    }

    fn leaf(text: &str, policy: HardLines) -> Self
    {
        let mut hard_breaks = 0;
        let mut previous_cr = false;
        for byte in text.bytes()
        {
            if byte == b'\n' && (policy == HardLines::Lf || !previous_cr)
            {
                hard_breaks += 1;
            }
            if byte == b'\r' && policy == HardLines::Universal
            {
                hard_breaks += 1;
            }
            previous_cr = byte == b'\r';
        }
        Self { bytes: text.len() as u64,
               hard_breaks,
               begins_lf: text.starts_with('\n'),
               ends_cr: text.ends_with('\r') }
    }

    fn join(self, right: Self, policy: HardLines) -> Result<Self, ModelError>
    {
        if self.bytes == 0
        {
            return Ok(right);
        }
        if right.bytes == 0
        {
            return Ok(self);
        }
        let hard_breaks =
            self.hard_breaks.checked_add(right.hard_breaks).ok_or(ModelError::Overflow)?
            - u64::from(policy == HardLines::Universal && self.ends_cr && right.begins_lf);
        Ok(Self { bytes: self.bytes.checked_add(right.bytes).ok_or(ModelError::Overflow)?,
                  hard_breaks,
                  begins_lf: self.begins_lf,
                  ends_cr: right.ends_cr })
    }
}
struct Node
{
    summary:  Summary,
    contents: Contents
}
enum Contents
{
    Chunk(String),
    Branch
    {
        left:  Arc<Node>,
        right: Arc<Node>
    }
}
#[derive(Clone)]
pub struct PersistentRoot
{
    root:   Option<Arc<Node>>,
    policy: HardLines
}
impl PersistentRoot
{
    /// Balanced construction only. Incremental split/join/edit/rebalance and
    /// file-extent materialization belong to the later TE2 stage.
    pub fn from_chunks(chunks: &[&str], policy: HardLines) -> Result<Self, ModelError>
    {
        Ok(Self { root: build(chunks, policy)?,
                  policy })
    }

    pub fn summary(&self) -> Summary
    {
        self.root.as_ref().map_or(Summary::default(), |n| n.summary)
    }

    pub const fn policy(&self) -> HardLines
    {
        self.policy
    }

    /// O(tree height) lookup without flattening or scanning previous chunks.
    /// The byte need not be a character boundary: callers get the containing
    /// chunk and its absolute start, never an invalid UTF-8 substring.
    pub fn chunk_at(&self, byte: u64) -> Option<(u64, &str)>
    {
        let mut node = self.root.as_deref()?;
        if byte >= node.summary.bytes
        {
            return None;
        }
        let mut start = 0;
        loop
        {
            match &node.contents
            {
                Contents::Chunk(text) => return Some((start, text)),
                Contents::Branch { left,
                                   right } =>
                {
                    if byte - start < left.summary.bytes
                    {
                        node = left;
                    }
                    else
                    {
                        start += left.summary.bytes;
                        node = right;
                    }
                }
            }
        }
    }
}
fn build(chunks: &[&str], policy: HardLines) -> Result<Option<Arc<Node>>, ModelError>
{
    if chunks.is_empty()
    {
        return Ok(None);
    }
    if chunks.len() == 1
    {
        if chunks[0].is_empty()
        {
            return Ok(None);
        }
        let mut text = String::new();
        text.try_reserve_exact(chunks[0].len()).map_err(|_| ModelError::MetadataAllocation)?;
        text.push_str(chunks[0]);
        let summary = Summary::leaf(&text, policy);
        return Arc::try_new(Node { summary, contents: Contents::Chunk(text) }).map(Some).map_err(|_| ModelError::MetadataAllocation);
    }
    let middle = chunks.len() / 2;
    let left = build(&chunks[..middle], policy)?;
    let right = build(&chunks[middle..], policy)?;
    match (left, right)
    {
        (None, other) | (other, None) => Ok(other),
        (Some(left), Some(right)) =>
        {
            let summary = left.summary.join(right.summary, policy)?;
            Arc::try_new(Node { summary, contents: Contents::Branch { left, right } }).map(Some).map_err(|_| ModelError::MetadataAllocation)
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StorageMode
{
    Owned,
    View
}
#[derive(Clone, Copy, Debug)]
pub struct StoragePolicy
{
    percent:             u8,
    owned_ceiling_bytes: u64
}
impl StoragePolicy
{
    /// Chloe's 2026-10-23 first-cut review ratified an approximately 3.2 MiB
    /// default ceiling, independent of growth in the designated control space.
    /// Both limits stay tunable; this is policy, not a control allocation cap.
    pub const RULED_DEFAULT: Self = Self { percent:             40,
                                           owned_ceiling_bytes: 32 * 1024 * 1024 / 10 };

    pub fn new(percent: u8, owned_ceiling_bytes: u64) -> Result<Self, ModelError>
    {
        if percent > 100
        {
            Err(ModelError::InvalidPercentage)
        }
        else
        {
            Ok(Self { percent,
                      owned_ceiling_bytes })
        }
    }

    pub const fn percent(self) -> u8
    {
        self.percent
    }

    pub const fn owned_ceiling_bytes(self) -> u64
    {
        self.owned_ceiling_bytes
    }

    pub fn mode(self, document_bytes: u64, designated_control_space: u64) -> StorageMode
    {
        if document_bytes < self.owned_ceiling_bytes
           && (document_bytes as u128) * 100
              < (designated_control_space as u128) * self.percent as u128
        {
            StorageMode::Owned
        }
        else
        {
            StorageMode::View
        }
    }
}
#[cfg(test)]
mod tests
{
    use super::*;
    #[test]
    fn persistent_augmented_chunks_preserve_utf8_and_cross_chunk_crlf()
    {
        let root = PersistentRoot::from_chunks(&["café\r", "", "\n🦀\r", "x\n"],
                                               HardLines::Universal).unwrap();
        let snapshot = root.clone();
        assert!(Arc::ptr_eq(root.root.as_ref().unwrap(), snapshot.root.as_ref().unwrap()));
        assert_eq!(root.summary().bytes(), "café\r\n🦀\rx\n".len() as u64);
        assert_eq!(root.summary().hard_breaks(), 3);
        assert_eq!(root.chunk_at(7), Some((6, "\n🦀\r")));
        assert_eq!(root.chunk_at(root.summary().bytes()), None);
        drop(root);
        assert_eq!(snapshot.chunk_at(1), Some((0, "café\r")));
        let lf = PersistentRoot::from_chunks(&["a\r", "\nb\r"], HardLines::Lf).unwrap();
        assert_eq!(lf.summary().hard_breaks(), 1);
        assert_eq!(PersistentRoot::from_chunks(&[], HardLines::Universal).unwrap().summary(),
                   Summary::default());
    }
    #[test]
    fn policy_threshold_is_checked_without_size_overflow()
    {
        assert_eq!(StoragePolicy::RULED_DEFAULT.mode(399, 1000), StorageMode::Owned);
        assert_eq!(StoragePolicy::RULED_DEFAULT.mode(400, 1000), StorageMode::View);
        assert_eq!(StoragePolicy::RULED_DEFAULT.mode(u64::MAX, u64::MAX), StorageMode::View);
        assert!(matches!(StoragePolicy::new(101, u64::MAX), Err(ModelError::InvalidPercentage)));
        let default = StoragePolicy::RULED_DEFAULT;
        assert_eq!(default.owned_ceiling_bytes(), 3_355_443);
        for space in [16 * 1024 * 1024, 32 * 1024 * 1024, u64::MAX]
        {
            assert_eq!(default.mode(default.owned_ceiling_bytes() - 1, space), StorageMode::Owned);
            assert_eq!(default.mode(default.owned_ceiling_bytes(), space), StorageMode::View);
        }
        assert_eq!(StoragePolicy::new(40, 8 * 1024 * 1024).unwrap()
                                                          .mode(6 * 1024 * 1024, 32 * 1024 * 1024),
                   StorageMode::Owned);
        assert_eq!(default.mode(40, 100), StorageMode::View);
        assert_eq!(StoragePolicy::new(40, 0).unwrap().mode(0, 100), StorageMode::View);
        assert!(alloc::format!("{}", ModelError::MetadataAllocation).contains("allocation \
                                                                               refused"));
    }
}
