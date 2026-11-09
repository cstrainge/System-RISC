//! Property identifiers and indices are generated from the actual model order.
use crate::*;
macro_rules! fields {
 ($(
 ($lt:lifetime,$name:ident,$tag:ident,$family:ident,$entry:ident,$constructor:ident,$properties:ident,$reference:ident,$id:literal,$variant:ident){
  $(($index:literal,$field:ident,$setter:ident,$marker:ident,[$ty:ty],$getter:ident,[$default:expr],$default_kind:ident)),*$(,)?
 }=>$validate:expr;
 )*)=>{$($(pub enum $marker{}impl Field for $marker{type Model=$tag;type Value<$lt>=$ty;const INDEX:usize=$index;})*)*};
}
crate::control_suite_inventory!(fields);
