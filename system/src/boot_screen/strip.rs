//! Boot-owned extension outcomes. Artwork is rasterized/admitted before module
//! entry; painting copies one retained 32px cell and never resolves resources.
//! The first-row 8px margin and 40px pitch follow the retained original 7.6.1
//! startup capture documented by the canonical extension-startup tests. Row
//! wrapping and the failure-X styling remain native behavior, not a pixel-match
//! claim for an original failed-extension capture.
#![forbid(unsafe_code)]

use alloc::{boxed::Box, string::String, sync::Arc, vec::Vec};
use core::fmt;

use mac_abi::{Status, *};

use crate::toolbox::quickdraw::{Canvas,
                                icons::{IconError, IconFamily, IconFlags}};

const ICON: usize = 32;
const PITCH: usize = 40;
const MARGIN: usize = 8;
const TRANSPARENT: u32 = 0xff000000;

fn theme_status(cause: Status) -> mac_formats::Error
{
    match cause
    {
        NO_MEMORY =>
        {
            mac_formats::Error::AllocationFailed("System standard extension icon bundle lookup \
                                                  could not reserve memory; free System memory \
                                                  and restart")
        }
        NOT_FOUND =>
        {
            mac_formats::Error::Missing("System standard extension icon is unavailable; rebuild \
                                         the System theme bundle")
        }
        NOT_SUPPORTED | TRANSLATION_REQUIRED =>
        {
            mac_formats::Error::TranslationRequired("System standard extension icon uses an \
                                                     unsupported format; rebuild the System theme \
                                                     bundle")
        }
        _ =>
        {
            mac_formats::Error::Malformed("System standard extension icon bundle is invalid; \
                                           rebuild the System theme bundle")
        }
    }
}
fn theme_icon(cause: IconError) -> mac_formats::Error
{
    match cause
    {
        IconError::Allocation =>
        {
            mac_formats::Error::AllocationFailed("System standard extension icon family could not \
                                                  reserve memory; free System memory and restart")
        }
        IconError::TooLarge =>
        {
            mac_formats::Error::BudgetExceeded("System standard extension icon family exceeds its \
                                                supported size; rebuild the System theme bundle")
        }
        IconError::NotFound =>
        {
            mac_formats::Error::Missing("System standard extension icon family is missing; \
                                         rebuild the System theme bundle")
        }
        IconError::Unsupported =>
        {
            mac_formats::Error::TranslationRequired("System standard extension icon family uses \
                                                     an unsupported format; rebuild the System \
                                                     theme bundle")
        }
        IconError::Truncated | IconError::InvalidData | IconError::MissingMask =>
        {
            mac_formats::Error::Malformed("System standard extension icon family is invalid or \
                                           incomplete; rebuild the System theme bundle")
        }
    }
}
fn theme_strip(cause: Error) -> mac_formats::Error
{
    match cause
    {
        Error::Memory =>
        {
            mac_formats::Error::AllocationFailed("System extension startup strip could not \
                                                  reserve retained artwork storage; free System \
                                                  memory and restart")
        }
        Error::Artwork(status) => theme_status(status),
        Error::Frame =>
        {
            mac_formats::Error::Malformed("System extension startup strip has invalid display \
                                           dimensions; rebuild System with valid display \
                                           dimensions")
        }
        Error::Space =>
        {
            mac_formats::Error::BudgetExceeded("System extension startup strip has no visible \
                                                cell below the welcome panel; use a larger \
                                                display or disable unneeded extensions")
        }
        Error::PaintPending =>
        {
            mac_formats::Error::Malformed("System extension startup strip already has an \
                                           unpainted outcome during admission; rebuild System")
        }
    }
}

/// The native BootScreen begin path and canonical host allocation probes use
/// this same admission. Preserve allocation, missing, malformed, unsupported and
/// budget causes before any framebuffer or module callback can be borrowed.
pub fn prepare_theme(resources: &[mac_formats::resource::Resource], width: usize, height: usize)
                     -> mac_formats::Result<Strip>
{
    let id = crate::toolbox::quickdraw::icons_service::file_id(resources, *b"MACS", *b"INIT")
        .map_err(theme_status)?;
    let family = IconFamily::from_lookup(id, |kind, id| {
                     resources.iter()
                              .find(|resource| resource.kind == kind && resource.id == id)
                              .map(|resource| resource.data.as_slice())
                 }).map_err(theme_icon)?;
    Strip::new(width, height, Bitmap::prepare(&family).map_err(theme_strip)?).map_err(theme_strip)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error
{
    Frame,
    Space,
    Memory,
    PaintPending,
    Artwork(Status)
}
impl fmt::Display for Error
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        f.write_str(match self
                    {
                        Self::Frame =>
                        {
                            "System extension startup strip has an invalid framebuffer; rebuild \
                             with valid display dimensions"
                        }
                        Self::Space =>
                        {
                            "System extension startup strip has no visible icon cell left below \
                             the welcome panel; use a larger display or disable unneeded startup \
                             extensions"
                        }
                        Self::Memory =>
                        {
                            "System extension startup strip cannot admit retained artwork/outcome \
                             storage; free System memory or disable unneeded startup extensions \
                             and restart"
                        }
                        Self::PaintPending =>
                        {
                            "System extension startup strip still owns an unpainted outcome; \
                             present that exact cell before starting another extension"
                        }
                        Self::Artwork(_) =>
                        {
                            "System extension startup strip could not admit package artwork; \
                             rebuild the icon family or remove its invalid artwork"
                        }
                    })
    }
}

/// Fully admitted pixels, including transparency. This never borrows a package
/// or live resource generation, and paint cannot fail a late image lookup.
pub struct Bitmap
{
    pixels: [u32; ICON * ICON]
}
impl Bitmap
{
    pub fn prepare(family: &IconFamily) -> Result<Self, Error>
    {
        let mut bitmap = Self { pixels: [TRANSPARENT; ICON * ICON] };
        family.draw(&mut Canvas::new(&mut bitmap.pixels, ICON, ICON),
                    Rect::new(0, 0, ICON as i16, ICON as i16),
                    IconFlags::default())
              .map_err(|cause| {
                  Error::Artwork(crate::toolbox::quickdraw::icons_service::status(cause))
              })?;
        Ok(bitmap)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal
{
    Initialization(Status),
    Contract,
    Location,
    Dependency,
    Device,
    Cycle,
    Artwork(Status),
    Interrupted
}
impl Refusal
{
    pub fn cause(self) -> &'static str
    {
        match self
        {
            Self::Initialization(NOT_SUPPORTED) =>
            {
                "a required channel, protocol or System service is unsupported"
            }
            Self::Initialization(NOT_FOUND) =>
            {
                "a required endpoint, package or service was not found"
            }
            Self::Initialization(NO_MEMORY) => "required module storage could not be admitted",
            Self::Initialization(SERVICE_BUSY) =>
            {
                "a required service is busy or its dependency is still active"
            }
            Self::Initialization(ACCESS_DENIED) =>
            {
                "the declared module authority does not permit its requested service"
            }
            Self::Initialization(BAD_ABI | BAD_ARGUMENT) =>
            {
                "the module entry or service contract is incompatible or invalid"
            }
            Self::Initialization(IO_ERROR) => "required endpoint communication failed",
            Self::Initialization(ROLLBACK_FAILED) =>
            {
                "initialization cleanup is quarantined and its protocol effects remain uncertain"
            }
            Self::Initialization(_) =>
            {
                "the provider refused initialization; no successful activation was published"
            }
            Self::Contract =>
            {
                "the System extension code or metadata contract is missing or invalid"
            }
            Self::Location => "the package is installed in the wrong class directory",
            Self::Dependency => "a required dependency was not admitted",
            Self::Device => "the declared required hardware or virtual endpoint is absent",
            Self::Cycle => "declared dependencies are missing, incompatible or cyclic",
            Self::Artwork(_) =>
            {
                "the package's declared icon family is invalid or cannot be admitted"
            }
            Self::Interrupted =>
            {
                "the initialization call did not finish with a confirmed activation"
            }
        }
    }

    pub fn action(self) -> &'static str
    {
        match self
        {
            Self::Initialization(ROLLBACK_FAILED) | Self::Interrupted =>
            {
                "restart System with the extension disabled; inspect its retained failure before \
                 retrying"
            }
            Self::Initialization(NO_MEMORY) =>
            {
                "free System memory or disable unneeded extensions, then restart"
            }
            Self::Initialization(NOT_SUPPORTED | NOT_FOUND) | Self::Device =>
            {
                "enable compatible required QEMU facilities or install the declared service, then \
                 restart; otherwise disable the extension"
            }
            Self::Initialization(SERVICE_BUSY) =>
            {
                "wait for the required service to become available, then restart or disable the \
                 extension"
            }
            Self::Location =>
            {
                "install the INIT package in the blessed System Folder/Extensions directory and \
                 restart"
            }
            Self::Dependency | Self::Cycle =>
            {
                "install compatible dependency packages and fix the declared dependency graph, \
                 then restart"
            }
            Self::Artwork(_) =>
            {
                "rebuild the package's PNG-backed icon family, or omit artwork to use the System \
                 theme fallback, then restart"
            }
            _ =>
            {
                "rebuild the extension and its matching System services, then restart or disable \
                 the extension"
            }
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome
{
    Loaded,
    Pending,
    Refused(Refusal)
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source
{
    Package,
    Theme
}

#[derive(Clone, Copy, Debug)]
pub struct Layout
{
    width:       usize,
    height:      usize,
    columns:     usize,
    minimum_top: usize
}
impl Layout
{
    pub fn new(width: usize, height: usize) -> Result<Self, Error>
    {
        if width > i16::MAX as usize
           || height > i16::MAX as usize
           || width < ICON + 2 * MARGIN
           || height < ICON + MARGIN
        {
            return Err(Error::Frame);
        }
        let panel = mac_boot_screen::PanelLayout::new(width as i16, height as i16).bounds;
        let columns = (width - 2 * MARGIN + (PITCH - ICON)) / PITCH;
        Ok(Self { width,
                  height,
                  columns,
                  minimum_top: panel.bottom.max(0) as usize + MARGIN })
    }

    pub fn bounds(self, index: usize) -> Result<Rect, Error>
    {
        let row = index / self.columns;
        let rise = row.checked_mul(PITCH)
                      .and_then(|value| value.checked_add(ICON + MARGIN))
                      .ok_or(Error::Space)?;
        let top = self.height.checked_sub(rise).ok_or(Error::Space)?;
        if top < self.minimum_top
        {
            return Err(Error::Space);
        }
        let left = MARGIN + (index % self.columns) * PITCH;
        Ok(Rect::new(left as i16, top as i16, (left + ICON) as i16, (top + ICON) as i16))
    }
}
enum Artwork
{
    Package(Box<Bitmap>),
    Theme(Arc<Bitmap>)
}
impl Artwork
{
    fn bitmap(&self) -> &Bitmap
    {
        match self
        {
            Self::Package(bitmap) => bitmap,
            Self::Theme(bitmap) => bitmap
        }
    }

    fn source(&self) -> Source
    {
        match self
        {
            Self::Package(_) => Source::Package,
            Self::Theme(_) => Source::Theme
        }
    }
}
struct Entry
{
    name:    String,
    bounds:  Rect,
    artwork: Artwork,
    outcome: Outcome
}
pub struct View<'a>
{
    pub name:    &'a str,
    pub bounds:  Rect,
    pub source:  Source,
    pub outcome: Outcome
}

/// Records contain no object capabilities. Their ordinal is only a presentation
/// position in already-resolved activation order, never module authority.
pub struct Strip
{
    layout:   Layout,
    fallback: Arc<Bitmap>,
    entries:  Vec<Entry>,
    pending:  Option<usize>
}
impl Strip
{
    pub fn new(width: usize, height: usize, fallback: Bitmap) -> Result<Self, Error>
    {
        Ok(Self { layout:   Layout::new(width, height)?,
                  fallback: Arc::try_new(fallback).map_err(|_| Error::Memory)?,
                  entries:  Vec::new(),
                  pending:  None })
    }

    pub fn len(&self) -> usize
    {
        self.entries.len()
    }

    pub fn view(&self, index: usize) -> Option<View<'_>>
    {
        let entry = self.entries.get(index)?;
        Some(View { name:    &entry.name,
                    bounds:  entry.bounds,
                    source:  entry.artwork.source(),
                    outcome: entry.outcome })
    }

    /// All allocations finish before the callback. No framebuffer borrow,
    /// resource/manager guard or mutable icon data crosses the foreign entry.
    /// Abandoning the call retains a refused outcome, never invented success.
    pub fn attempt<T>(&mut self, name: &str, artwork: Option<Bitmap>, refusal: Option<Refusal>,
                      call: impl FnOnce() -> Result<T, Status>)
                      -> Result<Result<T, Status>, Error>
    {
        if self.pending.is_some()
        {
            return Err(Error::PaintPending);
        }
        let bounds = self.layout.bounds(self.entries.len())?;
        let mut label = String::new();
        label.try_reserve_exact(name.len()).map_err(|_| Error::Memory)?;
        label.push_str(name);
        self.entries.try_reserve(1).map_err(|_| Error::Memory)?;
        let artwork = match artwork
        {
            Some(bitmap) => Artwork::Package(Box::try_new(bitmap).map_err(|_| Error::Memory)?),
            None => Artwork::Theme(self.fallback.clone())
        };
        let index = self.entries.len();
        self.entries.push(Entry { name: label,
                                  bounds,
                                  artwork,
                                  outcome: Outcome::Refused(Refusal::Interrupted) });
        self.pending = Some(index);
        let result = call();
        self.entries[index].outcome = match result.as_ref()
        {
            Ok(_) => Outcome::Loaded,
            Err(status) => Outcome::Refused(refusal.unwrap_or(Refusal::Initialization(*status)))
        };
        Ok(result)
    }

    pub(super) fn defer(&mut self, index: usize)
    {
        if let Some(entry) = self.entries.get_mut(index)
        {
            entry.outcome = Outcome::Pending;
            self.pending = Some(index);
        }
    }

    /// A failed framebuffer validation retains the exact pending outcome for
    /// retry. Repeated presentation without a change returns None immediately.
    pub fn paint_pending(&mut self, pixels: &mut [u32]) -> Result<Option<Rect>, Error>
    {
        let Some(index) = self.pending
        else
        {
            return Ok(None);
        };
        if self.layout.width.checked_mul(self.layout.height) != Some(pixels.len())
        {
            return Err(Error::Frame);
        }
        let entry = &self.entries[index];
        let bitmap = entry.artwork.bitmap();
        let refused = matches!(entry.outcome, Outcome::Refused(_));
        let left = entry.bounds.left as usize;
        let top = entry.bounds.top as usize;
        for y in 0..ICON
        {
            let target = &mut pixels
                [(top + y) * self.layout.width + left..(top + y) * self.layout.width + left + ICON];
            for (x, pixel) in target.iter_mut().enumerate()
            {
                let color = bitmap.pixels[y * ICON + x];
                if color != TRANSPARENT
                {
                    *pixel = color;
                }
                if refused && (2..ICON - 2).contains(&x) && (2..ICON - 2).contains(&y)
                {
                    // A narrow black X with a white halo remains legible on
                    // color and monochrome artwork, entirely inside this cell.
                    let distance = x.abs_diff(y).min(x.abs_diff(ICON - 1 - y));
                    if distance <= 2
                    {
                        *pixel = if distance <= 1 { 0 } else { 0xffffff };
                    }
                }
            }
        }
        self.pending = None;
        Ok(Some(entry.bounds))
    }
}
