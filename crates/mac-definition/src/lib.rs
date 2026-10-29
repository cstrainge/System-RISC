//! Native definition author API. Pointer decoding and the entry/panic scaffold
//! belong here; definition bodies receive safe Rust values and borrowed ports.
#![no_std]
/// Shared layout message/state types for the staged control reflow migration.
pub use sr_control_reflow_handling as reflow;
#[doc(hidden)]
pub use mac_abi as __abi;
mod binding;
mod families;
pub use families::{Button, ButtonKind, CaptionMode, Highlight, Progress, ProgressRange,
                   ProgressStyle, Reply, Scrollbar};
/// Generate the CDEF entry and panic scaffold for the family named by `Message`.
pub use mac_definition_macros::cdef_main;
pub use toolbox_controls::Part;
#[cfg(any(target_arch = "riscv64", test))]
#[doc(hidden)]
pub mod runtime;
#[doc(hidden)]
pub use binding::{buttons_entry, progress_entry, scrollbar_entry, static_text_entry};
pub use mac_abi::{quickdraw::FontSelection, theme::Palette as Theme};
pub use mac_traps::{Error, Result};
pub use toolbox_quickdraw::{Point, Rect, SurfacePort};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Alignment
{
    Left,
    Center,
    Right
}
/// Native borrowed text state. No reserved fields, Pascal bytes or raw pointer
/// survive decoding. The source text and port cannot escape the invocation.
#[derive(Clone, Copy, Debug)]
pub struct StaticText<'a>
{
    pub bounds:    Rect,
    pub text:      &'a str,
    pub font:      FontSelection,
    pub enabled:   bool,
    pub alignment: Alignment
}
/// Exhaustive author dispatch. Unknown wire values never enter author code.
/// Adding a supported message requires every author to answer it explicitly.
pub enum Message<'a, S>
{
    Draw
    {
        state: S,
        port:  SurfacePort<'a>
    },
    HitTest
    {
        state: S,
        point: Point
    },
    CalculateRegions(S),
    Track
    {
        state:  S,
        anchor: Point,
        point:  Point
    },
    Initialize(S),
    Dispose(S),
    Validate(S),
    ChangeBounds
    {
        before: S,
        after:  S,
        damage: &'a mut [Rect; 2]
    }
}

/// Synchronous resident services available during this definition invocation.
#[derive(Clone, Copy)]
pub struct Services<'a>
{
    client: mac_traps::Client<'a>
}
impl<'a> Services<'a>
{
    pub(crate) fn new(client: mac_traps::Client<'a>) -> Self
    {
        Self { client }
    }

    pub fn theme(self) -> Result<mac_abi::theme::Palette>
    {
        self.client.theme()
    }

    pub fn font_metrics(self, font: FontSelection) -> Result<mac_abi::quickdraw::FontMetrics>
    {
        Ok(self.client.font(mac_abi::quickdraw::FONT_METRICS, font, "", 0)?.metrics)
    }
}

/// Shared platform failure path for definition families migrating their author
/// API. Authors never implement a panic handler or silent spin loop themselves.
#[macro_export]
macro_rules! definition_panic_handler {
    () => {
        #[cfg(target_arch = "riscv64")]
        #[panic_handler]
        fn __definition_panic(info: &core::panic::PanicInfo<'_>) -> !
        {
            $crate::runtime::panic(info)
        }
    };
}
