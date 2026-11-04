//! Native copied SCBX request continuation. Only scalar SBOX snapshots cross a
//! wait; Surface-backed User calls remain an explicit separate producer gate.
#![forbid(unsafe_code)]
use mac_abi::scrollbox as sb;

use super::*;
use crate::toolbox::{controls::scrollbox::callbacks as staged,
                     resources::definitions::callbacks::{CompletionTarget, Pending},
                     windows::manager::paint::WindowRenderStamp};

pub(crate) struct UserOperation
{
    operation:    staged::Operation,
    prepared:     Option<staged::Prepared>,
    pending:      Option<Arc<Pending>>,
    caller:       u64,
    window:       u64,
    stamp:        Option<WindowRenderStamp>,
    desktop_wait: Option<crate::kernel::DesktopEpoch>,
    done:         bool
}
impl UserOperation
{
    pub(crate) fn begin(call: &sb::Call) -> Result<Option<Self>, WindowBeginError>
    {
        if !matches!(call.operation, sb::DRAW | sb::TRACK_BEGIN | sb::TRACK_POLL)
        {
            return Ok(None);
        }
        let epoch = crate::kernel::window_desktop_epoch();
        let captured =
            input_callbacks::with_window_toolbox(|toolbox| {
                let (recipient, storage) = toolbox.service_owners()?;
                let window =
                    toolbox.scroll_boxes.window(call.id, recipient.owner().incarnation())?;
                if window != 0 && !toolbox.owns(WindowId(window))
                {
                    return Err(if toolbox.windows.window(WindowId(window)).is_some()
                               {
                                   ACCESS_DENIED
                               }
                               else
                               {
                                   NOT_FOUND
                               });
                }
                if call.operation == sb::DRAW && call.surface.is_null() && window == 0
                {
                    return Err(BAD_ARGUMENT);
                }
                if matches!(call.operation, sb::TRACK_BEGIN | sb::TRACK_POLL)
                   && window != 0
                   && (toolbox.owner != toolbox.foreground
                       || toolbox.windows.active_window() != Some(WindowId(window)))
                {
                    // Original common request path converts to its pure cancel.
                    return Ok(None);
                }
                let fonts = crate::kernel::fonts().ok_or(NOT_SUPPORTED)?;
                let Some(plan) =
                    toolbox.scroll_boxes.plan_user_operation(&recipient, &storage, call, &fonts)?
                else
                {
                    return Ok(None);
                };
                let allocation = toolbox.processes.allocation(storage.owner().incarnation())?;
                if allocation.owner() != storage.owner()
                {
                    return Err(ACCESS_DENIED);
                }
                let stamp = if window == 0
                {
                    None
                }
                else
                {
                    Some(toolbox.windows.render_stamp(WindowId(window))?)
                };
                Ok(Some((plan, allocation, stamp, toolbox.owner)))
            }).map_err(|cause| {
                  WindowBeginError::Desktop { cause,
                                              epoch }
              })??;
        let Some((plan, allocation, stamp, caller)) = captured
        else
        {
            return Ok(None);
        };
        let window = plan.window();
        let waiter = crate::context::current_thread_context().identity().thread;
        let operation = plan.prepare(CompletionTarget::Thread(waiter), allocation)?;
        Ok(Some(Self { operation,
                       prepared: None,
                       pending: None,
                       caller,
                       window,
                       stamp,
                       desktop_wait: None,
                       done: false }))
    }

    pub(crate) fn advance(&mut self, call: &mut sb::Call) -> Result<Option<Status>, Status>
    {
        self.desktop_wait = None;
        if self.done
        {
            return Err(BAD_ARGUMENT);
        }
        if self.prepared.is_none()
        {
            drop(self.pending.take());
            match self.operation.advance()?
            {
                staged::Progress::Wait(pending) =>
                {
                    self.pending = Some(pending);
                    return Ok(None);
                }
                staged::Progress::Ready(prepared) => self.prepared = Some(prepared)
            }
        }
        let prepared = self.prepared.as_mut().ok_or(BAD_ABI)?;
        let epoch = crate::kernel::window_desktop_epoch();
        let result = input_callbacks::with_window_toolbox(|toolbox| {
            if toolbox.owner != self.caller
            {
                return Err(ACCESS_DENIED);
            }
            if prepared.window() != self.window
            {
                return Err(BAD_ABI);
            }
            if !prepared.matches(call)
            {
                return Err(BAD_ARGUMENT);
            }
            if let Some(stamp) = self.stamp
            {
                if !toolbox.owns(WindowId(self.window))
                   || toolbox.windows.render_stamp(WindowId(self.window))? != stamp
                {
                    return Err(SERVICE_BUSY);
                }
            }
            if matches!(call.operation, sb::TRACK_BEGIN | sb::TRACK_POLL)
               && self.window != 0
               && (toolbox.foreground != self.caller
                   || toolbox.windows.active_window() != Some(WindowId(self.window)))
            {
                return Err(OPERATION_CANCELLED);
            }
            if prepared.is_draw()
            {
                toolbox.scroll_boxes.stage_user_drawing(prepared, call)?;
                let command = toolbox.publish_scroll_box_drawing(self.window, prepared.drawing())?;
                prepared.finish_draw(call);
                Ok(Retired { _command:  command,
                             _viewport: None })
            }
            else
            {
                let viewport = toolbox.scroll_boxes.commit_user_operation(prepared, call)?;
                Ok(Retired { _command:  None,
                             _viewport: Some(viewport) })
            }
        });
        let retired = match result
        {
            Ok(result) => result?,
            Err(crate::state::AccessError::Busy) =>
            {
                self.desktop_wait = Some(epoch);
                return Ok(None);
            }
            Err(cause) => return Err(cause.status())
        };
        // Original old command/raster/source readers leave the manager first.
        drop(retired);
        drop(self.prepared.take());
        self.done = true;
        Ok(Some(OK))
    }

    pub(crate) fn wait(&self) -> Result<(), Status>
    {
        if let Some(epoch) = self.desktop_wait
        {
            return crate::machine::scheduler::callbacks::wait_desktop(epoch).map(|_| ());
        }
        let pending = self.pending.as_ref().ok_or(BAD_ABI)?;
        crate::machine::scheduler::callbacks::wait_call(pending).map(|_| ())
    }
}
struct Retired
{
    _command:  Option<RenderCommand>,
    _viewport: Option<staged::ViewportRetired>
}
