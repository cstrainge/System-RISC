//! The one modal diagnostic may outlive its provisional Finder. This is real
//! System UI state, never an entry in the Process directory or a resumed app.
#![forbid(unsafe_code)]
use core::sync::atomic::{AtomicBool, Ordering};

use mac_abi::{ACCESS_DENIED, BAD_ABI, NOT_FOUND, SERVICE_BUSY, Status};

use super::{FinderAdmission, ResourceBinding, Saved};
use crate::{recovery::{Activity, SystemReportLease},
            toolbox::{crashes, events::input::Input, handles::OwnerRef, runtime::Desktop}};

static ACTIVE: AtomicBool = AtomicBool::new(false);
static CHANGED: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum SystemReportActionKind
{
    Retry,
    Restart
}

/// An actual report-button invocation, not an owner/Window number supplied by
/// a caller. Its retained Session also prevents reuse before the request ends.
pub(crate) struct SystemReportAction
{
    activity:    Activity,
    beneficiary: ResourceBinding,
    kind:        SystemReportActionKind
}
impl SystemReportAction
{
    pub(crate) fn kind(&self) -> SystemReportActionKind
    {
        self.kind
    }

    pub(crate) fn beneficiary_matches(&self, binding: &ResourceBinding) -> bool
    {
        self.beneficiary.ledger().owner() == binding.ledger().owner()
        && self.beneficiary.image().map(crate::images::Binding::id)
           == binding.image().map(crate::images::Binding::id)
    }

    pub(crate) fn authorizes(&self, lease: &SystemReportLease) -> bool
    {
        lease.matches_activity(&self.activity)
    }
}

/// Admitted once before pressure. Empty native manager stores and the boxed,
/// fixed EventManager queue have genuine System backing. No Process account or
/// directory row is constructed for this permanent service context.
pub(in crate::toolbox::runtime) struct SystemReportSlot
{
    saved:   Saved,
    session: Option<ReportSession>
}

pub(in crate::toolbox::runtime) struct ReportSession
{
    activity:          Activity,
    beneficiary:       ResourceBinding,
    close_requested:   bool,
    restart_requested: bool,
    frame_tracking:    bool,
    // Work owns this same unique lease until its real handoff to the service.
    lease:             Option<SystemReportLease>
}

struct BaselineForeground<'a>
{
    desktop:            &'a mut Desktop,
    previous:           u64,
    previous_temporary: bool
}
impl Drop for BaselineForeground<'_>
{
    fn drop(&mut self)
    {
        self.desktop.foreground = self.previous;
        self.desktop.system_report_temporary = self.previous_temporary;
    }
}

struct Selection<'a>
{
    desktop:             &'a mut Desktop,
    slot:                Option<SystemReportSlot>,
    previous_owner:      u64,
    previous_foreground: u64,
    previous_temporary:  bool
}
fn swap_ui(desktop: &mut Desktop, saved: &mut Saved)
{
    core::mem::swap(&mut desktop.crash, &mut saved.crash);
    core::mem::swap(&mut desktop.dialogs, &mut saved.dialogs);
    core::mem::swap(&mut desktop.menus, &mut saved.menus);
    core::mem::swap(&mut desktop.cursors, &mut saved.cursors);
    core::mem::swap(&mut desktop.classic_font, &mut saved.classic_font);
    core::mem::swap(&mut desktop.events, &mut saved.events);
    core::mem::swap(&mut desktop.textedit, &mut saved.textedit);
}
impl Drop for Selection<'_>
{
    fn drop(&mut self)
    {
        // No lookup, allocation, callback, assertion or fallible re-entry is
        // needed to restore actual borrowed UI state. Physical input persists.
        if let Some(mut slot) = self.slot.take()
        {
            let physical = (self.desktop.events.mouse,
                            self.desktop.events.modifiers,
                            self.desktop.events.keys);
            swap_ui(self.desktop, &mut slot.saved);
            self.desktop.events.mouse = physical.0;
            self.desktop.events.modifiers = physical.1;
            self.desktop.events.keys = physical.2;
            slot.session = self.desktop.system_report_active.take();
            self.desktop.owner = self.previous_owner;
            self.desktop.foreground = self.previous_foreground;
            self.desktop.system_report_temporary = self.previous_temporary;
            self.desktop.system_report = Some(slot);
        }
    }
}

impl Desktop
{
    pub(in crate::toolbox::runtime) fn prepare_system_report_slot(&mut self, owner: &OwnerRef)
                                                                  -> Result<(), Status>
    {
        if self.system_report.is_some()
           || self.system_report_active.is_some()
           || !crate::toolbox::handles::system_owner_if_ready().is_some_and(|actual| {
                                                                   actual.owner() == owner.owner()
                                                               })
        {
            return Err(BAD_ABI);
        }
        let saved = Saved::try_placeholder(owner.clone(), self.mdef)?;
        self.system_report = Some(SystemReportSlot { saved,
                                                     session: None });
        Ok(())
    }

    /// The only recipient transition occurs after the old report's canonical
    /// close/ACK. The returned lease must survive any later role-CAS refusal.
    pub(crate) fn adopt_finder_system_report(&mut self, old: &ResourceBinding,
                                             admission: &mut FinderAdmission)
                                             -> Result<(), Status>
    {
        if self.processes.ledger(old.owner())?.owner() != old.ledger().owner()
           || self.processes.ledger(admission.id())?.owner() != admission.owner().owner()
           || admission.old() != old.owner()
           || crate::context::finder_app().0 != old.owner()
           || !admission.owner().accepting()
           || admission.system_report.is_some()
           || self.core_dialogs_active
           || self.system_report_active.is_some()
           || !self.system_report.as_ref().is_some_and(|slot| slot.session.is_none())
        {
            return Err(ACCESS_DENIED);
        }
        self.native_failure_selection(old)?;
        crate::toolbox::processes::finder::report_cleanup_wait();
        self.close_crash_report(&admission.activity())?;
        let beneficiary = self.processes.resource_binding(admission.id())?;
        let donor = if self.owner == admission.id()
        {
            &mut self.crash
        }
        else
        {
            &mut self.locals
                     .iter_mut()
                     .find(|saved| saved.id == admission.id())
                     .ok_or(NOT_FOUND)?
                     .crash
        };
        if !donor.owns_protected_stock()
           || donor.info().window.0 != 0
           || donor.controls.iter().any(Option::is_some)
        {
            return Err(BAD_ABI);
        }
        let slot = self.system_report.as_mut().ok_or(NOT_FOUND)?;
        if slot.session.is_some() || slot.saved.crash.info().report_bytes != 0
        {
            return Err(SERVICE_BUSY);
        }
        // All fallible validation is complete before the recipient changes.
        let activity = admission.activity();
        let lease = activity.retain_as_system_report(old.ledger())?;
        // Ownership is recorded before any later role commit can refuse.
        admission.system_report = Some(lease);
        core::mem::swap(donor, &mut slot.saved.crash);
        slot.saved.crash.restart_pending = true;
        slot.session = Some(ReportSession { activity,
                                            beneficiary,
                                            close_requested: false,
                                            restart_requested: false,
                                            frame_tracking: false,
                                            lease: None });
        ACTIVE.store(true, Ordering::Release);
        CHANGED.store(true, Ordering::Release);
        Ok(())
    }

    pub(crate) fn system_report_manager<'a>(&'a mut self, lease: &SystemReportLease)
                                            -> Result<&'a mut crashes::Manager, Status>
    {
        if let Some(session) = &self.system_report_active
        {
            if !lease.matches_activity(&session.activity)
            {
                return Err(ACCESS_DENIED);
            }
            return Ok(&mut self.crash);
        }
        let slot = self.system_report.as_mut().ok_or(NOT_FOUND)?;
        if !slot.session.as_ref().is_some_and(|session| lease.matches_activity(&session.activity))
        {
            return Err(ACCESS_DENIED);
        }
        Ok(&mut slot.saved.crash)
    }

    pub(crate) fn with_system_report<R>(&mut self, lease: &SystemReportLease,
                                        operation: impl FnOnce(&mut Self) -> R)
                                        -> Result<R, Status>
    {
        self.with_system_report_activity(lease.activity(), operation)
    }

    fn with_system_report_activity<R>(&mut self, activity: &Activity,
                                      operation: impl FnOnce(&mut Self) -> R)
                                      -> Result<R, Status>
    {
        let result = crate::allocator::with_system(crate::allocator::SystemClass::Ordinary, || {
            activity.run(|| self.select_system_report(activity, operation))?
        });
        if !self.system_report_temporary
        {
            let scene = core::mem::take(&mut self.system_report_present_pending);
            let cursor = core::mem::take(&mut self.system_report_cursor_pending);
            // Rendering occurs only after actual foreground state is restored,
            // never in Drop. Existing damage/early-outs remain authoritative.
            if scene
            {
                self.present();
            }
            else if cursor
            {
                self.present_cursor_overlay();
            }
        }
        result
    }

    fn select_system_report<R>(&mut self, activity: &Activity,
                               operation: impl FnOnce(&mut Self) -> R)
                               -> Result<R, Status>
    {
        if let Some(selected) = &self.system_report_active
        {
            if !selected.activity.same_session(activity)
            {
                return Err(ACCESS_DENIED);
            }
            if self.owner != selected.activity.storage_owner().owner().incarnation()
            {
                return Err(ACCESS_DENIED);
            }
            let previous = self.foreground;
            let previous_temporary = self.system_report_temporary;
            self.system_report_temporary |= previous != self.owner;
            self.foreground = self.owner;
            let restore = BaselineForeground { desktop: self,
                                               previous,
                                               previous_temporary };
            return Ok(operation(&mut *restore.desktop));
        }
        if self.core_dialogs_active || !activity.is_system_report()
        {
            return Err(ACCESS_DENIED);
        }
        let mut slot = self.system_report.take().ok_or(NOT_FOUND)?;
        if !slot.session.as_ref().is_some_and(|session| session.activity.same_session(activity))
        {
            self.system_report = Some(slot);
            return Err(ACCESS_DENIED);
        }
        let previous_owner = self.owner;
        let previous_foreground = self.foreground;
        let previous_temporary = self.system_report_temporary;
        self.system_report_temporary = true;
        let physical = (self.events.mouse, self.events.modifiers, self.events.keys);
        swap_ui(self, &mut slot.saved);
        self.events.mouse = physical.0;
        self.events.modifiers = physical.1;
        self.events.keys = physical.2;
        self.owner = activity.storage_owner().owner().incarnation();
        self.foreground = self.owner;
        self.system_report_active = slot.session.take();
        let restore = Selection { desktop: self,
                                  slot: Some(slot),
                                  previous_owner,
                                  previous_foreground,
                                  previous_temporary };
        Ok(operation(&mut *restore.desktop))
    }

    /// No payload is extracted. Retain the same affine owner before Work ends;
    /// report dismissal and its actual Window ACK now own the service lifetime.
    pub(crate) fn retain_system_report(&mut self, lease: SystemReportLease)
                                       -> Result<(), (Status, SystemReportLease)>
    {
        let session = if let Some(active) = &mut self.system_report_active
        {
            Some(active)
        }
        else
        {
            self.system_report.as_mut().and_then(|slot| slot.session.as_mut())
        };
        let Some(session) = session
        else
        {
            return Err((NOT_FOUND, lease));
        };
        if !lease.matches_activity(&session.activity) || session.lease.is_some()
        {
            return Err((ACCESS_DENIED, lease));
        }
        session.lease = Some(lease);
        CHANGED.store(true, Ordering::Release);
        Ok(())
    }

    pub(crate) fn system_report_action(&self, kind: SystemReportActionKind)
                                       -> Result<SystemReportAction, Status>
    {
        let session = self.system_report_active.as_ref().ok_or(ACCESS_DENIED)?;
        if session.close_requested
           || session.activity.closing()
           || !crate::context::recovery_activity().is_some_and(|actual| {
                                                      actual.same_session(&session.activity)
                                                  })
           || self.crash.info().window.0 == 0
           || session.activity.owner() != self.owner
           || self.owner != self.foreground
           || !self.windows.owned_by(self.crash.info().window, self.owner)
        {
            return Err(ACCESS_DENIED);
        }
        Ok(SystemReportAction { activity: session.activity.clone(),
                                beneficiary: session.beneficiary.retain(),
                                kind })
    }

    pub(crate) fn request_system_report_restart(&mut self, action: &SystemReportAction)
                                                -> Result<(), Status>
    {
        let session = self.system_report_active.as_mut().ok_or(ACCESS_DENIED)?;
        if action.kind != SystemReportActionKind::Restart
           || !session.lease.as_ref().is_some_and(|lease| action.authorizes(lease))
           || session.close_requested
        {
            return Err(ACCESS_DENIED);
        }
        session.restart_requested = true;
        CHANGED.store(true, Ordering::Release);
        Ok(())
    }

    pub(crate) fn request_system_report_dismiss(&mut self) -> Result<(), Status>
    {
        let session = self.system_report_active.as_mut().ok_or(ACCESS_DENIED)?;
        if session.lease.is_none()
        {
            // The live Work must explicitly hand off before exposing Dismiss.
            return Err(SERVICE_BUSY);
        }
        session.close_requested = true;
        session.activity.close();
        CHANGED.store(true, Ordering::Release);
        Ok(())
    }

    pub(in crate::toolbox::runtime) fn system_report_selected(&self) -> bool
    {
        self.system_report_active.is_some()
    }
}

pub(crate) fn active() -> bool
{
    ACTIVE.load(Ordering::Acquire)
}

pub(crate) fn changed()
{
    if ACTIVE.load(Ordering::Acquire)
    {
        CHANGED.store(true, Ordering::Release);
    }
}

impl Desktop
{
    pub(crate) fn system_report_closing(&self, lease: &SystemReportLease) -> Result<bool, Status>
    {
        let session =
            self.system_report_active
                .as_ref()
                .or_else(|| self.system_report.as_ref().and_then(|slot| slot.session.as_ref()))
                .ok_or(NOT_FOUND)?;
        if !lease.matches_activity(&session.activity)
        {
            return Err(ACCESS_DENIED);
        }
        Ok(session.close_requested || session.activity.closing())
    }

    pub(crate) fn system_report_for_beneficiary(&self, owner: &OwnerRef) -> Result<bool, Status>
    {
        let session =
            self.system_report_active.as_ref().or_else(|| {
                                                  self.system_report
                                                      .as_ref()
                                                      .and_then(|slot| slot.session.as_ref())
                                              });
        let Some(session) = session
        else
        {
            return Ok(false);
        };
        if session.beneficiary.ledger().owner() != owner.owner()
        {
            return Ok(false);
        }
        let current = self.processes.resource_binding(owner.owner().incarnation())?;
        if current.ledger().owner() != owner.owner()
           || current.image().map(crate::images::Binding::id)
              != session.beneficiary.image().map(crate::images::Binding::id)
        {
            return Err(ACCESS_DENIED);
        }
        Ok(true)
    }

    /// A subsequent real capture may close only this same beneficiary's report.
    /// Requesting close is not an ACK or a grant to reset the shared payload.
    pub(crate) fn request_system_report_close_for(&mut self, owner: &OwnerRef)
                                                  -> Result<bool, Status>
    {
        if !self.system_report_for_beneficiary(owner)?
        {
            return Ok(false);
        }
        if !owner.accepting()
        {
            return Err(ACCESS_DENIED);
        }
        let session =
            self.system_report_active.as_mut().or_else(|| {
                                                  self.system_report
                                                      .as_mut()
                                                      .and_then(|slot| slot.session.as_mut())
                                              });
        if let Some(session) = session
        {
            session.close_requested = true;
            session.activity.close();
            CHANGED.store(true, Ordering::Release);
            crate::toolbox::processes::finder::cleanup_progress();
            return Ok(true);
        }
        Err(NOT_FOUND)
    }

    pub(in crate::toolbox::runtime) fn route_system_report_event(&mut self, event: mac_abi::Event,
                                                                 target: u64,
                                                                 position: mac_abi::events::EventPosition)
                                                                 -> bool
    {
        if self.system_report_active.is_some()
        {
            if target == self.owner && event.window == self.crash.info().window.0
            {
                self.events.post_positioned(event, position);
                changed();
                return true;
            }
            return false;
        }
        let Some(slot) = &mut self.system_report
        else
        {
            return false;
        };
        if slot.session.as_ref().is_some_and(|session| {
                                    target == session.activity.owner()
                                    && event.window != 0
                                    && event.window == slot.saved.crash.info().window.0
                                })
        {
            slot.saved.events.post_positioned(event, position);
            changed();
            return true;
        }
        false
    }

    fn system_report_input_state(&self) -> Option<(&ReportSession, &crashes::Manager)>
    {
        if let Some(session) = &self.system_report_active
        {
            return Some((session, &self.crash));
        }
        let slot = self.system_report.as_ref()?;
        Some((slot.session.as_ref()?, &slot.saved.crash))
    }

    pub(in crate::toolbox::runtime) fn system_report_needs_hit(&self) -> bool
    {
        self.system_report_input_state().is_some_and(|(session, manager)| {
                                            !session.close_requested
                                            && !session.activity.closing()
                                            && !session.frame_tracking
                                            && manager.tracking.is_none()
                                            && manager.info().window.0 != 0
                                        })
    }

    pub(in crate::toolbox::runtime) fn sync_system_report_input(&mut self, mouse: mac_abi::Point,
                                                                modifiers: u16,
                                                                keys: mac_abi::events::KeyMap)
    {
        if self.system_report_active.is_none()
           && let Some(slot) = &mut self.system_report
        {
            slot.saved.events.mouse = mouse;
            slot.saved.events.modifiers = modifiers;
            slot.saved.events.keys = keys;
        }
    }

    pub(in crate::toolbox::runtime) fn system_report_input(&mut self, input: Input, when: u32,
                                                           hit: Option<crate::toolbox::windows::manager::Hit>)
                                                           -> Result<bool, Status>
    {
        let Some((session, manager)) = self.system_report_input_state()
        else
        {
            return Ok(false);
        };
        let report = manager.info().window;
        if session.close_requested || session.activity.closing() || report.0 == 0
        {
            return Ok(false);
        }
        // Frame capture is recorded only when THIS report's authenticated hit
        // started it. An unrelated background Command-drag cannot become ours.
        let captured = session.frame_tracking || manager.tracking.is_some();
        let targeted = match input
        {
            Input::Key { .. } => self.windows.active_window() == Some(report),
            Input::Button { down: true,
                            .. } => hit.is_some_and(|hit| hit.window == Some(report)),
            _ => captured
        };
        if !targeted || (self.windows.is_tracking() && !session.frame_tracking)
        {
            return Ok(false);
        }
        let activity = session.activity.clone();
        let started = !self.windows.is_tracking();
        let result = self.with_system_report_activity(&activity, |desktop| {
                             desktop.input_impl_with_hit(input, when, hit);
                             let tracking = desktop.windows.is_tracking();
                             if let Some(session) = &mut desktop.system_report_active
                             {
                                 session.frame_tracking =
                                     tracking && (session.frame_tracking || started);
                             }
                         });
        // A valid report target never falls through to ordinary app delivery
        // after refusal. Original canonical state remains pinned for progress.
        result?;
        Ok(true)
    }

    /// The cleanup path may select the real System context while CLOSING. It
    /// cannot reopen a report or return ownership to a stopped beneficiary.
    pub(crate) fn cancel_system_report_for_recovery(&mut self, activity: &Activity)
                                                    -> Result<(), Status>
    {
        if activity.system_report_children_closed()
           && self.system_report_active.is_none()
           && self.system_report.as_ref().is_some_and(|slot| slot.session.is_none())
        {
            // The service has already detached/destroyed its payload after the
            // genuine child ACK. Reap still waits all real Session/Subject pins.
            return Ok(());
        }
        self.with_system_report_activity(activity, |desktop| desktop.close_crash_report(activity))?
    }
}

struct RetiredReport
{
    // The actual report payload is released outside Desktop, before lease and
    // Session pins can advertise that the reusable modal stock is idle.
    _manager:  crashes::Manager,
    _replaced: Option<crashes::Manager>,
    _session:  ReportSession
}
enum Turn
{
    None,
    Retired(RetiredReport),
    Restart(Activity)
}
impl Desktop
{
    fn system_report_turn(&mut self) -> Result<Turn, Status>
    {
        let Some(session) =
            self.system_report_active.as_ref().or_else(|| {
                                                  self.system_report
                                                      .as_ref()
                                                      .and_then(|slot| slot.session.as_ref())
                                              })
        else
        {
            return Ok(Turn::None);
        };
        let activity = session.activity.clone();
        if !session.beneficiary.ledger().accepting()
           && session.lease.is_none()
           && !activity.system_report_abandoned()
        {
            crate::toolbox::processes::finder::cleanup_progress();
        }
        if session.close_requested || activity.closing()
        {
            self.with_system_report_activity(&activity, |desktop| {
                    desktop.close_crash_report(&activity)
                })??;
            if self.system_ui_baseline
            {
                if !self.system_report_active
                        .as_ref()
                        .is_some_and(|session| session.lease.is_some())
                   && !activity.system_report_abandoned()
                {
                    return Ok(Turn::None);
                }
                let replaced = self.preserve_system_report_metadata();
                let session = self.system_report_active.take().ok_or(NOT_FOUND)?;
                let manager = core::mem::take(&mut self.crash);
                ACTIVE.store(false, Ordering::Release);
                return Ok(Turn::Retired(RetiredReport { _manager:  manager,
                                                        _replaced: replaced,
                                                        _session:  session }));
            }
            let may_retire = self.system_report
                                 .as_ref()
                                 .and_then(|slot| slot.session.as_ref())
                                 .is_some_and(|session| session.lease.is_some())
                             || activity.system_report_abandoned();
            if !may_retire
            {
                return Ok(Turn::None);
            }
            let replaced = self.preserve_system_report_metadata();
            let slot = self.system_report.as_mut().ok_or(NOT_FOUND)?;
            let session = slot.session.take().ok_or(NOT_FOUND)?;
            let manager = core::mem::take(&mut slot.saved.crash);
            ACTIVE.store(false, Ordering::Release);
            return Ok(Turn::Retired(RetiredReport { _manager:  manager,
                                                    _replaced: replaced,
                                                    _session:  session }));
        }
        if let Some(result) = activity.take_system_report_power_result()
        {
            let applied = self.with_system_report_activity(&activity, |desktop| {
                                  desktop.finder_restart_terminal(desktop.crash
                                                                         .restart_error
                                                                         .unwrap_or(BAD_ABI),
                                                                  result)
                              })
                              .and_then(core::convert::identity);
            if let Err(cause) = applied
            {
                activity.restore_system_report_power_result(result);
                return Err(cause);
            }
        }
        let restart =
            self.system_report_active
                .as_mut()
                .or_else(|| self.system_report.as_mut().and_then(|slot| slot.session.as_mut()))
                .is_some_and(|session| core::mem::take(&mut session.restart_requested));
        if restart
        {
            return Ok(Turn::Restart(activity));
        }
        self.with_system_report_activity(&activity, |desktop| {
                if desktop.crash.info().window.0 == 0
                {
                    let window = desktop.resume_crash_report()?;
                    activity.set_object(window.0);
                }
                let report = desktop.crash.info().window;
                let mut redraw = desktop.windows
                                        .next_update_for(report, &desktop.events, Self::when())
                                        .is_some();
                while let Some(event) = desktop.events.next_matching(mac_abi::events::EVERY_EVENT)
                {
                    if event.window != report.0
                    {
                        continue;
                    }
                    if event.kind == mac_abi::EventKind::Update
                    {
                        redraw = true;
                    }
                    else if event.kind == mac_abi::EventKind::Activate
                              && event.modifiers & mac_abi::events::ACTIVE_FLAG == 0
                    {
                        desktop.cancel_crash_tracking();
                    }
                }
                if redraw
                {
                    desktop.resume_crash_report()?;
                }
                Ok::<_, Status>(())
            })??;
        Ok(Turn::None)
    }
}

/// One coalesced service checkpoint. Never call hardware/provider power paths
/// or destroy report owners under Desktop, Session, ledger or FLIGHT guards.
pub(crate) fn pump()
{
    if !CHANGED.swap(false, Ordering::AcqRel)
    {
        return;
    }
    match crate::kernel::with_desktop(|desktop| desktop.system_report_turn())
    {
        Err(_) => CHANGED.store(true, Ordering::Release),
        Ok(Err(_)) | Ok(Ok(Turn::None)) => (), // actual ACK/use/UI action wakes a retry
        Ok(Ok(Turn::Retired(retired))) => drop(retired),
        Ok(Ok(Turn::Restart(activity))) =>
        {
            let result = crate::toolbox::processes::finder::perform_system_report_restart();
            activity.system_report_power_result(result);
        }
    }
}

impl Desktop
{
    /// Park the stopped current Process's charged state in its ACTUAL Saved
    /// row before the reaper removes that row. The installed System context is
    /// the boot-admitted owner above, never a fabricated Process/BOOT_THREAD.
    pub(in crate::toolbox::runtime) fn park_stopped_system_ui(&mut self, owner: &OwnerRef)
                                                              -> Result<(), Status>
    {
        if self.system_ui_baseline
           || self.owner != owner.owner().incarnation()
           || owner.accepting()
           || self.processes.ledger(self.owner)?.owner() != owner.owner()
           || self.core_dialogs_active
           || self.system_report_active.is_some()
        {
            return Err(ACCESS_DENIED);
        }
        let at = self.locals.iter().position(|saved| saved.id == self.owner).ok_or(NOT_FOUND)?;
        let system = crate::toolbox::handles::system_owner_if_ready().ok_or(NOT_FOUND)?;
        let mut slot = self.system_report.take().ok_or(SERVICE_BUSY)?;
        let physical = (self.events.mouse, self.events.modifiers, self.events.keys);
        // Split this as field swaps to keep all ownership, including the empty
        // travelling placeholder. No manager is destroyed in this transition.
        core::mem::swap(&mut self.crash, &mut self.locals[at].crash);
        core::mem::swap(&mut self.dialogs, &mut self.locals[at].dialogs);
        core::mem::swap(&mut self.menus, &mut self.locals[at].menus);
        core::mem::swap(&mut self.cursors, &mut self.locals[at].cursors);
        core::mem::swap(&mut self.classic_font, &mut self.locals[at].classic_font);
        core::mem::swap(&mut self.events, &mut self.locals[at].events);
        core::mem::swap(&mut self.textedit, &mut self.locals[at].textedit);
        swap_ui(self, &mut slot.saved);
        if self.foreground == self.owner
        {
            self.foreground = system.owner().incarnation();
        }
        self.owner = system.owner().incarnation();
        self.system_ui_baseline = true;
        self.system_report_active = slot.session.take();
        self.system_report = Some(slot);
        self.events.mouse = physical.0;
        self.events.modifiers = physical.1;
        self.events.keys = physical.2;
        Ok(())
    }

    pub(in crate::toolbox::runtime) fn enter_process_from_system(&mut self, id: u64)
    {
        if !self.system_ui_baseline
           || self.core_dialogs_active
           || !crate::toolbox::handles::system_owner_if_ready().is_some_and(|owner| {
                                                                   owner.owner().incarnation()
                                                                   == self.owner
                                                               })
           || self.processes.get(id).is_err()
        {
            return;
        }
        let Some(at) = self.locals.iter().position(|saved| saved.id == id)
        else
        {
            return;
        };
        let Some(mut slot) = self.system_report.take()
        else
        {
            return;
        };
        let physical = (self.events.mouse, self.events.modifiers, self.events.keys);
        swap_ui(self, &mut slot.saved);
        core::mem::swap(&mut self.crash, &mut self.locals[at].crash);
        core::mem::swap(&mut self.dialogs, &mut self.locals[at].dialogs);
        core::mem::swap(&mut self.menus, &mut self.locals[at].menus);
        core::mem::swap(&mut self.cursors, &mut self.locals[at].cursors);
        core::mem::swap(&mut self.classic_font, &mut self.locals[at].classic_font);
        core::mem::swap(&mut self.events, &mut self.locals[at].events);
        core::mem::swap(&mut self.textedit, &mut self.locals[at].textedit);
        slot.session = self.system_report_active.take();
        self.system_report = Some(slot);
        self.system_ui_baseline = false;
        self.owner = id;
        self.events.mouse = physical.0;
        self.events.modifiers = physical.1;
        self.events.keys = physical.2;
    }
}

impl Desktop
{
    /// Existing native CRASH calls retain their authentic beneficiary view,
    /// while capture writes require a fresh BEGIN after real service teardown.
    pub(crate) fn system_report_request(&mut self, owner: &OwnerRef,
                                        call: &mut mac_abi::crashes::CrashCall, input: &str,
                                        output: &mut [u8])
                                        -> Result<bool, Status>
    {
        use mac_abi::crashes::*;
        if !self.system_report_for_beneficiary(owner)?
        {
            return Ok(false);
        }
        let Some((session, manager)) = self.system_report_input_state()
        else
        {
            return Err(NOT_FOUND);
        };
        match call.operation
        {
            INFO | READ =>
            {
                // Pure snapshot access needs no Process activation, executable
                // readiness or mutation of the protected immutable payload.
                if self.system_report_active.is_some()
                {
                    self.crash.request(call, input, output)?;
                }
                else if let Some(slot) = &mut self.system_report
                {
                    slot.saved.crash.request(call, input, output)?;
                }
            }
            SCROLL | SHOW =>
            {
                if session.activity.closing() || !owner.accepting()
                {
                    return Err(SERVICE_BUSY);
                }
                let activity = session.activity.clone();
                self.with_system_report_activity(&activity, |desktop| {
                        desktop.crash_request(call, input, output)
                    })??;
            }
            DISMISS =>
            {
                call.info = manager.info();
                self.request_system_report_close_for(owner)?;
            }
            APPEND => return Err(SERVICE_BUSY),
            _ => return Err(mac_abi::BAD_ARGUMENT)
        }
        Ok(true)
    }

    /// The actual protected archive remains the original payload authority.
    /// This copies only its existing correlation/layout metadata into the
    /// genuine surviving beneficiary's empty Saved; no Data Arc pins the next
    /// capture. Every displaced value is returned for destruction outside.
    fn preserve_system_report_metadata(&mut self) -> Option<crashes::Manager>
    {
        let (session, manager) = self.system_report_input_state()?;
        let binding = session.beneficiary.retain();
        let mut metadata = manager.dismissed_metadata();
        let current = match self.processes.resource_binding(binding.owner())
        {
            Ok(current) => current,
            Err(_) => return Some(metadata)
        };
        if current.ledger().owner() != binding.ledger().owner()
           || current.image().map(crate::images::Binding::id)
              != binding.image().map(crate::images::Binding::id)
        {
            return Some(metadata);
        }
        let recipient = if self.owner == binding.owner()
        {
            Some(&mut self.crash)
        }
        else
        {
            self.locals
                .iter_mut()
                .find(|saved| saved.id == binding.owner())
                .map(|saved| &mut saved.crash)
        };
        if let Some(recipient) = recipient
           && recipient.info().app == 0
        {
            core::mem::swap(recipient, &mut metadata);
        }
        Some(metadata)
    }
}
