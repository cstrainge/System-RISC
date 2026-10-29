//! Owned Control Manager objects. Tracking and pressed pixels belong to System.
use mac_abi::{controls::*,
              events::{BTN_STATE, EventCall, INPUT_STATE},
              *};

use super::{Client, Result, StandardKind, StandardState};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ControlId(pub u64);
#[derive(Clone, Copy)]
pub struct ControlRef<'a>
{
    client: Client<'a>,
    id:     ControlId
}
pub struct Control<'a>
{
    control: ControlRef<'a>
}
#[derive(Clone, Copy)]
pub struct ControlInfo
{
    pub window:    WindowId,
    pub standard:  StandardState,
    pub scrollbar: ScrollbarState,
    pub tracking:  bool,
    pub handled:   bool,
    pub action:    bool,
    pub changed:   bool,
    pub part:      super::Part
}
impl TryFrom<ControlCall> for ControlInfo
{
    type Error = super::Error;

    fn try_from(c: ControlCall) -> Result<Self>
    {
        Ok(Self { window:    WindowId(c.window),
                  standard:  c.standard,
                  scrollbar: c.scrollbar,
                  tracking:  c.flags & TRACKING != 0,
                  handled:   c.flags & HANDLED != 0,
                  action:    c.flags & ACTION != 0,
                  changed:   c.flags & CHANGED != 0,
                  part:      super::Part::from_code(c.result as i16).map_err(|_| {
                                                                        super::Error::BadAbi
                                                                    })? })
    }
}
impl<'a> super::Controls<'a>
{
    pub fn control(&self, id: ControlId) -> ControlRef<'a>
    {
        ControlRef { client: self.client,
                     id }
    }

    fn create(&self, mut call: ControlCall) -> Result<Control<'a>>
    {
        self.client.control(&mut call)?;
        Ok(Control { control: self.control(ControlId(call.id)) })
    }

    pub fn create_standard(&self, window: WindowId, kind: StandardKind, state: StandardState)
                           -> Result<Control<'a>>
    {
        self.create(ControlCall { operation: CREATE_STANDARD,
                                  window: window.0,
                                  procedure: kind as i16,
                                  standard: state,
                                  ..Default::default() })
    }

    pub fn create_scrollbar(&self, window: WindowId, state: ScrollbarState, step: i16, page: i16)
                            -> Result<Control<'a>>
    {
        self.create(ControlCall { operation: CREATE_SCROLLBAR,
                                  window: window.0,
                                  scrollbar: state,
                                  step,
                                  page,
                                  ..Default::default() })
    }
}
impl<'a> Control<'a>
{
    pub fn id(&self) -> ControlId
    {
        self.control.id
    }

    pub fn control(&self) -> ControlRef<'a>
    {
        self.control
    }

    /// Transfer lifetime to the parent window; System disposes its controls on close.
    pub fn into_id(self) -> ControlId
    {
        let id = self.id();
        core::mem::forget(self);
        id
    }
}
impl Drop for Control<'_>
{
    fn drop(&mut self)
    {
        let _ = self.control.dispose();
    }
}
impl ControlRef<'_>
{
    pub fn id(self) -> ControlId
    {
        self.id
    }

    fn call(self, mut call: ControlCall) -> Result<ControlInfo>
    {
        call.id = self.id.0;
        self.client.control(&mut call)?;
        call.try_into()
    }

    pub fn info(self) -> Result<ControlInfo>
    {
        self.call(ControlCall { operation: INFO_OBJECT,
                                ..Default::default() })
    }

    pub fn configure(self, kind: StandardKind, state: StandardState) -> Result<ControlInfo>
    {
        self.call(ControlCall { operation: CONFIGURE_OBJECT,
                                procedure: kind as i16,
                                standard: state,
                                ..Default::default() })
    }

    pub fn configure_scrollbar(self, state: ScrollbarState, step: i16, page: i16)
                               -> Result<ControlInfo>
    {
        self.call(ControlCall { operation: CONFIGURE_OBJECT,
                                scrollbar: state,
                                step,
                                page,
                                ..Default::default() })
    }

    pub fn draw(self) -> Result<()>
    {
        self.call(ControlCall { operation: DRAW_OBJECT,
                                ..Default::default() })
            .map(|_| ())
    }

    /// Include this control in a window repaint without submitting it twice
    /// when configuration already committed a changed drawing.
    pub fn update(self, kind: StandardKind, state: StandardState) -> Result<()>
    {
        if !self.configure(kind, state)?.changed
        {
            self.draw()?;
        }
        Ok(())
    }

    pub fn update_scrollbar(self, state: ScrollbarState, step: i16, page: i16) -> Result<()>
    {
        if !self.configure_scrollbar(state, step, page)?.changed
        {
            self.draw()?;
        }
        Ok(())
    }

    pub fn dispose(self) -> Result<()>
    {
        self.call(ControlCall { operation: DISPOSE_OBJECT,
                                ..Default::default() })
            .map(|_| ())
    }

    pub fn cancel(self) -> Result<()>
    {
        self.call(ControlCall { operation: CANCEL_TRACKING,
                                ..Default::default() })
            .map(|_| ())
    }

    pub fn begin_tracking(self, point: Point, ticks: u64) -> Result<ControlInfo>
    {
        self.call(ControlCall { operation: BEGIN_TRACKING,
                                point,
                                ticks,
                                ..Default::default() })
    }

    pub fn poll_tracking(self, point: Point, down: bool, ticks: u64) -> Result<ControlInfo>
    {
        self.call(ControlCall { operation: POLL_TRACKING,
                                point,
                                ticks,
                                flags: if down { BUTTON_DOWN } else { 0 },
                                ..Default::default() })
    }

    /// Track without consuming queued app events. Other apps remain runnable.
    /// The callback runs only for actions, outside the resident manager borrow.
    pub fn track(self, anchor: Point, action: impl FnMut(ControlInfo) -> Result<()>)
                 -> Result<bool>
    {
        self.track_in(anchor, 0, action)
    }

    /// Track a System-captured local point. The resident manager checks that
    /// its complete WindowId matches this control before changing any state.
    pub fn track_at(self, anchor: mac_traps::coordinates::WindowPoint,
                    action: impl FnMut(ControlInfo) -> Result<()>)
                    -> Result<bool>
    {
        self.track_in(anchor.point()?, anchor.window().0, action)
    }

    fn track_in(self, anchor: Point, window: u64,
                mut action: impl FnMut(ControlInfo) -> Result<()>)
                -> Result<bool>
    {
        let mut input = EventCall { operation: INPUT_STATE,
                                    ..Default::default() };
        self.client.event(&mut input)?;
        let mut state = self.call(ControlCall { operation: BEGIN_TRACKING,
                                                point: anchor,
                                                window,
                                                ticks: input.uptime_ticks,
                                                ..Default::default() })?;
        let handled = state.handled;
        if !handled
        {
            return Ok(false);
        }
        struct Capture<'a>(Option<ControlRef<'a>>);
        impl Drop for Capture<'_>
        {
            fn drop(&mut self)
            {
                if let Some(control) = self.0
                {
                    let _ = control.cancel();
                }
            }
        }
        let mut capture = Capture(Some(self));
        let result = (|| {
            loop
            {
                if state.action
                {
                    action(state)?;
                }
                if !state.tracking
                {
                    return Ok(handled);
                }
                // Present only changed control bounds, then sleep until motion/tick.
                self.client.wait_event(0, 1)?;
                self.client.event(&mut input)?;
                let origin = self.client
                                 .window(GET_WINDOW,
                                         WindowRequest { id:             state.window.0,
                                                         bounds:         Rect::ZERO,
                                                         title:          [0; WINDOW_TITLE_BYTES],
                                                         title_length:   0,
                                                         procedure:      0,
                                                         minimum_width:  1,
                                                         minimum_height: 1,
                                                         reserved:       0 })?
                                 .bounds;
                state =
                    self.poll_tracking(Point { h: input.event.where_.h.saturating_sub(origin.left),
                                               v: input.event.where_.v.saturating_sub(origin.top) },
                                       input.event.modifiers & BTN_STATE == 0,
                                       input.uptime_ticks)?;
            }
        })();
        let cancelled = match self.cancel()
        {
            Err(super::Error::NotFound) => Ok(()), // Callback may dispose the control.
            result => result
        };
        if cancelled.is_ok()
        {
            capture.0 = None;
        }
        result.and_then(|handled| cancelled.map(|()| handled))
    }
}
