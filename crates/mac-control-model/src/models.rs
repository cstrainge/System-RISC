//! All model views and their atomic typed replacement codecs expand from the
//! same inventory used by definition shims and userspace wrapper accessors.
use crate::{Cursor, Error, Model, Record, Result, Sink, field};
#[derive(Clone, Copy)]
pub struct Descriptor
{
    pub name:   &'static str,
    pub id:     i16,
    pub entry:  &'static str,
    pub fields: &'static [&'static str]
}
macro_rules! instance {
 (edit,$name:ident,$lt:lifetime)=>{
  fn instance<$lt>(variation:i16,bytes:&$lt [u8])->Result<Self::View<$lt>>{
   if !(0..=2).contains(&variation){return Err(Error::BadArgument)}
   let mut value=$name::read(bytes)?;
   if bytes.is_empty(){value.variant=match variation{0=>crate::EditVariant::Text,1=>crate::EditVariant::View,_=>crate::EditVariant::Password}}
   if value.variant as i16!=variation{return Err(Error::BadArgument)}Ok(value)
  }
 };
 ($mode:ident,$name:ident,$lt:lifetime)=>{};
}
macro_rules! models {
 ($(
 ($lt:lifetime,$name:ident,$tag:ident,$family:ident,$entry:ident,$constructor:ident,$properties:ident,$reference:ident,$id:literal,$variant:ident){
  $(($index:literal,$field:ident,$setter:ident,$marker:ident,[$ty:ty],$getter:ident,[$default:expr],$default_kind:ident)),*$(,)?
 }=>$validate:expr;
 )*)=>{
  $(
   #[derive(Clone,Copy)]pub struct $name<$lt>{$(#[doc=concat!(stringify!($default_kind)," default: ",stringify!($default))]pub $field:$ty,)*}
   impl<$lt> Default for $name<$lt>{fn default()->Self{Self{$($field:$default,)*}}}
   impl<$lt> Record<$lt> for $name<$lt>{
    fn read(bytes:&$lt [u8])->Result<Self>{if bytes.is_empty(){return Ok(Self::default())}let mut input=Cursor::new(bytes);let value=Self{$($field:input.field::<$ty>()?,)*};input.finish()?;value.validate()?;Ok(value)}
    fn write(&self,output:&mut dyn Sink)->Result<()>{self.validate()?;$(field(&self.$field,output)?;)*Ok(())}
    fn validate(&self)->Result<()>{($validate)(self)}
   }
   pub enum $tag{}
   impl Model for $tag{
    type View<$lt>=$name<$lt>;const ID:i16=$id;const NAME:&'static str=stringify!($name);const FIELDS:&'static[&'static str]=&[$(stringify!($field),)*];
    instance!($variant,$name,$lt);
    fn replace<$lt>(original:&$lt [u8],index:usize,input:&$lt [u8],output:&mut dyn Sink)->Result<()>{
     let mut value=$name::read(original)?;let mut cursor=Cursor::new(input);match index{$($index=>value.$field=cursor.field::<$ty>()?,)*_=>return Err(Error::BadArgument)}cursor.finish()?;
     // Validation finishes before the first allocating output write. An invalid
     // range/UTF-8/enum is not disguised as an allocation failure.
     value.validate()?;value.write(output)
    }
   }
  )*
  pub const CONTROLS:&[Descriptor]=&[$(Descriptor{name:stringify!($name),id:$id,entry:stringify!($entry),fields:$tag::FIELDS},)*];
  pub fn validate(id:i16,bytes:&[u8])->Result<()>{match id{$($id=>$name::read(bytes).map(|_|()),)*_=>Err(Error::BadArgument)}}
  pub fn field_names(id:i16)->Option<&'static[&'static str]>{match id{$($id=>Some($tag::FIELDS),)*_=>None}}
  pub fn write_defaults(id:i16,output:&mut dyn Sink)->Result<()>{match id{$($id=>$name::default().write(output),)*_=>Err(Error::BadArgument)}}
  pub fn replace_property(id:i16,original:&[u8],index:usize,input:&[u8],output:&mut dyn Sink)->Result<()>{match id{$($id=>$tag::replace(original,index,input,output),)*_=>Err(Error::BadArgument)}}
 };
}
crate::control_suite_inventory!(models);
