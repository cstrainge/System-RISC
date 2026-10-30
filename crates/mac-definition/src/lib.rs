//! Native definition author API. Pointer decoding and the entry/panic scaffold
//! belong here; definition bodies receive safe Rust values and borrowed ports.
#![no_std]
#[doc(hidden)]
pub use mac_abi as __abi;
pub use reflow::Reflow;
/// Shared layout message/state types for the staged control reflow migration.
pub use sr_control_reflow_handling as reflow;
mod binding;
mod families;
pub use families::{Button, ButtonKind, CaptionMode, Highlight, Progress, ProgressRange,
                   ProgressStyle, Reply, Scrollbar, Table};
/// Generate the CDEF entry and panic scaffold for the family named by `Message`.
pub use mac_definition_macros::cdef_main;
pub use toolbox_controls::Part;
#[cfg(any(target_arch = "riscv64", test))]
#[doc(hidden)]
pub mod runtime;
#[doc(hidden)]
pub use binding::{buttons_entry, progress_entry, scrollbar_entry, static_text_entry, table_entry};
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
    Reflow
    {
        state:    S,
        request:  reflow::Message,
        creation: reflow::Size
    },
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
    client: mac_traps::Client<'a>,
    reflow: Reflow
}
impl<'a> Services<'a>
{
    pub(crate) fn new(client: mac_traps::Client<'a>, reflow: Reflow) -> Self
    {
        Self { client,
               reflow }
    }

    /// Immutable choice captured from the resident control object.
    pub const fn reflow(self) -> Reflow
    {
        self.reflow
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

/// Base leaf protocol. These fixed-content controls retain their creation size
/// as preferred until their family supplies a calculated default. Constraints do
/// not lower minimum requirements or perform assignment. Relationship messages
/// require a container implementation and are explicitly refused.
pub fn leaf_reflow(request: reflow::Message, creation: reflow::Size, minimum: reflow::Size)
                   -> Result<Reply>
{
    match request
    {
        reflow::Message::AskSize(_) =>
        {
            let requirements = reflow::Requirements::new(minimum, creation);
            Ok(Reply::Measurement(reflow::Measurement { intrinsic:   requirements,
                                                        constrained: requirements }))
        }
        reflow::Message::AssignRect(_)
        | reflow::Message::SetVisibleRect(_)
        | reflow::Message::InvalidateLayout(_) => Ok(Reply::Done),
        _ => Err(Error::NotSupported)
    }
}
