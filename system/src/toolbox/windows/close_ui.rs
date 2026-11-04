//! Shared close UI suffix. Actual manager custody and affine receipts decide
//! when this runs; detached dialog owners leave Desktop before their ACK.
#![forbid(unsafe_code)]
use super::*;

impl Toolbox
{
    pub(crate) fn prepare_user_window_removal(&mut self,
                                              job:&Arc<crate::toolbox::windows::manager::operations::WindowJob>)
                                              -> Result<(), Status>
    {
        let window = self.windows.removal_window(job)?;
        // Arm before testing the actual claim; a real return can arrive between
        // refusal and the caller's park, and must not be cleared afterward.
        job.arm_removal_wait();
        // request_window_disposal arms this window's process-source definition
        // disposal and then reports busy while that disposal drains. The kernel
        // completes it unaided: control_work::pump advances every armed slot from
        // the scheduler maintenance path. A stock application reads busy as a
        // terminal failure and abandons the close it just started, so treat an
        // in-flight disposal as admission rather than refusal. Creation against
        // the window is refused while its disposal is pending, and a stuck
        // advance still reports CONTROL_DISPOSAL_RETAINED with its canonical job.
        match self.controls.request_window_disposal(window.id.0)
        {
            Ok(()) => (),
            Err(SERVICE_BUSY) if self.controls.disposal_pending_window(window.id.0) => (),
            Err(status) => return Err(status)
        }
        job.removal_admitted();
        Ok(())
    }

    /// Shared original suffix. The caller already owns a real close outcome;
    /// no WDEF author code, close admission or synthetic ACK belongs here.
    pub(super) fn remove_closed_window_content(&mut self, id: WindowId, when: u32)
                                               -> Result<(), Status>
    {
        self.controls.remove_window(id.0)?;
        let before = self.drag.feedback();
        let drag_cleanup = self.drag.forget_window(id.0, when);
        self.drag_effects(before);
        if let Err(status) = drag_cleanup
        {
            serial!("DRAG_WINDOW_CLEANUP_DEFERRED window={} status={status}", id.0);
        }
        self.scroll_boxes.remove_window(id.0);
        self.presentation.content.retain(|content| content.id != id);
        self.presentation.content_damage.retain(|(target, _)| *target != id);
        self.presentation.ordinary_underlay.forget(SceneTarget::from(id));
        self.presentation.pending_content.retain(|content| content.id != id);
        self.forget_cursor_window(id);
        self.last_action = "CLOSE";
        Ok(())
    }

    /// Root/native consumers borrow the genuine affine receipt. The returned
    /// removed Dialog owner must Drop outside Desktop before receipt.finish().
    /// Focus failure leaves that dialog's original record canonical for retry.
    pub(crate) fn cleanup_user_window_close(
        &mut self, receipt: &crate::toolbox::windows::manager::operations::CloseCleanup)
        -> Result<Option<crate::toolbox::dialogs::ReleasedAlert>, Status>
    {
        let caller = self.owner;
        self.enter(receipt.owner());
        // Logical recipient and registered storage are separate identities.
        // A deferred core close runs after the temporary core-store selection
        // has unwound; select that original System store by its full domain.
        let core = if self.core_dialogs_active
        {
            self.dialogs.owner().owner() == receipt.storage_owner()
        }
        else
        {
            self.core_dialogs.owner().owner() == receipt.storage_owner()
        };
        let result = if core
        {
            self.with_core_dialogs(|toolbox| toolbox.cleanup_window_close_in_scope(receipt))
        }
        else
        {
            self.cleanup_window_close_in_scope(receipt)
        };
        self.enter(caller);
        result
    }

    fn cleanup_window_close_in_scope(
        &mut self, receipt: &crate::toolbox::windows::manager::operations::CloseCleanup)
        -> Result<Option<crate::toolbox::dialogs::ReleasedAlert>, Status>
    {
        let result = (|| {
            if self.owner != receipt.owner()
            {
                return Err(ACCESS_DENIED);
            }
            let id = receipt.window_id();
            self.controls.prepare_remove_window(id.0)?;
            if self.dialogs.active() && self.dialogs.window == id.0
            {
                self.restore_alert_focus(self.dialogs.previous)?;
            }
            self.remove_closed_window_content(id, Self::when())?;
            if self.dialogs.active() && self.dialogs.window == id.0
            {
                self.dialogs.close_window_activity(id.0)?;
            }
            let removed = self.dialogs.take_window(id.0);
            self.presentation.redraw = true;
            Ok(removed)
        })();
        result
    }
}

pub(super) fn cleanup_status(cause: crate::toolbox::handles::cleanup::Cause) -> Status
{
    cleanup_status_of(cause)
}

fn cleanup_status_of(cause: crate::toolbox::handles::cleanup::Cause) -> Status
{
    match cause
    {
        crate::toolbox::handles::cleanup::Cause::WindowDefinitionDisposal { reply,
                                                                            .. } =>
        {
            i32::try_from(reply).unwrap_or(BAD_ABI)
        }
        #[cfg(guest)]
        crate::toolbox::handles::cleanup::Cause::ProcessSpace(cause) => cause.status(),
        #[cfg(guest)]
        crate::toolbox::handles::cleanup::Cause::ModuleMapping(cause) =>
        {
            match cause
            {
                crate::machine::paging::modules::CleanupCause::Pending { .. } => SERVICE_BUSY,
                _ => BAD_ABI
            }
        }
        #[cfg(guest)]
        crate::toolbox::handles::cleanup::Cause::NativeTls(cause) =>
        {
            match cause
            {
                crate::toolbox::native_threads::CleanupCause::Contract => BAD_ABI,
                crate::toolbox::native_threads::CleanupCause::Unavailable => NOT_SUPPORTED,
                crate::toolbox::native_threads::CleanupCause::Threads
                | crate::toolbox::native_threads::CleanupCause::Readers { .. }
                | crate::toolbox::native_threads::CleanupCause::Preparing { .. } => SERVICE_BUSY
            }
        }
        #[cfg(guest)]
        crate::toolbox::handles::cleanup::Cause::ApplicationHeap(cause) =>
        {
            match cause
            {
                crate::toolbox::application_heap::CleanupCause::Unavailable => NOT_SUPPORTED,
                crate::toolbox::application_heap::CleanupCause::Fault(_)
                | crate::toolbox::application_heap::CleanupCause::UserMapping(_) => BAD_ABI,
                crate::toolbox::application_heap::CleanupCause::Threads
                | crate::toolbox::application_heap::CleanupCause::Readers { .. }
                | crate::toolbox::application_heap::CleanupCause::Mutation
                | crate::toolbox::application_heap::CleanupCause::Remaining { .. } => SERVICE_BUSY
            }
        }
        crate::toolbox::handles::cleanup::Cause::ControlDefinitionDisposal { reply,
                                                                             .. } =>
        {
            Status::try_from(reply).unwrap_or(BAD_ABI)
        }
        crate::toolbox::handles::cleanup::Cause::ControlDisposalPending { status,
                                                                          .. } => status,
        crate::toolbox::handles::cleanup::Cause::ControlOperationPending { .. }
        | crate::toolbox::handles::cleanup::Cause::WindowControlsPending { .. }
        | crate::toolbox::handles::cleanup::Cause::WindowPaintPending { .. } => SERVICE_BUSY,
        crate::toolbox::handles::cleanup::Cause::Facility(_) => BAD_ARGUMENT,
        #[cfg(guest)]
        crate::toolbox::handles::cleanup::Cause::ThreadPreparations { .. }
        | crate::toolbox::handles::cleanup::Cause::DefinitionPending { .. }
        | crate::toolbox::handles::cleanup::Cause::NativeJoin { .. } => SERVICE_BUSY,
        crate::toolbox::handles::cleanup::Cause::PublicationPreparations { .. }
        | crate::toolbox::handles::cleanup::Cause::Alias(_)
        | crate::toolbox::handles::cleanup::Cause::ContextUnavailable { .. }
        | crate::toolbox::handles::cleanup::Cause::DriverInterfaceBusy { .. }
        | crate::toolbox::handles::cleanup::Cause::DriverDependencyBusy { .. }
        | crate::toolbox::handles::cleanup::Cause::DriverDependencyScopeActive { .. }
        | crate::toolbox::handles::cleanup::Cause::DriverReplyPending { .. }
        | crate::toolbox::handles::cleanup::Cause::DriverCancellationUncertain { .. }
        | crate::toolbox::handles::cleanup::Cause::SchedulerPending { .. }
        | crate::toolbox::handles::cleanup::Cause::DeviceStop { .. }
        | crate::toolbox::handles::cleanup::Cause::DevicePending { .. } => SERVICE_BUSY,
        crate::toolbox::handles::cleanup::Cause::DragEffectStorage { .. }
        | crate::toolbox::handles::cleanup::Cause::RetirementStorage { .. } => NO_MEMORY,
        crate::toolbox::handles::cleanup::Cause::MemoryManagerUnavailable(error)
        | crate::toolbox::handles::cleanup::Cause::FileManagerUnavailable(error)
        | crate::toolbox::handles::cleanup::Cause::RequestRegistryUnavailable(error)
        | crate::toolbox::handles::cleanup::Cause::DeviceManagerUnavailable(error)
        | crate::toolbox::handles::cleanup::Cause::SchedulerUnavailable(error)
        | crate::toolbox::handles::cleanup::Cause::ScrapManagerUnavailable(error) => error.status()
    }
}

impl Toolbox
{
    /// Zero-allocation internal join using the SAME original prepaid WIND
    /// endpoint. A callback origin is checked even when no native waiter is
    /// requested; None is not a substitute causal identity.
    pub(crate) fn request_trusted_window_close(
        &mut self, id: WindowId)
        -> Result<crate::toolbox::windows::manager::trusted::TrustedCloseHandle, Status>
    {
        self.request_trusted_window_close_for(id, None)
    }

    pub(super) fn request_trusted_window_close_for(
        &mut self, id: WindowId, waiter: Option<crate::context::ThreadId>)
        -> Result<crate::toolbox::windows::manager::trusted::TrustedCloseHandle, Status>
    {
        if let Some(handle) = self.windows.trusted_close_lookup(id)
        {
            if !self.windows.trusted_close_accessible(&handle, self.owner)
            {
                return Err(ACCESS_DENIED);
            }
            if let Ok(execution) = crate::machine::scheduler::callbacks::authenticate_current()
               && self.controls.window_disposal_waits_on(id.0, &execution)
            {
                return Err(SERVICE_BUSY);
            }
            let handle = handle.join_waiter(waiter)?;
            self.observe_dialog_window_close(super::DialogCloseObserver::Trusted(handle.clone()));
            return Ok(handle);
        }
        if !self.owns(id)
        {
            return Err(if self.windows.window(id).is_some() { ACCESS_DENIED } else { NOT_FOUND });
        }
        if let Ok(execution) = crate::machine::scheduler::callbacks::authenticate_current()
           && self.controls.window_disposal_waits_on(id.0, &execution)
        {
            return Err(SERVICE_BUSY);
        }
        let plan = self.windows.plan_trusted_close(id)?.ok_or(NOT_SUPPORTED)?;
        let handle = self.windows.trusted_close_stock(&plan, waiter)?.ok_or(BAD_ABI)?;
        self.drag.prepare_window_close(id.0)?;
        self.windows.admit_trusted_close(plan, &handle, &mut self.events, Self::when())?;
        self.observe_dialog_window_close(super::DialogCloseObserver::Trusted(handle.clone()));
        Ok(handle)
    }

    /// Real CTRL retirement admission precedes trusted WDEF DISPOSE and WIND
    /// release. Refusal retains the original ticket in canonical WM custody.
    pub(crate) fn advance_trusted_window_close(&mut self,
                                               handle: &crate::toolbox::windows::manager::trusted::TrustedCloseHandle)
                                               -> Result<(), Status>
    {
        let window = self.windows.trusted_removal_window(handle)?;
        // The original ticket/header/storage were validated above. Deferred
        // cleanup cannot recover this source through a THRD after its reclaim.
        let binding = if handle.storage_owner().kind()
                         == mac_object_table::domain::DomainKind::Process
        {
            let binding = self.processes.resource_binding(handle.owner())?;
            if binding.ledger().owner() != handle.storage_owner()
            {
                return Err(ACCESS_DENIED);
            }
            Some(binding)
        }
        else
        {
            None
        };
        let mut advance = || {
            handle.arm_wait();
            let children = self.controls.prepare_window_removal(window.id.0)?;
            let result = self.windows.finish_trusted_close(handle, &children);
            if result.is_ok()
            {
                children.commit();
            }
            result.map(|_| ()).map_err(|cause| {
                                  match cause
                                  {
                                      crate::toolbox::windows::manager::CloseError::NotFound(_) =>
                                      {
                                          NOT_FOUND
                                      }
                                      crate::toolbox::windows::manager::CloseError::Cleanup(cause) =>
                                      {
                                          cleanup_status(cause)
                                      }
                                  }
                              })
        };
        match binding
        {
            Some(binding) => crate::kernel::with_process_resources(&binding, advance)?,
            None => advance()
        }
    }
}
