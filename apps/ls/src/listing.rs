//! Directory listing: reading, sorting, recursion, and output.
//!
//! One entry per line in v1: there is no column layout because std alone
//! cannot ask the Terminal how wide it is, and an icon already makes each
//! entry two cells wide and one row tall. `-1` is accepted and is
//! currently a no-op for the same reason.

use std::{ffi::OsString,
          io::{self, Write},
          path::{Path, PathBuf},
          time::SystemTime};

use crate::{args::{Options, Sort},
            format,
            icons::IconProvider,
            native_fs::{self as fs, Metadata},
            sixel};

/// Number of terminal cells a 16-pixel icon spans. Assumes the Terminal's
/// space glyph is 8 pixels wide, so 16 pixels is two cells. Unverified:
/// the Terminal measures its cell from font family 4 at runtime, so
/// confirm on the guest; if the space glyph is wider, icons and names
/// will overlap.
const ICON_CELLS: usize = 2;

struct Entry
{
    path: PathBuf,
    name: OsString,
    md:   Metadata
}

/// The shared listing state: options, the icon provider, and the two
/// output streams. Every listing helper is a method on this so the call
/// signatures stay short.
struct Lister<'a>
{
    opts:     &'a Options,
    provider: &'a dyn IconProvider,
    out:      &'a mut dyn Write,
    err:      &'a mut dyn Write,
    icons:    bool
}

/// List the operands. Returns the exit code: 0, or 1 if any path failed
/// to list (other paths are still attempted). Output errors abort with 1.
pub fn run(opts: &Options, provider: &dyn IconProvider, out: &mut dyn Write, err: &mut dyn Write,
           tty: bool)
           -> i32
{
    let mut lister = Lister { opts,
                              provider,
                              out,
                              err,
                              icons: opts.icons && tty };
    let paths = operands(opts);
    if opts.directory
    {
        return lister.list_as_entries(&paths);
    }
    let mut files = Vec::new();
    let mut dirs = Vec::new();
    let mut status = 0;
    for path in &paths
    {
        match fs::metadata(path)
        {
            Ok(md) if md.is_dir() => dirs.push(*path),
            Ok(md) =>
            {
                files.push(Entry { name: file_name(path),
                                   path: (*path).to_path_buf(),
                                   md })
            }
            Err(e) =>
            {
                let _ = writeln!(lister.err, "ls: {}: {e}", path.display());
                status = 1;
            }
        }
    }
    lister.finish(files, dirs) | status
}

impl<'a> Lister<'a>
{
    /// List the collected files, then the directories. Returns the exit
    /// code.
    fn finish(&mut self, mut files: Vec<Entry>, mut dirs: Vec<&Path>) -> i32
    {
        let opts = self.opts;
        sort_entries(&mut files, opts);
        let mut status = 0;
        let mut first = true;
        if !files.is_empty()
        {
            if self.print_entries(&files).is_err()
            {
                return 1;
            }
            first = false;
        }
        sort_paths(&mut dirs, opts);
        let dir_count = dirs.len();
        for dir in dirs
        {
            let header = opts.recursive || dir_count > 1 || !files.is_empty();
            match self.list_dir(dir, header, &mut first)
            {
                Ok(section) => status |= section,
                Err(_) => return 1
            }
        }
        status
    }

    /// `-d`: list the operands themselves, directories included.
    fn list_as_entries(&mut self, paths: &[&Path]) -> i32
    {
        let mut status = 0;
        let mut entries = Vec::new();
        for path in paths
        {
            match fs::metadata(path)
            {
                Ok(md) =>
                {
                    entries.push(Entry { name: file_name(path),
                                         path: (*path).to_path_buf(),
                                         md })
                }
                Err(e) =>
                {
                    let _ = writeln!(self.err, "ls: {}: {e}", path.display());
                    status = 1;
                }
            }
        }
        sort_entries(&mut entries, self.opts);
        if self.print_entries(&entries).is_err()
        {
            return 1;
        }
        status
    }

    /// List one directory. Returns its status contribution, or Err if
    /// output itself failed (the caller aborts). `header` prints `dir:`
    /// first; `first` tracks whether anything has been printed yet, for
    /// the blank line between sections.
    fn list_dir(&mut self, dir: &Path, header: bool, first: &mut bool) -> io::Result<i32>
    {
        let (entries, mut status) = match self.read_entries(dir)
        {
            Ok(pair) => pair,
            Err(e) =>
            {
                let _ = writeln!(self.err, "ls: {}: {e}", dir.display());
                return Ok(1);
            }
        };
        if header
        {
            if !*first
            {
                writeln!(self.out)?;
            }
            writeln!(self.out, "{}:", dir.display())?;
        }
        *first = false;
        self.print_entries(&entries)?;
        if self.opts.recursive
        {
            for entry in &entries
            {
                if !entry.md.is_dir() || entry.name == "." || entry.name == ".."
                {
                    continue;
                }
                status |= self.list_dir(&entry.path, true, first)?;
            }
        }
        Ok(status)
    }

    /// Read a directory's entries, sorted. With `-a`, `.` and `..` lead.
    /// Returns the entries and a status contribution (1 if any single
    /// entry failed to stat; the rest are still listed).
    fn read_entries(&mut self, dir: &Path) -> io::Result<(Vec<Entry>, i32)>
    {
        let mut entries = Vec::new();
        let mut status = 0;
        if self.opts.all
        {
            if let Ok(md) = fs::metadata(dir)
            {
                entries.push(Entry { path: dir.to_path_buf(),
                                     name: OsString::from("."),
                                     md });
            }
            if let Some(parent) = dir.parent()
            {
                if let Ok(md) = fs::metadata(parent)
                {
                    entries.push(Entry { path: parent.to_path_buf(),
                                         name: OsString::from(".."),
                                         md });
                }
            }
        }
        for entry in fs::read_dir(dir)?
        {
            let entry = entry?;
            let name = entry.file_name();
            if !self.opts.all && name.as_encoded_bytes().starts_with(b".")
            {
                continue;
            }
            match entry.metadata()
            {
                Ok(md) =>
                {
                    entries.push(Entry { path: entry.path(),
                                         name,
                                         md })
                }
                Err(e) =>
                {
                    let _ = writeln!(self.err, "ls: {}: {e}", entry.path().display());
                    status = 1;
                }
            }
        }
        sort_entries(&mut entries, self.opts);
        Ok((entries, status))
    }

    fn print_entries(&mut self, entries: &[Entry]) -> io::Result<()>
    {
        // Long format aligns the size column.
        let width = if self.opts.long
        {
            entries.iter().map(|entry| size_text(entry, self.opts).len()).max().unwrap_or(0)
        }
        else
        {
            0
        };
        for entry in entries
        {
            self.print_entry(entry, width)?;
        }
        Ok(())
    }

    fn print_entry(&mut self, entry: &Entry, size_width: usize) -> io::Result<()>
    {
        let name = entry.name.to_string_lossy();
        let mut suffix = String::new();
        if self.opts.classify
        {
            suffix.push_str(format::indicator(&entry.md));
        }
        if self.icons
        {
            let icon = self.provider.icon_for(&entry.path, &entry.md);
            self.out.write_all(&sixel::encode(&icon, self.opts.color))?;
            // The cursor does not move when the Terminal places an image:
            // it stays where the sixel sequence started, so step right
            // past the icon and print the name on the same row. No
            // cursor-up: the icon is exactly one row tall.
            write!(self.out, "\x1b[{ICON_CELLS}C")?;
        }
        if self.opts.long
        {
            writeln!(self.out,
                     "{} {:>size_width$} {} {}{}",
                     format::attributes(&entry.md),
                     size_text(entry, self.opts),
                     format::mtime(&entry.md),
                     name,
                     suffix)?;
        }
        else
        {
            writeln!(self.out, "{name}{suffix}")?;
        }
        Ok(())
    }
}

fn operands(opts: &Options) -> Vec<&Path>
{
    if opts.paths.is_empty()
    {
        vec![Path::new(".")]
    }
    else
    {
        opts.paths.iter().map(Path::new).collect()
    }
}

fn sort_entries(entries: &mut [Entry], opts: &Options)
{
    match opts.sort
    {
        Sort::Name =>
        {
            entries.sort_by(|a, b| a.name.as_encoded_bytes().cmp(b.name.as_encoded_bytes()));
        }
        Sort::Time =>
        {
            entries.sort_by(|a, b| {
                       mtime_of(&b.md).cmp(&mtime_of(&a.md)).then_with(|| {
                                                                a.name
                                                                 .as_encoded_bytes()
                                                                 .cmp(b.name.as_encoded_bytes())
                                                            })
                   });
        }
        Sort::Size =>
        {
            entries.sort_by(|a, b| {
                       b.md.len().cmp(&a.md.len()).then_with(|| {
                                                      a.name
                                                       .as_encoded_bytes()
                                                       .cmp(b.name.as_encoded_bytes())
                                                  })
                   });
        }
    }
    if opts.reverse
    {
        entries.reverse();
    }
}

fn sort_paths(paths: &mut [&Path], opts: &Options)
{
    match opts.sort
    {
        Sort::Name =>
        {
            paths.sort_by(|a, b| {
                     a.as_os_str().as_encoded_bytes().cmp(b.as_os_str().as_encoded_bytes())
                 });
        }
        Sort::Time =>
        {
            paths.sort_by(|a, b| {
                     mtime_of_path(b).cmp(&mtime_of_path(a)).then_with(|| {
                                                                a.as_os_str()
                                                                 .as_encoded_bytes()
                                                                 .cmp(b.as_os_str()
                                                                       .as_encoded_bytes())
                                                            })
                 });
        }
        Sort::Size =>
        {
            paths.sort_by(|a, b| {
                     size_of_path(b).cmp(&size_of_path(a)).then_with(|| {
                                                              a.as_os_str()
                                                               .as_encoded_bytes()
                                                               .cmp(b.as_os_str()
                                                                     .as_encoded_bytes())
                                                          })
                 });
        }
    }
    if opts.reverse
    {
        paths.reverse();
    }
}

fn mtime_of(md: &Metadata) -> SystemTime
{
    md.modified().unwrap_or(SystemTime::UNIX_EPOCH)
}

fn mtime_of_path(path: &Path) -> SystemTime
{
    fs::metadata(path).and_then(|md| md.modified()).unwrap_or(SystemTime::UNIX_EPOCH)
}

fn size_of_path(path: &Path) -> u64
{
    fs::metadata(path).map(|md| md.len()).unwrap_or(0)
}

fn file_name(path: &Path) -> OsString
{
    path.file_name()
        .map(|name| name.to_os_string())
        .unwrap_or_else(|| path.as_os_str().to_os_string())
}

fn size_text(entry: &Entry, opts: &Options) -> String
{
    if opts.human_readable
    {
        format::human_size(entry.md.len())
    }
    else
    {
        entry.md.len().to_string()
    }
}
