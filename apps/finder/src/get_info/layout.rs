//! Native resource descriptor projected from original fvew child rectangles.
use mac_toolbox::{Error, *};
pub const WIDTH: i16 = 242;
pub const FONT: quickdraw::fonts::FontSelection = quickdraw::fonts::FontSelection { family: 1,
                                                                                    size:   9,
                                                                                    face:   0 };
#[derive(Clone, Copy)]
pub struct Layout
{
    pub width:       i16,
    pub height:      i16,
    pub name:        Rect,
    pub description: Rect,
    pub kind:        Rect,
    pub size:        Rect,
    pub location:    Rect,
    pub created:     Rect,
    pub modified:    Rect,
    pub version:     Rect,
    pub comments:    Rect,
    pub icon:        Rect,
    pub locked:      Rect,
    pub stationery:  Rect
}
impl Layout
{
    pub fn load(system: &System<'_>, folder: bool, document: bool, volume: bool)
                -> Result<Self, Error>
    {
        let id = if volume
        {
            11511
        }
        else if folder
        {
            11261
        }
        else if document
        {
            17011
        }
        else
        {
            12761
        };
        let resource = system.resources()
                             .application()
                             .get(ResourceKey { kind: mac_toolbox::TypeCode::new(*b"gInf"),
                                                id })?;
        let mut bytes = [0; 104];
        let read = resource.read(0, &mut bytes);
        let released = resource.release();
        let read = read?;
        released?;
        if read != bytes.len() || &bytes[..4] != b"GIn1"
        {
            return Err(Error::BadArgument);
        }
        let word = |at| i16::from_be_bytes([bytes[at], bytes[at + 1]]);
        let rect = |index: usize| {
            let at = 8 + index * 8;
            Rect::new(word(at + 2), word(at), word(at + 6), word(at + 4))
        };
        let layout = Self { width:       word(4),
                            height:      word(6),
                            name:        rect(0),
                            description: rect(1),
                            kind:        rect(2),
                            size:        rect(3),
                            location:    rect(4),
                            created:     rect(5),
                            modified:    rect(6),
                            version:     rect(7),
                            comments:    rect(8),
                            icon:        rect(9),
                            locked:      rect(10),
                            stationery:  rect(11) };
        if layout.width != WIDTH || layout.height != if folder { 217 } else { 261 }
        {
            return Err(Error::BadArgument);
        }
        for bounds in [layout.name, layout.description, layout.kind, layout.size, layout.location,
                       layout.created, layout.modified, layout.version, layout.comments,
                       layout.icon, layout.locked, layout.stationery]
        {
            if bounds.left < 0
               || bounds.top < 0
               || bounds.right > layout.width
               || bounds.bottom > layout.height
               || bounds.right < bounds.left
               || bounds.bottom < bounds.top
            {
                return Err(Error::BadArgument);
            }
        }
        Ok(layout)
    }
}
pub fn contains(bounds: Rect, point: Point) -> bool
{
    point.h >= bounds.left
    && point.h < bounds.right
    && point.v >= bounds.top
    && point.v < bounds.bottom
}
pub fn inset(bounds: Rect, n: i16) -> Rect
{
    Rect::new(bounds.left + n, bounds.top + n, bounds.right - n, bounds.bottom - n)
}
