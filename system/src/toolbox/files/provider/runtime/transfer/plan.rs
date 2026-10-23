//! One catalog/read step at a time; no source data changes during planning.
use super::*;
impl State
{
    pub(super) fn add_node(&mut self, entry: CatalogInfo, parent: Option<usize>)
                           -> Result<usize, Status>
    {
        let root = &self.roots[self.current_root];
        let expected_parent =
            parent.map_or(root.input.expected_parent, |at| self.nodes[at].source.id);
        if parent.is_none() && entry.parent != expected_parent
        {
            return Err(SCAN_STALE);
        }
        if entry.volume != root.input.volume
           || entry.parent != expected_parent
           || entry.id <= ROOT_ID
           || !matches!(entry.kind, FILE | DIRECTORY)
           || entry.name_length == 0
           || entry.name_length as usize > entry.name.len()
           || core::str::from_utf8(&entry.name[..entry.name_length as usize]).is_err()
           || entry.kind == DIRECTORY && (entry.data_length != 0 || entry.resource_length != 0)
        {
            return Err(BAD_ABI);
        }
        if parent.is_none() && entry.id != root.input.cnid
        {
            return Err(BAD_ABI);
        }
        if !matches!(self.mode, Mode::Copy | Mode::CountTrash)
           && (parent.is_none() || !self.fast_move)
           && (entry.flags & FILE_LOCKED != 0
               || u16::from_be_bytes(entry.finder_info[8..10].try_into().unwrap())
                  & FINDER_NAME_LOCKED
                  != 0)
        {
            return Err(ACCESS_DENIED);
        }
        if entry.is_directory()
           && entry.volume == self.target.volume
           && self.ancestors.contains(&entry.id)
        {
            return Err(BAD_ARGUMENT);
        }
        let at = self.seen.binary_search(&(entry.volume, entry.id));
        if at.is_ok()
        {
            return Err(BAD_ARGUMENT);
        }
        if self.nodes.len() == MAX_NODES
        {
            return Err(NO_MEMORY);
        }
        let bytes = entry.data_length.checked_add(entry.resource_length).ok_or(BAD_ARGUMENT)?;
        let total = self.stats.total_bytes.checked_add(bytes).ok_or(BAD_ARGUMENT)?;
        self.nodes.try_reserve(1).map_err(|_| NO_MEMORY)?;
        self.seen.try_reserve(1).map_err(|_| NO_MEMORY)?;
        let name = if parent.is_none() { Some(text(entry.name())?) } else { None };
        self.seen.insert(at.unwrap_err(), (entry.volume, entry.id));
        let index = self.nodes.len();
        if let Some(parent) = parent
        {
            self.nodes[parent].children =
                self.nodes[parent].children.checked_add(1).ok_or(NO_MEMORY)?;
        }
        self.nodes.push(Node { source: entry,
                               children: 0,
                               comment: String::new(),
                               parent,
                               root: self.current_root,
                               destination: 0,
                               copied: false,
                               finalized: false,
                               removed: false });
        if let Some(name) = name
        {
            let root = &mut self.roots[self.current_root];
            root.node = Some(index);
            root.name = name;
        }
        self.stats.total_bytes = total;
        self.stats.total_items = self.nodes.len() as u32;
        self.stats.current = entry;
        self.current_node = index;
        self.stage = Stage::SourceComment;
        Ok(index)
    }

    pub(super) fn comment_ready(&mut self, bytes: Vec<u8>) -> Result<(), Status>
    {
        if bytes.len() > COMMENT_MAX_BYTES
        {
            return Err(BAD_ABI);
        }
        self.nodes[self.current_node].comment = String::from_utf8(bytes).map_err(|_| BAD_ABI)?;
        if !self.fast_move && self.nodes[self.current_node].source.is_directory()
        {
            if self.frames.len() == MAX_DEPTH
            {
                return Err(NO_MEMORY);
            }
            self.frames.try_reserve(1).map_err(|_| NO_MEMORY)?;
            self.frames.push(Frame { node:   self.current_node,
                                     cursor: DirectoryScanCursor::default() });
        }
        self.stage = if self.frames.is_empty() { Stage::NextRoot } else { Stage::SourceScan };
        Ok(())
    }

    pub(super) fn scan_ready(&mut self, call: FileScanCall) -> Result<(), Status>
    {
        let frame = self.frames.last_mut().ok_or(BAD_ABI)?;
        let parent = frame.node;
        let expected = self.nodes[parent].reference();
        if call.volume != expected.volume || call.directory != expected.id
        {
            return Err(BAD_ABI);
        }
        frame.cursor = call.cursor;
        match call.step
        {
            SCAN_PENDING => (),
            SCAN_ENTRY =>
            {
                self.account_allocation(call.data_allocated, call.resource_allocated)?;
                self.add_node(call.entry, Some(parent))?;
            }
            SCAN_DONE =>
            {
                self.frames.pop();
                self.stage =
                    if self.frames.is_empty() { Stage::NextRoot } else { Stage::SourceScan };
            }
            _ => return Err(BAD_ABI)
        }
        Ok(())
    }

    pub(super) fn account_allocation(&mut self, data: u64, resources: u64) -> Result<(), Status>
    {
        if self.mode != Mode::CountTrash
        {
            return Ok(());
        }
        self.stats.allocated_bytes = if self.stats.allocated_bytes == ALLOCATION_UNKNOWN
                                        || data == ALLOCATION_UNKNOWN
                                        || resources == ALLOCATION_UNKNOWN
        {
            ALLOCATION_UNKNOWN
        }
        else
        {
            self.stats
                .allocated_bytes
                .checked_add(data)
                .and_then(|n| n.checked_add(resources))
                .filter(|n| *n != ALLOCATION_UNKNOWN)
                .ok_or(BAD_ARGUMENT)?
        };
        Ok(())
    }

    pub(super) fn copy_name(&mut self) -> Result<(), Status>
    {
        let root = &self.roots[self.current_root];
        let source = &self.nodes[root.node.ok_or(BAD_ABI)?].source;
        let mut suffix = String::new();
        suffix.try_reserve_exact(32).map_err(|_| NO_MEMORY)?;
        use core::fmt::Write;
        if root.name_attempt == 0
        {
            suffix.push_str(" copy");
        }
        else
        {
            write!(&mut suffix, " copy {}", root.name_attempt + 1).map_err(|_| BAD_ARGUMENT)?;
        }
        let name = source.name();
        let mut end = name.len().min(255 - suffix.len());
        while !name.is_char_boundary(end)
        {
            end -= 1;
        }
        let mut candidate = String::new();
        candidate.try_reserve_exact(end + suffix.len()).map_err(|_| NO_MEMORY)?;
        candidate.push_str(&name[..end]);
        candidate.push_str(&suffix);
        let root = &mut self.roots[self.current_root];
        root.name = candidate;
        root.name_attempt =
            root.name_attempt.checked_add(1).filter(|n| *n <= 4096).ok_or(ALREADY_EXISTS)?;
        Ok(())
    }

    pub(super) fn add_stamp(&mut self, reference: Reference, modified: u64) -> Result<(), Status>
    {
        match self.stamps.binary_search_by_key(&(reference.volume, reference.id), |stamp| {
                             (stamp.reference.volume, stamp.reference.id)
                         })
        {
            Ok(_) => (),
            Err(at) =>
            {
                self.stamps.try_reserve(1).map_err(|_| NO_MEMORY)?;
                self.stamps.insert(at,
                                   Stamp { reference,
                                           modified,
                                           volume_modified: None });
            }
        }
        Ok(())
    }
}
