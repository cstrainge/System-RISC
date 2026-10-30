//! Rust Toolbox facade over the resident System trap gate.
#![no_std]
#![cfg_attr(all(feature = "application", target_arch = "riscv64"), feature(alloc_error_handler))]
#[cfg(all(feature = "application", target_os = "systemrisc"))]
compile_error!("the System RISC std target supplies startup; remove the no_std application \
                feature");
/// Explicit wire/entry-point escape hatch. Ordinary applications use decoded
/// native services, typed identities and Error rather than numeric statuses.
pub use mac_abi as abi;
pub use mac_abi::{ApplicationEntry, CAP_EVENT_WAIT, CAP_GRAPHICS, CAP_MONOTONIC_CLOCK, Point,
                  Rect, SystemServices, WindowId, dates::Timestamp};
/// Core-only native thread/TLS transport consumed by the platform CRT.
pub use mac_traps::address_wait;
/// Owned, bounded UTF-8 text returned by Toolbox snapshots.
pub use mac_traps::types::InlineText;
pub use mac_traps::{Client, Cnid, Error, MenuId, ProcessId, Result as ToolboxResult, ThreadId,
                    TypeCode, VolumeId,
                    coordinates::{PointerPosition, ScreenPoint, WindowPoint},
                    native_threads,
                    types::{ResourceKey, ResourceScope}};
pub use toolbox_drag as drag;
pub use toolbox_events::{Event, EventMask, Modifiers};
pub use toolbox_inspection as inspection;
pub mod panic;
/// Native application entry, allocator and failure scaffold.
#[cfg(all(target_arch = "riscv64", not(target_os = "systemrisc")))]
pub mod application;
pub use toolbox_controls as controls;
pub use toolbox_crashes as crashes;
pub use toolbox_dialogs as dialogs;
pub use toolbox_events as events;
pub use toolbox_files as files;
pub use toolbox_memory as memory;
pub use toolbox_menus as menus;
pub use toolbox_power as power;
pub use toolbox_processes as processes;
pub use toolbox_quickdraw as quickdraw;
pub use toolbox_resources as resources;
pub use toolbox_scrap as scrap;
pub use toolbox_standard_file as standard_file;
pub use toolbox_textedit as textedit;
pub use toolbox_windows as windows;


#[derive(Clone, Copy)]
pub struct System<'a>
{
    client: Client<'a>
}
impl System<'static>
{
    /// Acquire the services installed by this application's platform startup.
    ///
    /// This is a cheap, repeatable routing view, not a new System owner. It
    /// allocates nothing and does not change application or thread authority.
    /// Calls made before valid application startup return `Error::BadAbi`.
    /// Each service still enforces its documented ownership/thread restrictions.
    pub fn new() -> ToolboxResult<Self>
    {
        crate::panic::installed_client().map(|client| Self { client }).ok_or(Error::BadAbi)
    }
}
impl<'a> System<'a>
{
    pub fn power(&self) -> power::Power<'a>
    {
        power::Power::new(self.client)
    }

    pub fn inspection(&self) -> inspection::Inspection<'a>
    {
        inspection::Inspection::new(self.client)
    }

    /// # Safety
    /// The loader-provided table and its advertised callbacks must remain valid.
    pub unsafe fn from_raw(services: *const SystemServices) -> core::result::Result<Self, Error>
    {
        unsafe { Client::from_raw(services) }.map(|client| Self { client })
    }

    pub fn client(&self) -> Client<'a>
    {
        self.client
    }

    pub fn drags(&self) -> drag::Drags<'a>
    {
        drag::Drags::new(self.client)
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

    pub fn standard_file(&self) -> standard_file::StandardFile<'a>
    {
        standard_file::StandardFile::new(self.client)
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

    pub fn log(&self, text: &str) -> core::result::Result<(), Error>
    {
        self.client.log(text)
    }

    pub fn capabilities(&self) -> u64
    {
        self.client.capabilities()
    }

    pub fn new_window(&self, bounds: Rect, title: &str, minimum: (i16, i16), procedure: i16)
                      -> core::result::Result<WindowId, Error>
    {
        self.windows().create(windows::WindowSpec::new(bounds, title).minimum(minimum.0, minimum.1)
                                                                     .procedure(procedure))
    }

    pub fn close_window(&self, id: WindowId) -> core::result::Result<(), Error>
    {
        self.windows().close(id)
    }

    pub fn select_window(&self, id: WindowId) -> core::result::Result<(), Error>
    {
        self.windows().select(id)
    }

    pub fn select_desktop(&self) -> core::result::Result<(), Error>
    {
        self.windows().select_desktop()
    }

    pub fn window(&self, id: WindowId) -> core::result::Result<windows::WindowInfo, Error>
    {
        self.windows().info(id)
    }

    pub fn begin_update(&self, id: WindowId) -> core::result::Result<Rect, Error>
    {
        self.windows().begin_update(id)
    }

    pub fn end_update(&self, id: WindowId) -> core::result::Result<(), Error>
    {
        self.windows().end_update(id)
    }

    pub fn draw(&self, id: WindowId, kind: quickdraw::DrawOperation, bounds: Rect, color: u32,
                text: &str)
                -> core::result::Result<(), Error>
    {
        self.quickdraw().port(id).command(kind, bounds, color, text)
    }

    pub fn next_positioned(&self, mask: EventMask) -> ToolboxResult<events::PositionedEvent<'a>>
    {
        self.events().next_positioned(mask)
    }

    pub fn wait_positioned(&self, mask: EventMask, timeout_ticks: u32)
                           -> ToolboxResult<events::PositionedEvent<'a>>
    {
        self.events().wait_positioned(mask, timeout_ticks)
    }

    pub fn next_event(&self, mask: EventMask) -> core::result::Result<Event<'a>, Error>
    {
        self.events().next(mask)
    }

    pub fn wait_event(&self, mask: EventMask, timeout_ticks: u32)
                      -> core::result::Result<Event<'a>, Error>
    {
        self.events().wait_event(mask, timeout_ticks)
    }
}
#[cfg(test)]
mod tests
{
    use mac_abi::{EventKind, Header, NEW_WINDOW, NOT_SUPPORTED, OK, traps};

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
                r.event = mac_abi::Event { kind: EventKind::KeyDown,
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
        assert_eq!(system.events()
                         .next(events::EventMask::ALL)
                         .unwrap()
                         .key()
                         .unwrap()
                         .character(),
                   'A');
        assert!(matches!(unsafe { System::from_raw(core::ptr::null()) }, Err(Error::BadArgument)));
        let short = Header::for_type::<Header>();
        assert!(matches!(unsafe { System::from_raw((&short as *const Header).cast()) },
                         Err(Error::BadAbi)));
    }
}

/// Supply the platform entry for a safe native `run(&System) -> ToolboxResult<()>`.
///
/// Enable the `application` feature. Current native applications are no_std and
/// may use `alloc`; this does not provide a Rust std platform implementation.
/// The SDK owns service installation, GlobalAlloc, panic and allocation failure.
#[cfg(feature = "application")]
#[macro_export]
macro_rules! application {
    ($run:path) => {
        #[cfg(not(target_arch = "riscv64"))]
        compile_error!("System RISC application runtime currently requires its native RV64 target");
        #[cfg(target_arch = "riscv64")]
        #[unsafe(no_mangle)]
        #[unsafe(link_section = ".text.entry")]
        pub unsafe extern "C" fn _start(services: *const $crate::SystemServices)
                                        -> $crate::abi::Status
        {
            // SAFETY: the real native loader owns this entry's live table,
            // initialized image and permanent service callbacks.
            unsafe { $crate::application::enter(services, $run) }
        }
    };
}
