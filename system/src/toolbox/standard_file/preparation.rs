//! Detached payload for the existing System-owned Standard File publication.
//! Preparation and refused payload destruction occur outside Desktop. The
//! original reserved endpoint still owns identity, completion and child ACK.
use core::fmt;

use mac_object_table::{shared::InvocationGate as _, store::PublicationError};

use super::*;
use crate::{recovery::Activity, toolbox::handles::InvocationGate};

#[derive(Clone, Copy, Debug)]
pub(crate) enum BeginError
{
    Service
    {
        stage:  &'static str,
        status: Status
    },
    Publication(PublicationError)
}
impl BeginError
{
    pub(crate) fn service(stage: &'static str, status: Status) -> Self
    {
        Self::Service { stage,
                        status }
    }

    pub(crate) fn status(self) -> Status
    {
        match self
        {
            Self::Service { status,
                            .. } => status,
            Self::Publication(PublicationError::Busy) => SERVICE_BUSY,
            Self::Publication(PublicationError::Pool(cause)) => pool_status(cause)
        }
    }
}
impl fmt::Display for BeginError
{
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::Service { stage,
                            status } =>
            {
                write!(output, "Standard File {stage} refused with status {status}")
            }
            Self::Publication(cause) =>
            {
                write!(output, "Standard File reserved publication refused: {cause:?}")
            }
        }
    }
}

/// The same owned Dialog stays here until its real canonical birth. Even a
/// final publication refusal restores it; the outer caller disposes this
/// envelope only after Desktop has been released.
#[must_use = "retain and dispose Standard File preparation outside Desktop"]
pub(crate) struct PreparedDialog
{
    dialog:    Option<Dialog>,
    recipient: OwnerRef,
    storage:   OwnerRef,
    directory: u32
}

fn inline(bytes: &[u8], length: u32) -> Result<&str, Status>
{
    core::str::from_utf8(bytes.get(..length as usize).ok_or(BAD_ARGUMENT)?).map_err(|_| {
                                                                               BAD_ARGUMENT
                                                                           })
}
fn validate_call(call: &StandardFileCall) -> Result<(&str, &str), Status>
{
    if call.operation != BEGIN
       || call.reserved != [0; 2]
       || call.mode > PUT
       || !(-1..=MAX_TYPES as i32).contains(&call.type_count)
    {
        return Err(BAD_ARGUMENT);
    }
    Ok((inline(&call.prompt, call.prompt_length)?, inline(&call.name, call.name_length)?))
}

fn validate_scope(activity: &Activity, recipient: &OwnerRef, storage: &OwnerRef)
                  -> Result<(), Status>
{
    activity.validate_standard_file_begin(recipient, storage)?;
    if !crate::context::recovery_activity().is_some_and(|actual| actual.same_session(activity))
       || !crate::context::io_subject().is_some_and(|subject| {
                                           subject.scope()
                                                  .same_scope(activity.root_subject().scope())
                                       })
    {
        return Err(ACCESS_DENIED);
    }
    Ok(())
}

impl PreparedDialog
{
    /// Called under the genuine Opening's Activity/System allocation and
    /// protected backing scope, with no Desktop or Store guard retained.
    pub(crate) fn prepare(activity: &Activity, call: &StandardFileCall)
                          -> Result<Self, BeginError>
    {
        let (prompt, name) =
            validate_call(call).map_err(|status| BeginError::service("request", status))?;
        let recipient = activity.recipient();
        let storage = activity.storage_owner();
        validate_scope(activity, &recipient, &storage).map_err(|status| {
                                                          BeginError::service("service admission",
                                                                              status)
                                                      })?;
        let owner = recipient.owner().incarnation();
        let prompt = Text::new(prompt).map_err(|status| BeginError::service("prompt", status))?;
        let mut editor =
            Editor::new(Rect::new(15, 157, 4111, 173), Rect::new(15, 157, 227, 173))
                .map_err(|status| BeginError::service("filename editor", status))?;
        editor.set_text(name).map_err(|status| BeginError::service("filename editor", status))?;
        editor.select(0, name.len() as u32)
              .map_err(|status| BeginError::service("filename selection", status))?;
        editor.activate(true);
        // Preserve the original editor/font/input order and character set;
        // these operations need the System font service, not a born Window.
        let fonts =
            crate::kernel::fonts().ok_or(BeginError::service("font service", NOT_SUPPORTED))?;
        editor.set_font(mac_abi::quickdraw::FontSelection::default(), fonts)
              .map_err(|status| BeginError::service("font", status))?;
        let mut characters = Text::default();
        for byte in 32..=255
        {
            characters.push(mac_formats::text::character(byte).encode_utf8(&mut [0; 4]))
                      .map_err(|status| BeginError::service("font input", status))?;
        }
        editor.prepare_font_input(characters.text().map_err(|status| {
                                                        BeginError::service("font input", status)
                                                    })?,
                                  fonts)
              .map_err(|status| BeginError::service("font input", status))?;
        let mut types = [[0; 4]; MAX_TYPES];
        let type_count = call.type_count.max(0) as usize;
        types[..type_count].copy_from_slice(&call.types[..type_count]);
        let mut icons = Vec::new();
        icons.try_reserve_exact(page::ROWS)
             .map_err(|_| BeginError::service("icon page", NO_MEMORY))?;
        let mut ancestors = Vec::new();
        ancestors.try_reserve_exact(page::ANCESTORS)
                 .map_err(|_| BeginError::service("ancestors", NO_MEMORY))?;
        let mut entries = Page::default();
        entries.reset();
        let subject = activity.subject();
        let dialog =
            Dialog { saved_name: None,
                     owner,
                     window: WindowId(0),
                     auxiliary: WindowId(0),
                     auxiliary_sheet: Sheet::Main,
                     mode: call.mode,
                     location: (call.volume,
                                if call.directory == 0 { ROOT_ID } else { call.directory }),
                     entries,
                     icons,
                     selected: None,
                     first: 0,
                     prompt,
                     editor,
                     focus_name: call.mode == PUT,
                     name_tracking: false,
                     state: PENDING,
                     result: CatalogInfo::default(),
                     replacing: false,
                     types,
                     type_count,
                     all: call.type_count == -1,
                     sheet: Sheet::Main,
                     error: OK,
                     last_click: None,
                     popup: false,
                     ancestors,
                     typeahead: Text::default(),
                     typed_at: 0,
                     name_format: None,
                     volume_info: VolumeInfo::default(),
                     stage: Some(if call.volume == 0
                                 {
                                     Stage::DefaultVolume
                                 }
                                 else
                                 {
                                     Stage::Volume
                                 }),
                     issued: false,
                     revision: 1,
                     desktop_count: 0,
                     create_when: 0,
                     main_widgets: Widgets::default(),
                     auxiliary_widgets: Widgets::default(),
                     subject: Some(subject),
                     recovery_activity: Some(activity.clone()) };
        Ok(Self { dialog: Some(dialog),
                  recipient,
                  storage,
                  directory: call.directory })
    }

    fn validate(&self, recipient: &OwnerRef, storage: &OwnerRef, call: &StandardFileCall)
                -> Result<(), Status>
    {
        let (prompt, name) = validate_call(call)?;
        let dialog = self.dialog.as_ref().ok_or(BAD_ABI)?;
        let activity = dialog.recovery_activity.as_ref().ok_or(ACCESS_DENIED)?;
        if recipient.owner() != self.recipient.owner()
           || storage.owner() != self.storage.owner()
           || dialog.owner != recipient.owner().incarnation()
        {
            return Err(ACCESS_DENIED);
        }
        validate_scope(activity, recipient, storage)?;
        if !dialog.subject.as_ref().is_some_and(|subject| {
            subject.scope().same_scope(activity.root_subject().scope())
        })
        {
            return Err(ACCESS_DENIED);
        }
        let count = call.type_count.max(0) as usize;
        if dialog.mode != call.mode
           || dialog.location.0 != call.volume
           || self.directory != call.directory
           || dialog.type_count != count
           || dialog.all != (call.type_count == -1)
           || dialog.types[..count] != call.types[..count]
           || dialog.prompt.text()? != prompt
           || dialog.name() != name
        {
            return Err(BAD_ARGUMENT);
        }
        Ok(())
    }
}

impl Manager
{
    /// Every prebirth refusal leaves the same owned Dialog in `prepared`.
    /// Only the reserved canonical publication consumes it; its original
    /// initialization guard then owns rollback and actual child cleanup.
    pub(crate) fn begin_prepared(&mut self, recipient: &OwnerRef, storage: &OwnerRef,
                                 call: &mut StandardFileCall, prepared: &mut PreparedDialog)
                                 -> Result<(), BeginError>
    {
        // The owned publisher performs no payload allocation or callback.
        // Keep final session/owner checks and birth in the same native turn;
        // all refused heavy payloads remain in the outer envelope.
        InvocationGate::run(|| {
            prepared.validate(recipient, storage, call)
                    .map_err(|status| BeginError::service("final admission", status))?;
            if self.active(recipient.owner().incarnation()).is_some()
            {
                return Err(BeginError::service("protocol vacancy", SERVICE_BUSY));
            }
            let reservation = self.publication
                                  .as_mut()
                                  .ok_or(BeginError::service("reserved endpoint", NOT_SUPPORTED))?;
            let dialog =
                prepared.dialog.take().ok_or(BeginError::service("prepared payload", BAD_ABI))?;
            let Some(activity) = dialog.recovery_activity.as_ref()
            else
            {
                prepared.dialog = Some(dialog);
                return Err(BeginError::service("prepared activity", ACCESS_DENIED));
            };
            let subject = activity.root_subject();
            let pin = activity.acknowledgement_pin();
            let state = dialog.state;
            let replacing = u32::from(dialog.replacing);
            let entry = dialog.result;
            match self.slots.insert_reserved_in_scope_owned_with_acknowledgement(reservation,
                                                                                 subject.scope(),
                                                                                 Some(pin),
                                                                                 dialog)
            {
                Ok(publication) =>
                {
                    call.handle = publication.id();
                    call.state = state;
                    call.replacing = replacing;
                    call.entry = entry;
                    Ok(())
                }
                Err((cause, dialog)) =>
                {
                    prepared.dialog = Some(dialog);
                    Err(BeginError::Publication(cause))
                }
            }
        })
    }
}
