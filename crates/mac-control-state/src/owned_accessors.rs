//! Every owning configuration accessor follows the same shared model inventory.
use super::*;
macro_rules! accessors {
 ($(
 ($lt:lifetime,$name:ident,$tag:ident,$family:ident,$entry:ident,$constructor:ident,$properties:ident,$reference:ident,$id:literal,$variant:ident){
  $(($index:literal,$field:ident,$setter:ident,$marker:ident,[$ty:ty],$getter:ident,[$default:expr],$default_kind:ident)),*$(,)?
 }=>$validate:expr;
 )*)=>{$(impl Properties<mac_control_model::$tag>{$(pub fn $field<$lt>(&$lt self)->Result<$ty>{Ok(self.view()?.$field)}pub fn $setter<$lt>(&mut self,value:$ty)->Result<()>{self.set($index,&value)})*})*};
}
mac_control_model::control_suite_inventory!(accessors);
