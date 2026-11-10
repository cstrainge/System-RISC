//! Icon selection for `ls`.
//!
//! The only thing `ls` knows about icons is the [`IconProvider`] trait.
//! The Finder icon utility now being designed will provide its own
//! implementation; swapping providers means changing one construction site
//! in `main`, nothing else.
//!
//! The built-in provider below embeds small hand-drawn 16x16 bitmaps. It
//! does not talk to the Resource Manager or the IconSet: those arrive with
//! the Finder utility, which will implement this same trait.

use std::path::Path;

use crate::native_fs::Metadata;

/// Palette index of the ink every mono pixel draws in.
pub const INK: u8 = 1;
/// Palette index of a folder's body.
pub const FOLDER_BODY: u8 = 2;
/// Palette index of a document page.
pub const PAGE: u8 = 3;
/// Palette index of an application's title bar.
pub const TITLE_BAR: u8 = 4;

/// One color layer of an icon: all set pixels fill with one palette color,
/// painted under the mono bitmap.
pub struct Layer
{
    pub color: u8,
    pub rows:  [u16; 16]
}

/// A 16x16 icon: one mono bitmap, plus optional color layers.
pub struct Icon
{
    pub width:  usize,
    pub height: usize,
    /// Bitmap rows, top to bottom; bit (15 - x) of `mono[y]` is pixel (x, y).
    pub mono:   [u16; 16],
    /// Color layers, painted in order under the mono bitmap.
    pub layers: &'static [Layer]
}

impl Icon
{
    /// Resolve the icon to a grid of palette indices, or `None` for a
    /// transparent pixel. In mono mode the layers are ignored and every
    /// set pixel draws in the ink color.
    pub fn pixels(&self, color_mode: bool) -> [[Option<u8>; 16]; 16]
    {
        let mut grid = [[None; 16]; 16];
        if color_mode
        {
            for layer in self.layers
            {
                for (y, row) in layer.rows.iter().enumerate().take(self.height.min(16))
                {
                    for (x, cell) in grid[y].iter_mut().enumerate().take(self.width.min(16))
                    {
                        if row & (1 << (15 - x)) != 0
                        {
                            *cell = Some(layer.color);
                        }
                    }
                }
            }
        }
        for (y, row) in self.mono.iter().enumerate().take(self.height.min(16))
        {
            for (x, cell) in grid[y].iter_mut().enumerate().take(self.width.min(16))
            {
                if row & (1 << (15 - x)) != 0
                {
                    *cell = Some(INK);
                }
            }
        }
        grid
    }

    /// A folder: tabbed top, hollow body.
    pub fn folder() -> Self
    {
        Self { width:  16,
               height: 16,
               mono:   FOLDER_MONO,
               layers: FOLDER_LAYERS }
    }

    /// A plain page, no text lines.
    pub fn document() -> Self
    {
        Self { width:  16,
               height: 16,
               mono:   PLAIN_DOC_MONO,
               layers: DOC_LAYERS }
    }

    /// A page with text lines.
    pub fn text_document() -> Self
    {
        Self { width:  16,
               height: 16,
               mono:   TEXT_DOC_MONO,
               layers: DOC_LAYERS }
    }

    /// An application window: solid title bar, center mark.
    pub fn application() -> Self
    {
        Self { width:  16,
               height: 16,
               mono:   APP_MONO,
               layers: APP_LAYERS }
    }
}

/// Parse 16 rows of 16 characters (`#` set, anything else clear) into a
/// bitmap at compile time. A row that is not exactly 16 characters fails
/// the build, so a miscounted drawing cannot slip through.
const fn bitmap(rows: [&str; 16]) -> [u16; 16]
{
    let mut out = [0u16; 16];
    let mut y = 0;
    while y < 16
    {
        let bytes = rows[y].as_bytes();
        if bytes.len() != 16
        {
            panic!("icon row must be exactly 16 characters");
        }
        let mut x = 0;
        while x < 16
        {
            if bytes[x] == b'#'
            {
                out[y] |= 1 << (15 - x);
            }
            x += 1;
        }
        y += 1;
    }
    out
}

const FOLDER_MONO: [u16; 16] =
    bitmap(["................", "..######........", "..######........", ".##############.",
            ".##############.", ".#............#.", ".#............#.", ".#............#.",
            ".#............#.", ".#............#.", ".#............#.", ".#............#.",
            ".#............#.", ".##############.", "................", "................"]);

const FOLDER_FILL: [u16; 16] =
    bitmap(["................", "................", "................", ".##############.",
            ".##############.", ".##############.", ".##############.", ".##############.",
            ".##############.", ".##############.", ".##############.", ".##############.",
            ".##############.", ".##############.", "................", "................"]);

static FOLDER_LAYERS: &[Layer] = &[Layer { color: FOLDER_BODY,
                                           rows:  FOLDER_FILL }];

const PLAIN_DOC_MONO: [u16; 16] =
    bitmap(["................", "...##########...", "...#........#...", "...#........#...",
            "...#........#...", "...#........#...", "...#........#...", "...#........#...",
            "...#........#...", "...#........#...", "...#........#...", "...#........#...",
            "...##########...", "................", "................", "................"]);

const TEXT_DOC_MONO: [u16; 16] =
    bitmap(["................", "...##########...", "...#........#...", "...#.######.#...",
            "...#.######.#...", "...#........#...", "...#.######.#...", "...#.######.#...",
            "...#........#...", "...#.######.#...", "...#.######.#...", "...#........#...",
            "...##########...", "................", "................", "................"]);

const DOC_FILL: [u16; 16] =
    bitmap(["................", "...##########...", "...##########...", "...##########...",
            "...##########...", "...##########...", "...##########...", "...##########...",
            "...##########...", "...##########...", "...##########...", "...##########...",
            "...##########...", "................", "................", "................"]);

static DOC_LAYERS: &[Layer] = &[Layer { color: PAGE,
                                        rows:  DOC_FILL }];

const APP_MONO: [u16; 16] =
    bitmap(["................", "..############..", "..############..", "..#..........#..",
            "..#..........#..", "..#....##....#..", "..#....##....#..", "..#..........#..",
            "..#..........#..", "..#..........#..", "..#..........#..", "..############..",
            "................", "................", "................", "................"]);

const APP_TITLE: [u16; 16] =
    bitmap(["................", "..############..", "..############..", "................",
            "................", "................", "................", "................",
            "................", "................", "................", "................",
            "................", "................", "................", "................"]);

static APP_LAYERS: &[Layer] = &[Layer { color: TITLE_BAR,
                                        rows:  APP_TITLE }];

/// Selects an icon for a directory entry. Implementations own every
/// decision about which icon a path gets; `ls` never inspects a path
/// itself beyond handing it over.
pub trait IconProvider
{
    fn icon_for(&self, path: &Path, md: &Metadata) -> Icon;
}

/// The built-in provider: kind first, then a small extension table.
/// The Finder icon utility will replace this with one that reads the
/// real icon resources; the trait stays the same.
pub struct BuiltinIconProvider;

impl IconProvider for BuiltinIconProvider
{
    fn icon_for(&self, path: &Path, md: &Metadata) -> Icon
    {
        if md.is_dir()
        {
            return Icon::folder();
        }
        if md.is_application()
        {
            return Icon::application();
        }
        match path.extension().and_then(|ext| ext.to_str())
        {
            Some(ext) if is_text_extension(ext) => Icon::text_document(),
            _ => Icon::document()
        }
    }
}

/// Extensions that get the text-lines document. Anything else plain gets
/// the plain page.
fn is_text_extension(ext: &str) -> bool
{
    matches!(ext.to_ascii_lowercase().as_str(),
             "txt" | "md" | "rs" | "toml" | "json" | "sh" | "c" | "h" | "py")
}
