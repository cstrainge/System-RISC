//! Shared desktop integration. Pointer feedback never invalidates scene content.
#![forbid(unsafe_code)]
use mac_abi::{drag::*,
              geometry::{Point, Rect},
              *};

use crate::toolbox::{drag::{PreparedReceiver, ReceiverError, ReceiverPlan, ReceiverRetired,
                            ReceiverTarget},
                     events::input::Input,
                     windows::{WindowId, WindowPart}};

pub(super) fn admit_receiver_plan(plan: ReceiverPlan) -> Result<PreparedReceiver, ReceiverError>
{
    use mac_object_table::{CapacityError, projected::PreparedError,
                           store::PreparedError as NativeError};
    let mut input = Some(plan.prepare()?);
    let mut receiver = loop
    {
        let admitted = crate::kernel::with_toolbox(|toolbox| {
                           toolbox.drag.admit_receiver(&mut input)
                       }).map_err(ReceiverError::Toolbox)?;
        match admitted
        {
            Ok(receiver) => break receiver,
            Err(ReceiverError::Publication(PreparedError::Projection(CapacityError::CapacityChanged))) =>
            {
                let value = input.as_mut().ok_or(ReceiverError::Binding(BAD_ABI))?;
                let mut capacity = value.prepare_capacity()?;
                value.replace_capacity(&mut capacity)?;
                drop(capacity);
            }
            Err(ReceiverError::Publication(PreparedError::Native(NativeError::CapacityChanged))) =>
            {
                // No canonical permit has been admitted yet. Refresh only the
                // metadata plan from the same original owner/thread/account.
                let plan = crate::kernel::with_toolbox(|toolbox| {
                    let value = input.as_ref().ok_or(ReceiverError::Binding(BAD_ABI))?;
                    toolbox.drag.replan_receiver(value)
                }).map_err(ReceiverError::Toolbox)??;
                let next = plan.prepare()?;
                drop(input.replace(next));
            }
            Err(cause) => return Err(cause)
        }
    };
    loop
    {
        let result = crate::kernel::with_toolbox(|toolbox| {
                         toolbox.drag.admit_receiver_projection(&mut receiver)
                     }).map_err(ReceiverError::Toolbox)?;
        match result
        {
            Ok(()) => return Ok(receiver),
            Err(ReceiverError::Publication(PreparedError::Projection(CapacityError::CapacityChanged))) =>
            {
                let mut capacity = receiver.prepare_capacity()?;
                match crate::kernel::with_toolbox(|toolbox| toolbox.drag.admit_receiver_capacity(&receiver, &mut capacity))
                    .map_err(ReceiverError::Toolbox)?
                {
                    Ok(()) | Err(ReceiverError::Publication(PreparedError::Projection(CapacityError::CapacityChanged))) => (),
                    Err(cause) => return Err(cause)
                }
                // Both a displaced array and a refused fresh array leave the
                // Desktop turn before destruction. Keep the actual permit.
                drop(capacity);
            }
            Err(cause) => return Err(cause)
        }
    }
}

enum DesktopReceiver<'a>
{
    Existing,
    Prepared
    {
        input:   &'a mut Option<PreparedReceiver>,
        retired: &'a mut Option<ReceiverRetired>
    }
}

/// Native drag requests retain their normal borrowed transport lifetime. Only
/// Finder's missing desktop receiver needs detached preadmission; requests are
/// never replayed after the session itself has taken effect.
pub(crate) fn native_drag(call: &mut Call, input: &[u8], output: &mut [u8]) -> Result<(), Status>
{
    let caller = crate::context::current_thread_context().identity();
    if call.operation != CREATE || caller.app != crate::context::finder_app()
    {
        return crate::kernel::with_toolbox(|toolbox| toolbox.drag_request(call, input, output))
            .map_err(|cause| cause.status())?;
    }
    let result = create_native_drag(call, input, output);
    result.map_err(|cause| {
              serial!("DRAG_RECEIVER_ADMISSION_REFUSED: {cause}");
              cause.status()
          })
}
fn create_native_drag(call: &mut Call, input: &[u8], output: &mut [u8])
                      -> Result<(), ReceiverError>
{
    let plan =
        crate::kernel::with_toolbox(|toolbox| {
            toolbox.preflight_drag_source(call).map_err(ReceiverError::Binding)?;
            let caller = crate::context::current_thread_context().identity();
            let owner = toolbox.processes.ledger(caller.app.0).map_err(ReceiverError::Binding)?;
            if toolbox.drag.receiver_binding(&owner, caller.thread, ReceiverTarget::Toolbox)?
            {
                return Ok(None);
            }
            let allocation =
                toolbox.processes.allocation(caller.app.0).map_err(ReceiverError::Binding)?;
            toolbox.drag.plan_receiver(&owner, caller.thread, &allocation).map(Some)
        }).map_err(ReceiverError::Toolbox)??;
    let mut prepared = plan.map(admit_receiver_plan).transpose()?;
    let mut retired = None;
    let publication = if prepared.is_some()
    {
        DesktopReceiver::Prepared { input:   &mut prepared,
                                    retired: &mut retired }
    }
    else
    {
        DesktopReceiver::Existing
    };
    let result = crate::kernel::with_toolbox(|toolbox| {
                     toolbox.request_drag(call, input, output, Some(publication))
                            .map_err(ReceiverError::Binding)
                 }).map_err(ReceiverError::Toolbox)?;
    if let Some(retired) = retired
    {
        retired.dispose();
    }
    drop(prepared);
    result
}

impl super::super::runtime::Toolbox
{
    pub(super) fn drag_hot(&self, point: Point) -> Option<u32>
    {
        let hit = self.windows.find_window(point);
        match hit.part
        {
            WindowPart::Content => self.drag.receiver_at(hit.window?.0),
            WindowPart::Desk => self.drag.receiver_at(0),
            _ => None
        }
    }

    pub(super) fn drag_effects(&mut self, before: Option<Rect>)
    {
        let after = self.drag.feedback();
        let highlight_changed = self.drag.take_highlight_changed();
        self.presentation.feedback_dirty |= before != after || highlight_changed;
        while let Some(notice) = self.drag.next_notice()
        {
            self.post_drag_notice(notice);
        }
    }

    pub(super) fn drag_tick(&mut self)
    {
        let before = self.drag.feedback();
        if let Err(status) = self.drag.tick(Self::when())
        {
            serial!("DRAG_INPUT_DEFERRED operation=tick status={status}; gesture state retained");
        }
        self.drag_effects(before);
    }

    pub(super) fn drag_input(&mut self, input: Input, when: u32) -> bool
    {
        if !self.drag.is_tracking()
        {
            return false;
        }
        let before = self.drag.feedback();
        let point = self.events.mouse;
        let hot = self.drag_hot(point);
        let result = match input
        {
            Input::Key { mac_code: 0x35,
                         down: true,
                         .. } => self.drag.escape(when).map(|_| ()),
            Input::Button { point,
                            down: false } =>
            {
                self.drag.release(point, self.events.modifiers, hot, when)
            }
            _ => self.drag.motion(point, self.events.modifiers, hot, when)
        };
        if let Err(status) = result
        {
            serial!("DRAG_INPUT_DEFERRED operation=input status={status}; gesture state retained");
        }
        self.drag_effects(before);
        true
    }

    pub fn drag_request(&mut self, call: &mut Call, input: &[u8], output: &mut [u8])
                        -> Result<(), Status>
    {
        self.request_drag(call, input, output, None)
    }

    fn preflight_drag_source(&self, call: &Call) -> Result<(), Status>
    {
        // NewDrag permits a real untied draft (zero). A named source must be
        // the live window owned by the caller, for native and classic requests.
        if call.operation == CREATE && call.window != 0 && !self.owns(WindowId(call.window))
        {
            return Err(if self.windows.window(WindowId(call.window)).is_some()
                       {
                           ACCESS_DENIED
                       }
                       else
                       {
                           NOT_FOUND
                       });
        }
        Ok(())
    }

    fn request_drag(&mut self, call: &mut Call, input: &[u8], output: &mut [u8],
                    publication: Option<DesktopReceiver<'_>>)
                    -> Result<(), Status>
    {
        let caller = crate::context::current_thread_context().identity();
        let owner = self.processes.ledger(caller.app.0)?;
        self.preflight_drag_source(call)?;
        if call.operation == CREATE && caller.app == crate::context::finder_app()
        {
            match publication
            {
                Some(DesktopReceiver::Existing) =>
                {
                    if !self.drag
                            .receiver_binding(&owner, caller.thread, ReceiverTarget::Toolbox)
                            .map_err(ReceiverError::status)?
                    {
                        return Err(SERVICE_BUSY);
                    }
                }
                Some(DesktopReceiver::Prepared { input,
                                                 retired }) =>
                {
                    if retired.is_some()
                    {
                        return Err(BAD_ABI);
                    }
                    *retired = self.drag
                                   .commit_receiver(input, ReceiverTarget::Toolbox)
                                   .map_err(ReceiverError::status)?;
                }
                None => self.drag.ensure_receiver(&owner, caller.thread, 0)?
            }
        }
        if matches!(call.operation, ACCEPT | READ | ACKNOWLEDGE | ABANDON | CANCEL_RECEIVE)
           && call.receiver == 0
        {
            call.receiver = self.drag.receiver_at(call.window).ok_or(NOT_FOUND)?;
        }
        let source_read_only = if call.operation == FILE_ITEMS
                                  || call.operation == OFFER && call.kind == *b"fref"
        {
            crate::kernel::with_files(|files| {
                crate::toolbox::drag::source_read_only(files, call, input)
            }).map_err(|e| e.status())??
        }
        else
        {
            false
        };
        if call.operation == BEGIN
        {
            call.flags = u32::from(self.events.modifiers);
        }
        let before = self.drag.feedback();
        let result =
            self.drag.request(&owner, caller.thread, call, input, output, Self::when(), |call| {
                         crate::kernel::with_files(|files| {
                             crate::toolbox::drag::validate_target(files, call)
                         }).map_err(|error| error.status())?
                     });
        if result.is_ok() && source_read_only
        {
            self.drag.source_read_only(call.id);
        }
        self.drag_effects(before);
        if result.is_ok() && call.operation == BEGIN && self.drag.needs_pointer_start(call.id)
        {
            let before = self.drag.feedback();
            let point = self.events.mouse;
            let hot = self.drag_hot(point);
            let motion = self.drag.motion(point, self.events.modifiers, hot, Self::when());
            if self.events.modifiers & mac_abi::events::BTN_STATE != 0
            {
                let release = self.drag.release(point, self.events.modifiers, hot, Self::when());
                self.drag_effects(before);
                motion?;
                release?;
                return result;
            }
            self.drag_effects(before);
            motion?;
        }
        result
    }
}

impl crate::toolbox::drag::lifetime::Host for crate::toolbox::runtime::Toolbox
{
    fn release_drag(&mut self, ticket: mac_object_table::domain::ReleasedResource,
                    kind: crate::toolbox::drag::lifetime::Registration)
                    -> Result<(), crate::toolbox::handles::cleanup::Failure>
    {
        let before = self.drag.release_registered(ticket, kind, Self::when())?;
        self.drag_effects(before);
        Ok(())
    }
}
