//! Typed views from the same property inventory as the resident store and SDK.
use mac_abi::{controls::suite::{self as model, Model},
              definitions::ControlDefinitionState};

use super::Family;
use crate::{Error, Rect, Result};
macro_rules! families {
 ($(
 ($lt:lifetime,$name:ident,$tag:ident,$family:ident,$entry:ident,$constructor:ident,$properties:ident,$reference:ident,$id:literal,$variant:ident){
  $(($index:literal,$field:ident,$setter:ident,$marker:ident,[$ty:ty],$getter:ident,[$default:expr],$default_kind:ident)),*$(,)?
 }=>$validate:expr;
 )*)=>{$(
  #[derive(Clone,Copy)]pub struct $name<$lt>{pub bounds:Rect,pub enabled:bool,pub highlighted:u32,pub variation:i16,pub caption:&$lt str,pub properties:<model::$tag as Model>::View<$lt>}
  impl<$lt> core::ops::Deref for $name<$lt>{type Target=<model::$tag as Model>::View<$lt>;fn deref(&self)->&Self::Target{&self.properties}}
  impl crate::stub::View for $name<'_>{fn bounds(&self)->Rect{self.bounds}fn limits(&self)->(model::Dimensions,Option<model::Dimensions>){(self.minimum,self.maximum)}}
  pub(crate) struct $family;
  impl Family for $family{
   type Wire=ControlDefinitionState;type View<$lt>=$name<$lt>;const ID:i16=$id;const STUB:bool=true;const TRACK:bool=true;const DELTA:bool=true;
   fn variation(value:i16)->Result<()>{model::$tag::instance(value,&[]).map(|_|()).map_err(|_|Error::BadArgument)}
   fn decode<$lt>(wire:&$lt Self::Wire,variation:i16)->Result<Self::View<$lt>>{Self::decode_properties(wire,variation,&[])}
   fn decode_properties<$lt>(wire:&$lt Self::Wire,variation:i16,bytes:&$lt[u8])->Result<Self::View<$lt>>{
    if wire.bounds.right<wire.bounds.left||wire.bounds.bottom<wire.bounds.top||wire.enabled>1||wire.flags!=0||wire.value!=0||wire.minimum!=0||!matches!(wire.maximum,0|1){return Err(Error::BadArgument)}
    let length=wire.title_length as usize;if length>wire.title.len(){return Err(Error::BadArgument)}let caption=core::str::from_utf8(&wire.title[..length]).map_err(|_|Error::BadArgument)?;
    // The original fixed-width popup variation carries the currently displayed
    // menu label in its retained legacy caption. Other stubs use typed fields.
    if !caption.is_empty()&&!($id==25&&variation==1){return Err(Error::BadArgument)}
    let properties=model::$tag::instance(variation,bytes).map_err(|_|Error::BadArgument)?;
    Ok($name{bounds:wire.bounds,enabled:wire.enabled!=0,highlighted:wire.highlighted,variation,caption,properties})
   }
   fn message<$lt>(wire:&$lt ControlDefinitionState,variation:i16,properties:&$lt[u8])->Result<Self::View<$lt>>{Self::decode_properties(wire,variation,properties)}
   fn bounds(state:&Self::View<'_>)->Rect{state.bounds}
   fn change<$lt>(before:&$lt ControlDefinitionState,after:&$lt ControlDefinitionState,variation:i16,properties:&$lt[u8])->Result<(Self::View<$lt>,Self::View<$lt>)>{Ok((Self::decode_properties(before,variation,properties)?,Self::decode_properties(after,variation,properties)?))}
  }
 )*};
}
model::control_suite_inventory!(families);
