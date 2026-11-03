//! The original native DIALOG CLOSE envelope observes canonical Window cleanup.
//! A Desktop refusal retains its start stage; it is not a completed close.
#![forbid(unsafe_code)]
use mac_abi::{ACCESS_DENIED, BAD_ARGUMENT, CLOSE_WINDOW, NOT_FOUND, NOT_SUPPORTED, Status,
              WindowId, WindowRequest, dialogs::DialogCall};

use super::{Toolbox, TrustedWindowClose, UserWindowOperation, WindowBeginError};
use crate::{kernel::DesktopEpoch, recovery::Activity, toolbox::handles::OwnerRef};

/// An observer of a genuine admitted WIND operation. It owns no Activity and
/// cannot create, release or acknowledge a Window by numeric identity.
#[derive(Clone)]
pub(crate) enum DialogCloseObserver
{
    User(alloc::sync::Arc<crate::toolbox::windows::manager::operations::WindowJob>),
    Trusted(crate::toolbox::windows::manager::trusted::TrustedCloseHandle)
}
impl DialogCloseObserver
{
    pub(crate) fn window_id(&self) -> WindowId
    {
        match self
        {
            Self::User(job) => job.window_id(),
            Self::Trusted(handle) => handle.window_id()
        }
    }

    pub(crate) fn owner(&self) -> u64
    {
        match self
        {
            Self::User(job) => job.owner(),
            Self::Trusted(handle) => handle.owner()
        }
    }

    pub(crate) fn storage_owner(&self) -> mac_object_table::domain::DomainId
    {
        match self
        {
            Self::User(job) => job.storage_owner(),
            Self::Trusted(handle) => handle.storage_owner()
        }
    }

    pub(crate) fn status(&self) -> Option<Status>
    {
        use crate::toolbox::windows::manager::CloseError;
        let result = match self
        {
            Self::User(job) =>
            {
                job.close_result().map(|outcome| {
                                      match outcome {
                crate::toolbox::windows::manager::operations::Outcome::Closed(result) => result,
                crate::toolbox::windows::manager::operations::Outcome::Quiesced(id) => Ok(id)
            }
                                  })
            }
            Self::Trusted(handle) => handle.poll_result()
        };
        result.map(|result| {
                  match result
                  {
                      Ok(_) => mac_abi::OK,
                      Err(CloseError::NotFound(_)) => NOT_FOUND,
                      Err(CloseError::Cleanup(cause)) => super::close_ui::cleanup_status(cause)
                  }
              })
    }
}

enum Backend
{
    User(UserWindowOperation),
    Trusted(TrustedWindowClose)
}
impl Backend
{
    fn begin(request: &WindowRequest) -> Result<Self, WindowBeginError>
    {
        if let Some(operation) = UserWindowOperation::begin(CLOSE_WINDOW, request)?
        {
            return Ok(Self::User(operation));
        }
        if let Some(operation) = TrustedWindowClose::begin(CLOSE_WINDOW, request)?
        {
            return Ok(Self::Trusted(operation));
        }
        Err(WindowBeginError::Service(NOT_SUPPORTED))
    }

    fn advance(&mut self) -> Result<Option<Status>, Status>
    {
        match self
        {
            Self::User(operation) => operation.advance(),
            Self::Trusted(operation) => operation.advance()
        }
    }

    fn wait(&self) -> Result<(), Status>
    {
        match self
        {
            Self::User(operation) => operation.wait(),
            Self::Trusted(operation) => operation.wait()
        }
    }
}

pub(crate) struct UserDialogClose
{
    id:           WindowId,
    owner:        OwnerRef,
    activity:     Option<Activity>,
    validated:    bool,
    backend:      Option<Backend>,
    desktop_wait: Option<DesktopEpoch>,
    terminal:     Option<Status>
}
impl UserDialogClose
{
    pub(crate) fn prepare(call: &DialogCall) -> Result<Self, Status>
    {
        if call.operation != mac_abi::dialogs::CLOSE || call.reserved != 0 || call.window == 0
        {
            return Err(BAD_ARGUMENT);
        }
        let owner = crate::context::current_owner()?;
        let id = WindowId(call.window);
        let activity = crate::recovery::alert_close_activity(&owner, id);
        Ok(Self { id,
                  owner,
                  activity,
                  validated: false,
                  backend: None,
                  desktop_wait: None,
                  terminal: None })
    }

    fn validate(&self, toolbox: &mut Toolbox) -> Result<(), Status>
    {
        let (recipient, _) = toolbox.service_owners()?;
        if recipient.owner() != self.owner.owner()
        {
            return Err(ACCESS_DENIED);
        }
        if let Some(activity) = &self.activity
        {
            toolbox.with_core_dialogs(|toolbox| {
                       if toolbox.dialogs.recovery_window(activity)? != Some(self.id.0)
                       {
                           return Err(NOT_FOUND);
                       }
                       if toolbox.dialogs.closing
                       {
                           return Err(mac_abi::SERVICE_BUSY);
                       }
                       Ok(())
                   })
        }
        else if toolbox.dialogs.system_menu
        {
            Err(BAD_ARGUMENT)
        }
        else if !toolbox.dialogs.active() || toolbox.dialogs.window != self.id.0
        {
            Err(NOT_FOUND)
        }
        else if toolbox.dialogs.closing
        {
            Err(mac_abi::SERVICE_BUSY)
        }
        else
        {
            Ok(())
        }
    }

    fn scoped<R>(&self, operation: impl FnOnce() -> R) -> Result<R, Status>
    {
        match &self.activity
        {
            Some(activity) => activity.run(operation),
            None => Ok(operation())
        }
    }

    pub(crate) fn advance(&mut self) -> Result<Option<Status>, Status>
    {
        if let Some(status) = self.terminal
        {
            return Ok(Some(status));
        }
        self.desktop_wait = None;
        if !self.validated
        {
            let epoch = crate::kernel::window_desktop_epoch();
            let checked = self.scoped(|| {
                                  crate::kernel::with_window_toolbox_owned((), |(), toolbox| {
                                      self.validate(toolbox)
                                  })
                              })?;
            match checked
            {
                Ok(Ok(())) => self.validated = true,
                Ok(Err(status)) => return self.finish(status),
                Err((crate::state::AccessError::Busy, ())) =>
                {
                    self.desktop_wait = Some(epoch);
                    return Ok(None);
                }
                Err((cause, ())) => return self.finish(cause.status())
            }
        }
        if self.backend.is_none()
        {
            let request = WindowRequest { id:             self.id.0,
                                          bounds:         mac_abi::geometry::Rect::ZERO,
                                          title:          [0; mac_abi::WINDOW_TITLE_BYTES],
                                          title_length:   0,
                                          procedure:      0,
                                          minimum_width:  0,
                                          minimum_height: 0,
                                          reserved:       0 };
            match self.scoped(|| Backend::begin(&request))?
            {
                Ok(backend) => self.backend = Some(backend),
                Err(WindowBeginError::Desktop { cause: crate::state::AccessError::Busy,
                                                epoch }) =>
                {
                    self.desktop_wait = Some(epoch);
                    return Ok(None);
                }
                Err(cause) => return self.finish(cause.status())
            }
        }
        let result = match self.backend.as_mut()
        {
            Some(backend) =>
            {
                match &self.activity
                {
                    Some(activity) => activity.run(|| backend.advance())?,
                    None => backend.advance()
                }
            }
            None => return self.finish(mac_abi::BAD_ABI)
        };
        match result
        {
            Ok(Some(status)) => self.finish(status),
            Ok(None) => Ok(None),
            Err(status) => self.finish(status)
        }
    }

    fn finish(&mut self, status: Status) -> Result<Option<Status>, Status>
    {
        self.terminal = Some(status);
        Ok(Some(status))
    }

    pub(crate) fn wait(&self) -> Result<(), Status>
    {
        if let Some(epoch) = self.desktop_wait
        {
            return crate::machine::scheduler::callbacks::wait_desktop(epoch).map(|_| ());
        }
        match &self.backend
        {
            Some(backend) => backend.wait(),
            None if self.terminal.is_some() => Ok(()),
            None => Err(mac_abi::BAD_ABI)
        }
    }
}
