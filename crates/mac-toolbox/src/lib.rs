//! Rust Toolbox facade over the resident System trap gate.
#![no_std]
pub use mac_abi::{ACCESS_DENIED, ACTIVE_FLAG, ALREADY_EXISTS, ApplicationEntry, BAD_ABI,
                  BAD_ARGUMENT, CAP_EVENT_WAIT, CAP_GRAPHICS, CAP_MONOTONIC_CLOCK,
                  DIRECTORY_NOT_EMPTY, DISK_FULL, DRAW_BUTTON, DRAW_TEXT, Event, EventKind,
                  FRAME_RECT, IO_ERROR, NO_MEMORY, NOT_FOUND, NOT_SUPPORTED, OK, PAINT_RECT,
                  Point, READ_ONLY, ROLLBACK_FAILED, Rect, ResourceKey, SCAN_STALE, SERVICE_BUSY,
                  Status, SystemServices, TRANSLATION_REQUIRED, WRONG_THREAD, WindowId,
                  WindowRequest, dates::Timestamp};
pub use mac_traps::{Client, Error, Result as ToolboxResult};
pub mod panic;
pub use toolbox_controls as controls;
pub use toolbox_crashes as crashes;
pub use toolbox_dialogs as dialogs;
pub use toolbox_events as events;
pub use toolbox_files as files;
pub use toolbox_memory as memory;
pub use toolbox_menus as menus;
pub use toolbox_processes as processes;
pub use toolbox_quickdraw as quickdraw;
pub use toolbox_resources as resources;
pub use toolbox_scrap as scrap;
pub use toolbox_textedit as textedit;
pub use toolbox_windows as windows;


#[derive(Clone, Copy)]
pub struct System<'a>
{
    client: Client<'a>
}
impl<'a> System<'a>
{
    /// # Safety
    /// The loader-provided table and its advertised callbacks must remain valid.
    pub unsafe fn from_raw(services: *const SystemServices) -> core::result::Result<Self, Status>
    {
        unsafe { Client::from_raw(services) }.map(|client| Self { client }).map_err(|e| e.0)
    }

    pub fn client(&self) -> Client<'a>
    {
        self.client
    }

    pub fn processes(&self) -> processes::Processes<'a>
    {
        processes::Processes::new(self.client)
    }

    pub fn windows(&self) -> windows::Windows<'a>
    {
        windows::Windows::new(self.client)
    }

    pub fn quickdraw(&self) -> quickdraw::QuickDraw<'a>
    {
        quickdraw::QuickDraw::new(self.client)
    }

    pub fn events(&self) -> events::Events<'a>
    {
        events::Events::new(self.client)
    }

    pub fn scrap(&self) -> scrap::Scrap<'a>
    {
        scrap::Scrap::new(self.client)
    }

    pub fn textedit(&self) -> textedit::TextEdit<'a>
    {
        textedit::TextEdit::new(self.client)
    }

    pub fn resources(&self) -> resources::Resources<'a>
    {
        resources::Resources::new(self.client)
    }

    pub fn files(&self) -> files::Files<'a>
    {
        files::Files::new(self.client)
    }

    pub fn controls(&self) -> controls::Controls<'a>
    {
        controls::Controls::new(self.client)
    }

    pub fn menus(&self) -> menus::Menus<'a>
    {
        menus::Menus::new(self.client)
    }

    pub fn now(&self) -> ToolboxResult<Timestamp>
    {
        self.client.now()
    }

    pub fn dialogs(&self) -> dialogs::Dialogs<'a>
    {
        dialogs::Dialogs::new(self.client)
    }

    pub fn crashes(&self) -> crashes::Crashes<'a>
    {
        crashes::Crashes::new(self.client)
    }

    pub fn memory(&self) -> memory::Memory<'a>
    {
        memory::Memory::new(self.client)
    }

    pub fn log(&self, text: &str) -> core::result::Result<(), Status>
    {
        self.client.log(text).map_err(|e| e.0)
    }

    pub fn capabilities(&self) -> u64
    {
        self.client.capabilities()
    }

    pub fn new_window(&self, bounds: Rect, title: &str, minimum: (i16, i16), procedure: i16)
                      -> core::result::Result<u32, Status>
    {
        self.windows()
            .create(windows::WindowSpec::new(bounds, title).minimum(minimum.0, minimum.1)
                                                           .procedure(procedure))
            .map(|id| id.0)
            .map_err(|e| e.0)
    }

    pub fn close_window(&self, id: u32) -> core::result::Result<(), Status>
    {
        self.windows().close(WindowId(id)).map_err(|e| e.0)
    }

    pub fn select_window(&self, id: u32) -> core::result::Result<(), Status>
    {
        self.windows().select(WindowId(id)).map_err(|e| e.0)
    }

    pub fn select_desktop(&self) -> core::result::Result<(), Status>
    {
        self.windows().select_desktop().map_err(|e| e.0)
    }

    pub fn window(&self, id: u32) -> core::result::Result<WindowRequest, Status>
    {
        self.windows().info(WindowId(id)).map_err(|e| e.0)
    }

    pub fn begin_update(&self, id: u32) -> core::result::Result<Rect, Status>
    {
        self.windows().begin_update(WindowId(id)).map_err(|e| e.0)
    }

    pub fn end_update(&self, id: u32) -> core::result::Result<(), Status>
    {
        self.windows().end_update(WindowId(id)).map_err(|e| e.0)
    }

    pub fn draw(&self, id: u32, kind: u32, bounds: Rect, color: u32, text: &str)
                -> core::result::Result<(), Status>
    {
        self.quickdraw().port(WindowId(id)).command(kind, bounds, color, text).map_err(|e| e.0)
    }

    pub fn next_event(&self, mask: u16) -> core::result::Result<Event, Status>
    {
        self.events().next_event(mask).map_err(|e| e.0)
    }

    pub fn wait_event(&self, mask: u16, timeout_ticks: u32) -> core::result::Result<Event, Status>
    {
        self.events().wait_event(mask, timeout_ticks).map_err(|e| e.0)
    }
}
#[cfg(test)]
mod tests
{
    use mac_abi::{Header, NEW_WINDOW, traps};

    use super::*;
    unsafe extern "C" fn gate(number: u32, request: *mut (), size: usize) -> i64
    {
        match number
        {
            traps::LOG =>
            {
                assert_eq!(size, core::mem::size_of::<traps::LogCall>());
                let r = unsafe { &*request.cast::<traps::LogCall>() };
                assert_eq!(unsafe { core::slice::from_raw_parts(r.bytes, r.length) }, b"Finder");
                OK as i64
            }
            traps::WINDOW =>
            {
                assert_eq!(size, core::mem::size_of::<traps::WindowCall>());
                let r = unsafe { &mut *request.cast::<traps::WindowCall>() };
                assert_eq!(r.operation, NEW_WINDOW);
                assert_eq!(&r.request.title[..r.request.title_length as usize], b"System RISC");
                r.request.id = 7;
                OK as i64
            }
            traps::NEXT_EVENT =>
            {
                assert_eq!(size, core::mem::size_of::<traps::NextEventCall>());
                let r = unsafe { &mut *request.cast::<traps::NextEventCall>() };
                r.event = Event { kind: EventKind::KeyDown,
                                  message: 65,
                                  ..Default::default() };
                OK as i64
            }
            _ => NOT_SUPPORTED as i64
        }
    }
    #[test]
    fn clients_marshal_to_one_gate_and_validate_headers()
    {
        let services = SystemServices { header:       Header::for_type::<SystemServices>(),
                                        trap:         gate,
                                        capabilities: CAP_GRAPHICS };
        let system = unsafe { System::from_raw(&services) }.unwrap();
        system.log("Finder").unwrap();
        assert_eq!(system.windows()
                         .create(windows::WindowSpec::new(Rect::new(20, 60, 300, 300),
                                                          "System RISC"))
                         .unwrap(),
                   WindowId(7));
        assert_eq!(system.events().next(mac_abi::EVERY_EVENT).unwrap().unwrap().character(), 65);
        assert!(matches!(unsafe { System::from_raw(core::ptr::null()) }, Err(BAD_ARGUMENT)));
        let short = Header::for_type::<Header>();
        assert!(matches!(unsafe { System::from_raw((&short as *const Header).cast()) },
                         Err(BAD_ABI)));
    }
}
