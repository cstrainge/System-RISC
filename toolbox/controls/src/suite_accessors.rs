//! Typed owners/accessors expand from the same model/property declarations used
//! by resident validation and CDEF views. There is no second field-index table.
use mac_abi::controls::suite as model;
use mac_control_state::Properties;

use super::{Property, SuiteControl, SuiteRef, copy_string};
use crate::{Controls, Rect, Reflow, Result, WindowId};
macro_rules! getter {
    (scalar,$method:ident,$marker:ident,[$ty:ty]) => {
        pub fn $method(self) -> Result<$ty>
        {
            let field = self.property::<model::fields::$marker>()?;
            field.get()
        }
    };
    (text,$method:ident,$marker:ident,[$ty:ty]) => {
        pub fn $method(self) -> Result<alloc::string::String>
        {
            let field = self.property::<model::fields::$marker>()?;
            copy_string(field.get()?)
        }
    };
    (optional_text,$method:ident,$marker:ident,[$ty:ty]) => {
        pub fn $method(self) -> Result<Option<alloc::string::String>>
        {
            let field = self.property::<model::fields::$marker>()?;
            field.get()?.map(copy_string).transpose()
        }
    };
    (borrowed,$method:ident,$marker:ident,[$ty:ty]) => {
        pub fn $method(self) -> Result<Property<model::fields::$marker>>
        {
            self.property::<model::fields::$marker>()
        }
    };
}
macro_rules! accessors {
 ($(
 ($lt:lifetime,$name:ident,$tag:ident,$family:ident,$entry:ident,$constructor:ident,$properties:ident,$reference:ident,$id:literal,$variant:ident){
  $(($index:literal,$field:ident,$setter:ident,$marker:ident,[$ty:ty],$getter:ident,[$default:expr],$default_kind:ident)),*$(,)?
 }=>$validate:expr;
 )*)=>{$(
  pub type $name<'client>=SuiteControl<'client,model::$tag>;
  pub type $reference<'client>=SuiteRef<'client,model::$tag>;
  pub type $properties=Properties<model::$tag>;
  impl<'client> Controls<'client>{pub fn $constructor(&self,window:WindowId,bounds:Rect,properties:&$properties,reflow:Reflow)->Result<$name<'client>>{self.create_suite(window,bounds,properties,reflow)}}
  impl<'client> SuiteRef<'client,model::$tag>{$(getter!($getter,$field,$marker,[$ty]);pub fn $setter<$lt>(self,value:$ty)->Result<()>{self.set_property::<model::fields::$marker>(value)})*}
 )*};
}
model::control_suite_inventory!(accessors);
