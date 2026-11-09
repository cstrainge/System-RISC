//! Detached immutable property preparation for the SAME canonical CTRL object.
//! No allocation, CDEF execution or byte-buffer destruction happens while the
//! manager publishes. A real completion key, original title and spec are checked
//! again before swapping the prepared title/property snapshot.
#![forbid(unsafe_code)]
use mac_control_model::{Codec, Model, Record, Sink};
use mac_control_state::{Buffer as Bytes, State as PropertyData};

use super::{super::snapshot::Title, *};

pub(crate) struct PropertySnapshot
{
    id:         u64,
    owner:      u64,
    key:        mac_object_table::store::CompletionKey,
    spec:       Spec,
    title:      Arc<Title>,
    definition: crate::toolbox::resources::definitions::RetainedDefinition
}
pub(crate) struct PreparedProperties
{
    snapshot: PropertySnapshot,
    title:    Arc<Title>,
    relayout: bool
}
fn failure(error: mac_control_model::Error) -> Status
{
    match error
    {
        mac_control_model::Error::BadArgument => BAD_ARGUMENT,
        mac_control_model::Error::NoMemory => NO_MEMORY
    }
}
impl PropertySnapshot
{
    pub(crate) fn message(
        &self, input: &[u8])
        -> Result<(super::super::DefinitionRef,
                   crate::toolbox::resources::definitions::callbacks::packet::Request),
                  Status>
    {
        use crate::toolbox::resources::definitions::callbacks::packet::{Arguments, Object, Request};
        mac_control_model::messages::Request::read(input).map_err(failure)?;
        let Spec::Standard { state,
                             procedure } = self.spec
        else
        {
            return Err(BAD_ARGUMENT);
        };
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(input.len()).map_err(|_| NO_MEMORY)?;
        bytes.extend_from_slice(input);
        Ok((self.definition.reference(),
            Request { properties: self.title.properties.clone(),
                      kind:       mac_abi::definitions::CDEF,
                      id:         procedure >> 4,
                      variation:  procedure & 15,
                      message:    mac_abi::controls::MESSAGE_DEFINITION,
                      reflow:     0,
                      object:     Object::Control(state),
                      arguments:  Arguments::ControlMessage(bytes),
                      surface:    None }))
    }

    pub(crate) fn revision(&self) -> u64
    {
        self.title.revision
    }

    pub(crate) fn kind(&self) -> i16
    {
        match self.spec
        {
            Spec::Standard { procedure,
                             .. } => procedure >> 4,
            _ => -1
        }
    }

    fn variation(&self) -> i16
    {
        match self.spec
        {
            Spec::Standard { procedure,
                             .. } => procedure & 15,
            _ => 0
        }
    }

    fn data(&self) -> Result<PropertyData, Status>
    {
        if let Some(data) = &self.title.properties
        {
            return PropertyData::new(data.kind(), data.as_bytes()).map_err(failure);
        }
        if self.kind() == 17
        {
            let mut view = mac_control_model::TextEdit::default();
            view.variant = match self.variation()
            {
                0 => mac_control_model::EditVariant::Text,
                1 => mac_control_model::EditVariant::View,
                2 => mac_control_model::EditVariant::Password,
                _ => return Err(BAD_ARGUMENT)
            };
            let mut bytes = Bytes::new();
            view.write(&mut bytes).map_err(failure)?;
            return PropertyData::new(17, bytes.as_slice()).map_err(failure);
        }
        PropertyData::defaults(self.kind()).map_err(failure)
    }

    pub(crate) fn read(&self, index: u32, output: &mut [u8]) -> Result<usize, Status>
    {
        // A GET only copies the requested immutable field. Existing records need no
        // allocation. Default records are materialized outside Desktop, once here.
        let default;
        let data = if let Some(data) = self.title.properties.as_deref()
        {
            data
        }
        else
        {
            default = self.data()?;
            &default
        };
        let bytes = if index == ALL_PROPERTIES
        {
            data.as_bytes()
        }
        else
        {
            data.field(index as usize).map_err(failure)?
        };
        if output.len() >= bytes.len()
        {
            output[..bytes.len()].copy_from_slice(bytes)
        }
        Ok(bytes.len())
    }

    pub(crate) fn prepare(self, index: u32, input: &[u8]) -> Result<PreparedProperties, Status>
    {
        let default;
        let previous = if let Some(data) = self.title.properties.as_deref()
        {
            data
        }
        else
        {
            default = self.data()?;
            &default
        };
        let old = if index == ALL_PROPERTIES
        {
            previous.as_bytes()
        }
        else
        {
            previous.field(index as usize).map_err(failure)?
        };
        if old == input
        {
            return Ok(PreparedProperties { title:    self.title.clone(),
                                           snapshot: self,
                                           relayout: false });
        }
        let data = if index == ALL_PROPERTIES
        {
            PropertyData::new(self.kind(), input).map_err(failure)?
        }
        else
        {
            previous.replaced(index as usize, input).map_err(failure)?
        };
        if self.kind() == 17
           && mac_control_model::TextEdit::read(data.as_bytes()).map_err(failure)?.variant as i16
              != self.variation()
        {
            return Err(BAD_ARGUMENT);
        }
        let fields = mac_control_model::field_names(self.kind()).ok_or(BAD_ARGUMENT)?;
        let mut relayout = false;
        for name in ["minimum", "maximum"]
        {
            let index = fields.iter().position(|field| *field == name).ok_or(BAD_ABI)?;
            relayout |=
                previous.field(index).map_err(failure)? != data.field(index).map_err(failure)?;
        }
        let revision = self.title.revision.checked_add(1).ok_or(NO_MEMORY)?;
        let properties = Arc::try_new(data).map_err(|_| NO_MEMORY)?;
        let title=Arc::try_new(Title::with_properties(&self.title,properties,revision)).map_err(|_|NO_MEMORY)?;
        Ok(PreparedProperties { snapshot: self,
                                title,
                                relayout })
    }
}
impl Manager
{
    pub(crate) fn property_snapshot(&self, id: u64, owner: u64)
                                    -> Result<PropertySnapshot, Status>
    {
        let object = self.get(id, owner)?;
        let Spec::Standard { procedure,
                             .. } = object.state.spec
        else
        {
            return Err(NOT_SUPPORTED);
        };
        if mac_control_model::field_names(procedure >> 4).is_none()
        {
            return Err(NOT_SUPPORTED);
        }
        let key = self.objects.completion_key(id).ok_or(NOT_FOUND)?;
        let title = object.state.title.clone().ok_or(BAD_ABI)?;
        Ok(PropertySnapshot { id,
                              owner,
                              key,
                              spec: object.state.spec,
                              title,
                              definition: object.definition.retain_source() })
    }

    pub(crate) fn install_properties(&mut self, value: &PreparedProperties) -> Result<u64, Status>
    {
        let s = &value.snapshot;
        let object = self.get(s.id, s.owner)?;
        if self.objects.completion_key(s.id) != Some(s.key)
           || object.state.spec != s.spec
           || !object.state.title.as_ref().is_some_and(|old| Arc::ptr_eq(old, &s.title))
        {
            return Err(SERVICE_BUSY);
        }
        if Arc::ptr_eq(&value.title, &s.title)
        {
            return Ok(s.title.revision);
        }
        // The detached snapshot still pins the old title and its property bytes. Its
        // last release occurs after Desktop is restored, never under this swap.
        let object = self.objects.get_mut(s.id).ok_or(NOT_FOUND)?;
        object.state.title = Some(value.title.clone());
        if value.relayout
        {
            if let Some(layout) = &mut object.state.layout
            {
                layout.measured = None;
                layout.intrinsic = None;
                layout.arrangement_pending = true;
            }
            self.schedule_layout(s.id);
        }
        Ok(value.title.revision)
    }
}
