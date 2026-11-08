mod storage;

use std::{ fmt::Display, io::{ self, Error, ErrorKind }, path::PathBuf, process::id,
           sync::atomic::{ AtomicU64, Ordering }, time::{ SystemTime, UNIX_EPOCH } };

use reedline::{ FileBackedHistory, History, HistoryItem, HistoryItemId, HistorySessionId,
                Reedline, Result as ReedlineResult, SearchDirection, SearchQuery };

use crate::runtime::history::storage::{ Record, synchronize };

pub const DEFAULT_LIMIT: usize = 10000;
static SESSIONS: AtomicU64 = AtomicU64::new(0);

pub struct ReplHistory
{
    memory: FileBackedHistory,
    path: Option<PathBuf>,
    limit: usize,
    pending: Vec<Record>,
    session: String,
    sequence: u64,
    max_bytes: usize,
    fail_to_memory: bool,
}

impl ReplHistory
{
    pub fn new(path: Option<PathBuf>, limit: usize) -> Self
    {
        let timestamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
        let session = SESSIONS.fetch_add(1, Ordering::Relaxed);
        Self
            {
                memory: FileBackedHistory::new(limit).expect("Validated history limit"),
                path,
                limit,
                pending: Vec::new(),
                session: format!("{}-{}-{}", timestamp, id(), session),
                sequence: 0,
                max_bytes: usize::MAX,
                fail_to_memory: true,
            }
    }
    pub(crate) fn bounded(mut self, max_bytes: usize, fail_to_memory: bool) -> Self
    {
        self.max_bytes = max_bytes;
        self.fail_to_memory = fail_to_memory;
        self
    }
}

impl History for ReplHistory
{
    fn save(&mut self, item: HistoryItem) -> ReedlineResult<HistoryItem>
    {
        if item.command_line.trim().is_empty()
        { return Ok(HistoryItem::from_command_line(item.command_line)); }
        let item = self.memory.save(item)?;
        if self.path.is_some() && item.id.is_some()
        {
            self.pending.push(Record
                {
                    id: format!("{}-{}", self.session, self.sequence),
                    command: item.command_line.clone(),
                });
            self.sequence += 1;
        }
        if self.max_bytes != usize::MAX
        {
            let commands = self.memory
                .search(SearchQuery::everything(SearchDirection::Forward, None))?;
            let mut bytes: usize = commands.iter().map(|item| item.command_line.len()).sum();
            for old in commands
            {
                if bytes <= self.max_bytes { break; }
                bytes -= old.command_line.len();
                if let Some(id) = old.id { self.memory.delete(id)?; }
            }
            let mut pending_bytes: usize = self.pending.iter().map(|item| item.command.len()).sum();
            while pending_bytes > self.max_bytes
            { pending_bytes -= self.pending.remove(0).command.len(); }
        }
        Ok(item)
    }

    fn load(&self, id: HistoryItemId) -> ReedlineResult<HistoryItem> { self.memory.load(id) }
    fn count(&self, query: SearchQuery) -> ReedlineResult<i64> { self.memory.count(query) }
    fn search(&self, query: SearchQuery) -> ReedlineResult<Vec<HistoryItem>>
    { self.memory.search(query) }
    fn update(&mut self, id: HistoryItemId, updater: &dyn Fn(HistoryItem) -> HistoryItem)
        -> ReedlineResult<()>
    { self.memory.update(id, updater) }
    fn delete(&mut self, id: HistoryItemId) -> ReedlineResult<()> { self.memory.delete(id) }
    fn clear(&mut self) -> ReedlineResult<()>
    {
        // Clearing shared persistent history needs a separate, explicit operation.
        if self.path.is_some()
        { return Err(Error::new(ErrorKind::Unsupported,
            "Clearing persistent history is not supported").into()); }
        self.memory.clear()
    }
    fn session(&self) -> Option<HistorySessionId> { None }

    fn sync(&mut self) -> io::Result<()>
    {
        let Some(path) = &self.path else { return Ok(()); };
        let records = match synchronize(path, &self.pending, self.limit, self.max_bytes)
        {
            Ok(records) => records,
            Err(error) =>
                {
                    let message = format!("{}: {}", path.display(), error);
                    // Preserve memory and the damaged/unavailable file; stop disk attempts
                    // for this session rather than repeatedly disrupting the prompt.
                    if self.fail_to_memory
                    {
                        self.path = None;
                        self.pending.clear();
                    }
                    return Err(Error::other(message));
                },
        };
        let commands = self.memory.search(SearchQuery::everything(SearchDirection::Forward, None))
            .map_err(Error::from)?;
        // Rebuild only when another session changed the visible history.
        if !commands.iter().map(|item| item.command_line.as_str())
            .eq(records.iter().map(|record| record.command.as_str()))
        {
            let mut memory = FileBackedHistory::new(self.limit).map_err(Error::from)?;
            for record in records
            {
                memory.save(HistoryItem::from_command_line(record.command))
                    .map_err(Error::from)?;
            }
            self.memory = memory;
        }
        self.pending.clear();
        Ok(())
    }
}

pub fn report_error(error: impl Display, warned: &mut bool)
{
    if !*warned
    {
        eprintln!("History unavailable; using in-memory history: {}.", error);
        *warned = true;
    }
}

pub fn sync(editor: &mut Reedline, warned: &mut bool)
{
    if let Err(error) = editor.sync_history() { report_error(error, warned); }
    if let Some(error) = editor.take_history_save_error() { report_error(error, warned); }
}
