
use alloc::rc::Rc;

use mac_abi::{ icons::IconSet, files::{ CatalogInfo, VolumeInfo }};
use mac_toolbox::{ System, ToolboxResult, TypeCode };



pub struct IconCache<'a>
{
    _system: System<'a>
}


impl<'a> IconCache<'a>
{
    pub fn new(system: System<'a>) -> Self
    {
        Self
        {
            _system: system
        }
    }

    pub fn resolve_icon_for_file(&mut self,
                                 _volume: &VolumeInfo,
                                 _entry: &CatalogInfo) -> ToolboxResult<Rc<IconSet>>
    {
        unimplemented!()
    }

    pub fn resolve_icon_for_type(&mut self,
                                 _creator: TypeCode,
                                 _file_type: TypeCode) -> ToolboxResult<Rc<IconSet>>
    {
        unimplemented!()
    }
}
