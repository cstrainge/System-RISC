//! Full property API for the V2 control stubs. Data getters copy the actual
//! resident value; setters update one field atomically against current state.
//! Behavior remains deliberately ignored until each family is implemented.
use alloc::{string::String, vec::Vec};
use core::{marker::PhantomData, ops::Deref};

pub use mac_abi::controls::suite as model;
use mac_abi::controls::{ALL_PROPERTIES, PropertiesCall, SET_PROPERTIES};
pub use mac_control_state as owned;
use mac_control_state::{Buffer as Bytes, Properties};
use model::{Cursor, Field, Model, Record};

use super::*;
#[path = "suite_accessors.rs"]
mod accessors;
pub use accessors::*;
fn error(e: model::Error) -> Error
{
    match e
    {
        model::Error::BadArgument => Error::BadArgument,
        model::Error::NoMemory => Error::NoMemory
    }
}
fn copy_string(value: &str) -> Result<String>
{
    let mut out = String::new();
    out.try_reserve_exact(value.len()).map_err(|_| Error::NoMemory)?;
    out.push_str(value);
    Ok(out)
}
/// An owned field result lends its decoded collections/strings only while this
/// guard is alive. It is a snapshot of a GET, never retained author state.
pub struct Property<F: Field>
{
    bytes:  Vec<u8>,
    marker: PhantomData<F>
}
impl<F: Field> Property<F>
{
    pub fn get(&self) -> Result<F::Value<'_>>
    {
        let mut input = Cursor::new(&self.bytes);
        let value = input.field().map_err(error)?;
        input.finish().map_err(error)?;
        Ok(value)
    }
}
pub struct SuiteControl<'a, M: Model>
{
    owner:     Control<'a>,
    reference: SuiteRef<'a, M>
}
impl<'a, M: Model> Deref for SuiteControl<'a, M>
{
    type Target = SuiteRef<'a, M>;

    fn deref(&self) -> &Self::Target
    {
        &self.reference
    }
}
pub struct SuiteRef<'a, M: Model>
{
    control: ControlRef<'a>,
    client:  Client<'a>,
    marker:  PhantomData<M>
}
impl<'a, M: Model> Clone for SuiteRef<'a, M>
{
    fn clone(&self) -> Self
    {
        *self
    }
}
impl<'a, M: Model> Copy for SuiteRef<'a, M>
{
}
impl<'a, M: Model> SuiteControl<'a, M>
{
    pub fn control(&self) -> SuiteRef<'a, M>
    {
        self.reference
    }

    pub fn id(&self) -> ControlId
    {
        self.owner.id()
    }

    /// Transfer this genuine control owner into a managed parent/container.
    pub fn into_control(self) -> Control<'a>
    {
        self.owner
    }
}
impl<'a, M: Model> SuiteRef<'a, M>
{
    pub fn id(self) -> ControlId
    {
        self.control.id()
    }

    pub fn raw(self) -> ControlRef<'a>
    {
        self.control
    }

    pub fn bounds(self) -> Result<Rect>
    {
        Ok(self.control.info()?.standard.bounds)
    }

    pub fn enabled(self) -> Result<bool>
    {
        Ok(self.control.info()?.standard.enabled != 0)
    }

    pub fn set_enabled(self, enabled: bool) -> Result<()>
    {
        let mut call = mac_abi::controls::ControlCall { id: self.id().0,
                                                        operation:
                                                            mac_abi::controls::INFO_OBJECT,
                                                        ..Default::default() };
        self.client.control(&mut call)?;
        if call.procedure >> 4 != M::ID
        {
            return Err(Error::BadArgument);
        }
        let kind = StandardKind::try_from(call.procedure)?;
        let mut state = call.standard;
        state.enabled = u32::from(enabled);
        self.control.configure(kind, state).map(|_| ())
    }

    fn read(self, index: u32) -> Result<Vec<u8>>
    {
        let mut call = PropertiesCall { id: self.id().0,
                                        field: index,
                                        ..Default::default() };
        match self.client.control_properties(&mut call, &[], &mut [])
        {
            Ok(()) | Err(Error::NoMemory) => (),
            Err(e) => return Err(e)
        }
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(call.required).map_err(|_| Error::NoMemory)?;
        bytes.resize(call.required, 0);
        self.client.control_properties(&mut call, &[], &mut bytes)?;
        if call.required != bytes.len()
        {
            return Err(Error::BadAbi);
        }
        Ok(bytes)
    }

    /// Deliver a borrowed request to the actual retained CDEF. Stub definitions
    /// accept it with no behavioral effects and never emit notifications.
    pub fn send_message(self, request: model::messages::Request<'_>) -> Result<()>
    {
        use model::Codec;
        let mut bytes = Bytes::new();
        request.encode(&mut bytes).map_err(error)?;
        let mut call = PropertiesCall { id: self.id().0,
                                        operation: mac_abi::controls::SEND_MESSAGE,
                                        ..Default::default() };
        self.client.control_properties(&mut call, bytes.as_slice(), &mut [])
    }

    pub fn properties(self) -> Result<Properties<M>>
    {
        Properties::from_bytes(&self.read(ALL_PROPERTIES)?).map_err(error)
    }

    pub fn configure(self, value: &Properties<M>) -> Result<()>
    {
        value.view().map_err(error)?;
        let mut call = PropertiesCall { id: self.id().0,
                                        operation: SET_PROPERTIES,
                                        ..Default::default() };
        self.client.control_properties(&mut call, value.as_bytes(), &mut [])
    }

    pub fn property<F: Field<Model = M>>(self) -> Result<Property<F>>
    {
        Ok(Property { bytes:  self.read(F::INDEX as u32)?,
                      marker: PhantomData })
    }

    pub fn set_property<'b, F: Field<Model = M>>(self, value: F::Value<'b>) -> Result<()>
    {
        let mut bytes = Bytes::new();
        model::field(&value, &mut bytes).map_err(error)?;
        let mut call = PropertiesCall { id: self.id().0,
                                        operation: SET_PROPERTIES,
                                        field: F::INDEX as u32,
                                        ..Default::default() };
        self.client.control_properties(&mut call, bytes.as_slice(), &mut [])
    }
}
impl<'a> Controls<'a>
{
    /// Construct another typed view of the actual resident object. The native
    /// manager validates its owner/generation on every subsequent operation.
    pub fn suite_ref<M: Model>(&self, id: ControlId) -> Result<SuiteRef<'a, M>>
    {
        let mut call = mac_abi::controls::ControlCall { id: id.0,
                                                        operation:
                                                            mac_abi::controls::INFO_OBJECT,
                                                        ..Default::default() };
        self.client.control(&mut call)?;
        if call.procedure >> 4 != M::ID
        {
            return Err(Error::BadArgument);
        }
        Ok(SuiteRef { control: self.control(id),
                      client:  self.client,
                      marker:  PhantomData })
    }

    pub fn create_suite<M: Model>(&self, window: WindowId, bounds: Rect,
                                  properties: &Properties<M>, reflow: Reflow)
                                  -> Result<SuiteControl<'a, M>>
    {
        properties.view().map_err(error)?;
        let variation = if M::ID == 17
        {
            model::TextEdit::read(properties.as_bytes()).map_err(error)?.variant as i16
        }
        else
        {
            0
        };
        let kind = StandardKind::try_from((M::ID << 4) | variation)?;
        let owner = self.create_standard(window,
                                         kind,
                                         StandardState { bounds,
                                                         ..Default::default() },
                                         reflow)?;
        let reference = SuiteRef { control: owner.control(),
                                   client:  self.client,
                                   marker:  PhantomData };
        // Failed configuration drops the original genuine Control owner. No partial
        // client object escapes or silently substitutes its own property cache.
        reference.configure(properties)?;
        Ok(SuiteControl { owner,
                          reference })
    }
}
