//! Property vocabulary. Opaque source keys are state, never authority to read a
//! file, invoke a callback or borrow another control. The stubs do none of those.
use crate::{Codec, Cursor, Error, List, Result, Sink};
macro_rules! enumeration {
 ($name:ident { $($variant:ident = $value:literal),+ $(,)? }) => {
  #[derive(Clone,Copy,Debug,PartialEq,Eq)] pub enum $name {$($variant),+}
  impl<'a> Codec<'a> for $name {
   fn decode(input:&mut Cursor<'a>)->Result<Self>{match u8::decode(input)?{$($value=>Ok(Self::$variant),)+_=>Err(Error::BadArgument)}}
   fn encode(&self,output:&mut dyn Sink)->Result<()>{match self{$(Self::$variant=>$value as u8,)+}.encode(output)}
  }
 };
}
enumeration!(Voice { Chicago=0,Geneva=1 });
enumeration!(Orientation { Horizontal=0,Vertical=1 });
enumeration!(ThemeMetric { Theme=0 });
enumeration!(RuleStyle { Simple=0,Etched=1 });
enumeration!(LayoutLength { Fill=0 });
enumeration!(Scaling { Natural=0,Fit=1,Fill=2 });
enumeration!(Alignment { Center=0,Leading=1,Trailing=2,Top=3,Bottom=4 });
enumeration!(ActivityMode { Indeterminate=0,Determinate=1 });
enumeration!(Directions { Neither=0,Backward=1,Forward=2,Both=3 });
enumeration!(RepeatPolicy { System=0 });
enumeration!(NumericKind { Integer=0,Decimal=1 });
enumeration!(DisclosureState { Collapsed=0,Expanded=1 });
enumeration!(TriangleLook { Filled=0 });
enumeration!(MeterMode { Scalar=0,Segments=1 });
enumeration!(RegionRole { Normal=0,Caution=1,Reserved=2 });
enumeration!(DateMode { Date=0,Time=1,DateTime=2 });
enumeration!(AmPm { Present=0,Am=1,Pm=2 });
enumeration!(WellLook { Theme=0 });
enumeration!(SelectionMode { Single=0,Multiple=1 });
enumeration!(CellArrangement { Classic=0 });
enumeration!(IconSize { Theme=0,Large=1,Small=2 });
enumeration!(LabelOverflow { Wrap=0,Ellipsis=1 });
enumeration!(Arrangement { Grid=0,Free=1 });
enumeration!(GridConstraint { Both=0,X=1,Y=2 });
enumeration!(DataMode { Owned=0,View=1 });
enumeration!(GraphMode { Line=0,Sparkline=1,StackedArea=2,Bar=3 });
enumeration!(AxisPolicy { Auto=0,Fixed=1 });
enumeration!(EditVariant { Text=0,View=1,Password=2 });
enumeration!(LineFilter { None=0,Classic=1,Unix=2,Windows=3 });
enumeration!(PictureKind { Empty=0,Pict=1,Gfx=2 });
macro_rules! structure {
 ($name:ident {$($field:ident:$ty:ty),+$(,)?}) => {
  #[derive(Clone,Copy)] pub struct $name {$(pub $field:$ty),+}
  impl<'a> Codec<'a> for $name {
   fn decode(input:&mut Cursor<'a>)->Result<Self>{Ok(Self{$($field:<$ty>::decode(input)?),+})}
   fn encode(&self,output:&mut dyn Sink)->Result<()>{$ (self.$field.encode(output)?;)+Ok(())}
  }
 };
 ($name:ident<'a> {$($field:ident:$ty:ty),+$(,)?}) => {
  #[derive(Clone,Copy)] pub struct $name<'a> {$(pub $field:$ty),+}
  impl<'a> Codec<'a> for $name<'a> {
   fn decode(input:&mut Cursor<'a>)->Result<Self>{Ok(Self{$($field:<$ty>::decode(input)?),+})}
   fn encode(&self,output:&mut dyn Sink)->Result<()>{$ (self.$field.encode(output)?;)+Ok(())}
  }
 };
}
structure!(Dimensions { width:  u32,
                        height: u32 });
structure!(Source { key:      u64,
                    revision: u64,
                    count:    u64 });
impl Source
{
    pub const fn empty() -> Self
    {
        Self { key:      0,
               revision: 0,
               count:    0 }
    }
}
structure!(Child { id: u64 });
impl Child
{
    pub const fn empty() -> Self
    {
        Self { id: 0 }
    }
}
structure!(Region { offset: f64,
                    span:   f64,
                    role:   RegionRole });
structure!(SortKey { column:    u64,
                     ascending: bool });
structure!(SampleWindow { offset: u64,
                          span:   u64 });
impl SampleWindow
{
    pub const fn empty() -> Self
    {
        Self { offset: 0,
               span:   0 }
    }
}
structure!(SeriesStyle { series: u64,
                         role:   u32,
                         fill:   bool,
                         stroke: bool });
structure!(Sample {time:i64,value:Option<f64>});
structure!(Segment<'a> {key:u64,icon:Option<u64>,label:Option<&'a str>,tooltip:&'a str});
structure!(MeterSegment<'a> {key:u64,label:&'a str,offset:f64,span:f64,role:RegionRole});
structure!(Column<'a> {key:u64,title:&'a str,width:u32,minimum:u32,maximum:Option<u32>,auto_size:bool,visible:bool,header_segment:Child,divider:Child});
structure!(TabItem<'a> {key:u64,title:&'a str,view:Child});
structure!(PathSegment<'a> {key:u64,title:&'a str,icon:Option<u64>,menu_items:Source,drop_menu:Child});
structure!(Series<'a> {key:u64,name:&'a str,samples:List<'a,Sample>});
structure!(TextChild<'a> {child:Child,text:&'a str,voice:Voice});
impl<'a> TextChild<'a>
{
    pub const fn empty() -> Self
    {
        Self { child: Child::empty(),
               text:  "",
               voice: Voice::Chicago }
    }
}
structure!(SearchConfig<'a> {text:&'a str,placeholder:&'a str,magnifier_visible:bool,clear_visible:bool});
impl<'a> SearchConfig<'a>
{
    pub const fn list_edit() -> Self
    {
        Self { text:              "",
               placeholder:       "",
               magnifier_visible: false,
               clear_visible:     false }
    }
}
#[derive(Clone, Copy)]
pub struct Bytes<'a>(pub &'a [u8]);
impl<'a> Codec<'a> for Bytes<'a>
{
    fn decode(input: &mut Cursor<'a>) -> Result<Self>
    {
        let length = u32::decode(input)? as usize;
        Ok(Self(input.take(length)?))
    }

    fn encode(&self, output: &mut dyn Sink) -> Result<()>
    {
        u32::try_from(self.0.len()).map_err(|_| Error::BadArgument)?.encode(output)?;
        output.bytes(self.0)
    }
}
structure!(PictureData<'a> {kind:PictureKind,data:Bytes<'a>,width:u32,height:u32,source:Source});
impl<'a> PictureData<'a>
{
    pub const fn empty() -> Self
    {
        Self { kind:   PictureKind::Empty,
               data:   Bytes(&[]),
               width:  0,
               height: 0,
               source: Source::empty() }
    }
}
#[derive(Clone, Copy)]
pub enum Number
{
    Integer(i64),
    Decimal(f64)
}
impl Number
{
    pub fn kind(self) -> NumericKind
    {
        match self
        {
            Self::Integer(_) => NumericKind::Integer,
            Self::Decimal(_) => NumericKind::Decimal
        }
    }

    pub fn compare(self, other: Self) -> Result<core::cmp::Ordering>
    {
        match (self, other)
        {
            (Self::Integer(a), Self::Integer(b)) => Ok(a.cmp(&b)),
            (Self::Decimal(a), Self::Decimal(b)) => a.partial_cmp(&b).ok_or(Error::BadArgument),
            _ => Err(Error::BadArgument)
        }
    }

    pub fn positive(self) -> bool
    {
        match self
        {
            Self::Integer(x) => x > 0,
            Self::Decimal(x) => x.is_finite() && x > 0.0
        }
    }
}
impl<'a> Codec<'a> for Number
{
    fn decode(input: &mut Cursor<'a>) -> Result<Self>
    {
        match u8::decode(input)?
        {
            0 => Ok(Self::Integer(i64::decode(input)?)),
            1 => Ok(Self::Decimal(f64::decode(input)?)),
            _ => Err(Error::BadArgument)
        }
    }

    fn encode(&self, output: &mut dyn Sink) -> Result<()>
    {
        match self
        {
            Self::Integer(x) =>
            {
                0u8.encode(output)?;
                x.encode(output)
            }
            Self::Decimal(x) =>
            {
                1u8.encode(output)?;
                x.encode(output)
            }
        }
    }
}
#[derive(Clone, Copy)]
pub enum DatePart
{
    Present,
    Number(u16)
}
impl<'a> Codec<'a> for DatePart
{
    fn decode(i: &mut Cursor<'a>) -> Result<Self>
    {
        match u8::decode(i)?
        {
            0 => Ok(Self::Present),
            1 => Ok(Self::Number(u16::decode(i)?)),
            _ => Err(Error::BadArgument)
        }
    }

    fn encode(&self, o: &mut dyn Sink) -> Result<()>
    {
        match self
        {
            Self::Present => 0u8.encode(o),
            Self::Number(x) =>
            {
                1u8.encode(o)?;
                x.encode(o)
            }
        }
    }
}
#[derive(Clone, Copy)]
pub enum YearRange
{
    Present,
    Range(i32, i32)
}
impl<'a> Codec<'a> for YearRange
{
    fn decode(i: &mut Cursor<'a>) -> Result<Self>
    {
        match u8::decode(i)?
        {
            0 => Ok(Self::Present),
            1 =>
            {
                let a = i32::decode(i)?;
                let b = i32::decode(i)?;
                if a > b
                {
                    return Err(Error::BadArgument);
                }
                Ok(Self::Range(a, b))
            }
            _ => Err(Error::BadArgument)
        }
    }

    fn encode(&self, o: &mut dyn Sink) -> Result<()>
    {
        match self
        {
            Self::Present => 0u8.encode(o),
            Self::Range(a, b) =>
            {
                if a > b
                {
                    return Err(Error::BadArgument);
                }
                1u8.encode(o)?;
                a.encode(o)?;
                b.encode(o)
            }
        }
    }
}
structure!(DateAndTime { year:   i32,
                         month:  u8,
                         day:    u8,
                         hour:   u8,
                         minute: u8,
                         second: u8 });
impl DateAndTime
{
    pub fn valid(self) -> bool
    {
        let leap = self.year % 4 == 0 && (self.year % 100 != 0 || self.year % 400 == 0);
        let days = match self.month
        {
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            4 | 6 | 9 | 11 => 30,
            2 =>
            {
                if leap
                {
                    29
                }
                else
                {
                    28
                }
            }
            _ => 0
        };
        self.year >= 1
        && self.day >= 1
        && self.day <= days
        && self.hour < 24
        && self.minute < 60
        && self.second < 60
    }
}
#[derive(Clone, Copy)]
pub enum DateValue
{
    Present,
    Value(DateAndTime)
}
impl<'a> Codec<'a> for DateValue
{
    fn decode(i: &mut Cursor<'a>) -> Result<Self>
    {
        match u8::decode(i)?
        {
            0 => Ok(Self::Present),
            1 =>
            {
                let value = DateAndTime::decode(i)?;
                if !value.valid()
                {
                    return Err(Error::BadArgument);
                }
                Ok(Self::Value(value))
            }
            _ => Err(Error::BadArgument)
        }
    }

    fn encode(&self, o: &mut dyn Sink) -> Result<()>
    {
        match self
        {
            Self::Present => 0u8.encode(o),
            Self::Value(x) =>
            {
                if !x.valid()
                {
                    return Err(Error::BadArgument);
                }
                1u8.encode(o)?;
                x.encode(o)
            }
        }
    }
}
pub(crate) fn common(minimum: Dimensions, maximum: Option<Dimensions>) -> Result<()>
{
    // These are local reflow sizes, carried by geometry::Dimensions on the
    // actual native wire (signed 16-bit). Document/data extents have separate
    // wide types; accepting a larger control minimum would fail at AskSize.
    let limit = i16::MAX as u32;
    if minimum.width == 0
       || minimum.height == 0
       || minimum.width > limit
       || minimum.height > limit
       || maximum.is_some_and(|m| {
                     m.width < minimum.width
                     || m.height < minimum.height
                     || m.width > limit
                     || m.height > limit
                 })
    {
        Err(Error::BadArgument)
    }
    else
    {
        Ok(())
    }
}
pub(crate) fn numeric(value: Number, min: Number, max: Number, step: Number) -> Result<()>
{
    use core::cmp::Ordering;
    if min.compare(max)? == Ordering::Greater
       || value.compare(min)? == Ordering::Less
       || value.compare(max)? == Ordering::Greater
       || !step.positive()
       || step.kind() != value.kind()
    {
        Err(Error::BadArgument)
    }
    else
    {
        Ok(())
    }
}
