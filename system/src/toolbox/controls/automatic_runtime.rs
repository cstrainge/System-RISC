//! Original EVENT-envelope layout prelude. Readiness stays in actual per-owner
//! CTRL Work; this value owns only its affine batch/calculation and waiters.
#![forbid(unsafe_code)]
use super::*;
use crate::toolbox::{controls::objects::{AutomaticBatchLease, AutomaticRoot, ControlLayout,
                                         LayoutProgress, PreparedWindowLayout},
                     handles::OwnerRef};

enum State
{
    Working(ControlLayout),
    Ready(PreparedWindowLayout)
}
pub(crate) struct AutomaticPrelude
{
    active:     Option<State>,
    roots:      Vec<AutomaticRoot>,
    at:         usize,
    recipient:  OwnerRef,
    storage:    OwnerRef,
    image:      crate::images::Binding,
    lease:      Arc<AutomaticBatchLease>,
    allocation: crate::machine::scheduler::accounting::AllocationAuthority,
    now:        u64,
    complete:   bool
}
pub(crate) enum AutomaticProgress
{
    Wait(Arc<crate::toolbox::resources::definitions::callbacks::Pending>),
    Complete
}
impl AutomaticPrelude
{
    /// Eligibility is derived from the actual typed polling envelope by the
    /// native dispatcher; no request is converted into another wire layout.
    pub(crate) fn begin(suspension: Result<(), Status>) -> Result<Option<Self>, Status>
    {
        if crate::context::recovery_activity().is_some()
           || crate::machine::scheduler::callbacks::authenticate_current().is_ok()
        {
            // The actual callback role cannot wait on its own queue; original
            // coalesced automatic work remains for an application poll.
            return Ok(None);
        }
        let now = Toolbox::uptime();
        let source =
            toolbox(|runtime| {
                let (recipient, storage) =
                    runtime.service_owners()
                           .map_err(|status| service("automatic owners", status))?;
                if !runtime.controls
                           .user_automatic_ready(&recipient, now)
                           .map_err(|status| service("automatic readiness", status))?
                {
                    return Ok(None);
                }
                if recipient.owner() != storage.owner()
                   || !recipient.accepting()
                   || !storage.accepting()
                {
                    return Err(service("automatic storage or closed owner", ACCESS_DENIED));
                }
                let image = runtime.processes
                                   .get(runtime.owner)
                                   .map_err(|status| service("automatic original process", status))?
                                   .image
                                   .ok_or(service("automatic original image", BAD_ABI))?;
                let allocation = runtime.processes
                                        .allocation(runtime.owner)
                                        .map_err(|status| service("automatic account", status))?;
                if allocation.owner() != storage.owner()
                {
                    return Err(service("automatic account owner", ACCESS_DENIED));
                }
                Ok(Some((recipient, storage, image, allocation)))
            }).map_err(ControlAdmissionError::status)?;
        let Some((recipient, storage, image, allocation)) = source
        else
        {
            return Ok(None);
        };
        suspension?;
        let thread = crate::context::current_thread_context().identity().thread;
        let lease = match AutomaticBatchLease::prepare(&recipient, image, thread, &allocation)
        {
            Ok(lease) => lease,
            Err(NO_MEMORY) =>
            {
                toolbox(|runtime| {
                    let (live_recipient, live_storage) =
                        runtime.service_owners()
                               .map_err(|status| service("automatic refusal owners", status))?;
                    if live_recipient.owner() != recipient.owner()
                       || live_storage.owner() != storage.owner()
                       || runtime.processes
                                 .get(runtime.owner)
                                 .map_err(|status| service("automatic refusal process", status))?
                                 .image
                          != Some(image)
                    {
                        return Err(service("automatic refusal binding", ACCESS_DENIED));
                    }
                    runtime.controls
                           .automatic_admission_refused(recipient.owner().incarnation(), now);
                    Ok(())
                }).map_err(ControlAdmissionError::status)?;
                return Ok(None);
            }
            Err(status) => return Err(status)
        };
        let roots = toolbox(|runtime| {
                        let (live_recipient, live_storage) =
                            runtime.service_owners()
                                   .map_err(|status| service("automatic install owners", status))?;
                        let live_image =
                            runtime.processes
                                   .get(runtime.owner)
                                   .map_err(|status| service("automatic install process", status))?
                                   .image;
                        if live_recipient.owner() != recipient.owner()
                           || live_storage.owner() != storage.owner()
                           || live_image != Some(image)
                           || !lease.matches(&live_recipient, image)
                        {
                            return Err(service("automatic install binding", ACCESS_DENIED));
                        }
                        runtime.controls
                               .capture_automatic_batch(&recipient, &lease, now)
                               .map_err(|status| service("automatic batch admission", status))
                    }).map_err(ControlAdmissionError::status);
        let roots = match roots
        {
            Ok(roots) => roots,
            Err(NO_MEMORY) =>
            {
                toolbox(|runtime| {
                    let (live_recipient, live_storage) =
                        runtime.service_owners()
                               .map_err(|status| service("automatic refusal owners", status))?;
                    if live_recipient.owner() != recipient.owner()
                       || live_storage.owner() != storage.owner()
                       || runtime.processes
                                 .get(runtime.owner)
                                 .map_err(|status| service("automatic refusal process", status))?
                                 .image
                          != Some(image)
                    {
                        return Err(service("automatic refusal binding", ACCESS_DENIED));
                    }
                    runtime.controls
                           .automatic_admission_refused(recipient.owner().incarnation(), now);
                    Ok(())
                }).map_err(ControlAdmissionError::status)?;
                return Ok(None);
            }
            Err(status) => return Err(status)
        };
        let Some((roots, retired)) = roots
        else
        {
            return Ok(None);
        };
        // End the previous expired header outside Desktop, after actual install.
        drop(retired);
        Ok(Some(Self { active: None,
                       roots,
                       at: 0,
                       recipient,
                       storage,
                       image,
                       lease,
                       allocation,
                       now,
                       complete: false }))
    }

    fn validate(&self, runtime: &Toolbox) -> Result<(), Status>
    {
        let (recipient, storage) = runtime.service_owners()?;
        if recipient.owner() != self.recipient.owner()
           || storage.owner() != self.storage.owner()
           || runtime.processes.get(runtime.owner)?.image != Some(self.image)
           || !self.lease.matches(&recipient, self.image)
        {
            return Err(ACCESS_DENIED);
        }
        Ok(())
    }

    fn refused(&self, root: AutomaticRoot, status: Status) -> Result<(), Status>
    {
        toolbox(|runtime| {
            self.validate(runtime)
                .map_err(|status| service("automatic refusal binding", status))?;
            if runtime.controls.layout_refused(root.id, status, self.now)
            {
                serial!("CONTROL_LAYOUT_DEFERRED root={} window={} status={status}; previous \
                         geometry retained",
                        root.id,
                        root.window);
            }
            Ok(())
        }).map_err(ControlAdmissionError::status)
    }

    pub(crate) fn advance(&mut self) -> Result<AutomaticProgress, Status>
    {
        if self.complete
        {
            return Ok(AutomaticProgress::Complete);
        }
        loop
        {
            let Some(root) = self.roots.get(self.at).copied()
            else
            {
                toolbox(|runtime| {
                    self.validate(runtime)
                        .map_err(|status| service("automatic completion binding", status))?;
                    runtime.controls.finish_layout_work(self.recipient.owner().incarnation());
                    Ok(())
                }).map_err(ControlAdmissionError::status)?;
                self.complete = true;
                return Ok(AutomaticProgress::Complete);
            };
            if self.active.is_none()
            {
                let plan = toolbox(|runtime| {
                               self.validate(runtime)
                                   .map_err(|status| service("automatic plan binding", status))?;
                               if !runtime.owns(WindowId(root.window))
                               {
                                   return Err(service("automatic Window owner", ACCESS_DENIED));
                               }
                               let viewport =
                                   runtime.windows
                                          .window(WindowId(root.window))
                                          .ok_or(service("automatic live Window", NOT_FOUND))?
                                          .local_bounds();
                               let pending=runtime.presentation.pending_content.iter()
                        .any(|content|content.id==SceneTarget::from(root.window));
                               runtime.controls
                                      .begin_automatic_layout(root, &self.recipient, viewport,
                                                              pending, &self.allocation)
                                      .map_err(|status| service("automatic plan", status))
                           }).map_err(ControlAdmissionError::status);
                let plan = match plan
                {
                    Ok(plan) => plan,
                    Err(status) =>
                    {
                        self.refused(root, status)?;
                        self.at += 1;
                        continue;
                    }
                };
                let thread = crate::context::current_thread_context().identity().thread;
                match plan.prepare(thread.into(), &self.allocation)
                {
                    Ok(work) => self.active = Some(State::Working(work)),
                    Err((status, plan)) =>
                    {
                        drop(plan);
                        self.refused(root, status)?;
                        self.at += 1;
                        continue;
                    }
                }
            }
            match self.active.as_mut().ok_or(BAD_ABI)?
            {
                State::Working(work) =>
                {
                    match work.advance()
                    {
                        Ok(LayoutProgress::Wait(pending)) =>
                        {
                            return Ok(AutomaticProgress::Wait(pending));
                        }
                        Ok(LayoutProgress::Ready(ready)) => self.active = Some(State::Ready(ready)),
                        Err(status) =>
                        {
                            self.active = None;
                            self.refused(root, status)?;
                            self.at += 1;
                        }
                    }
                }
                State::Ready(ready) =>
                {
                    let recipient = &self.recipient;
                    let storage = &self.storage;
                    let image = self.image;
                    let lease = &self.lease;
                    let result =
                        toolbox(|runtime| {
                            let (live_recipient, live_storage) =
                                runtime.service_owners()
                                       .map_err(|status| {
                                           service("automatic publish owners", status)
                                       })?;
                            if live_recipient.owner() != recipient.owner()
                               || live_storage.owner() != storage.owner()
                               || runtime.processes
                                         .get(runtime.owner)
                                         .map_err(|status| {
                                             service("automatic publish process", status)
                                         })?
                                         .image
                                  != Some(image)
                               || !lease.matches(&live_recipient, image)
                            {
                                return Err(service("automatic publish binding", ACCESS_DENIED));
                            }
                            runtime.commit_user_control_layout(ready)
                                   .map_err(|status| service("automatic publish", status))
                        }).map_err(ControlAdmissionError::status);
                    // Original retired state/readers/claim leave after Desktop.
                    self.active = None;
                    if let Err(status) = result
                    {
                        self.refused(root, status)?;
                    }
                    self.at += 1;
                }
            }
        }
    }
}
