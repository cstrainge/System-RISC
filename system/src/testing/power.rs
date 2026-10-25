//! Trusted virtual power peripherals around the resident selector, File Manager
//! durability barrier and generation-checked module/request registry.
use alloc::{vec, vec::Vec};

use mac_abi::*;
use mac_module::{Contract, Offer, PackageKind, Spec, Target};
use mac_power_interface::Action;

use crate::drivers::Registry;
#[derive(Clone, Copy)]
pub struct Device
{
    pub version:      u32,
    pub capabilities: u64,
    pub status:       Status
}
pub(super) struct State
{
    registry:     Registry,
    devices:      Vec<(u32, Device)>,
    triggered:    Vec<Action>,
    applications: bool
}
impl Default for State
{
    fn default() -> Self
    {
        Self { registry:     Registry::new(Target::RVA23U64),
               devices:      Vec::new(),
               triggered:    Vec::new(),
               applications: false }
    }
}
impl State
{
    fn add(&mut self, device: Device)
    {
        let offer = Offer { class:          mac_power_interface::CLASS,
                            version:        device.version,
                            request_schema: 1,
                            reply_schema:   1,
                            max_request:    0,
                            max_reply:      0,
                            flags:          Offer::KERNEL_ONLY,
                            capabilities:   device.capabilities };
        let mut identity = [41; 16];
        identity[..8].copy_from_slice(&(self.devices.len() as u64).to_le_bytes());
        let spec = Spec { kind: PackageKind::Driver,
                          kernel_abi: ABI_VERSION,
                          target: Target::RVA23U64,
                          identity,
                          code_id: 0,
                          code_digest: [0; 32],
                          heap_bytes: 0,
                          stack_bytes: 4096,
                          name: "Virtual power device",
                          version: "0.1.0",
                          provenance: "SDK hardware transport fixture",
                          offers: &[offer],
                          dependencies: &[],
                          services: &[],
                          devices: &[] };
        let mut bytes = vec![0; spec.encoded_len().unwrap()];
        spec.encode(&mut bytes).unwrap();
        let module = self.registry
                         .stage(Contract::decode(&bytes).unwrap(), PackageKind::Driver, *b"DRVR")
                         .unwrap();
        self.registry.commit(module).unwrap();
        let interface = self.registry
                            .find(mac_power_interface::CLASS, device.version)
                            .find(|(_, i)| i.module == module)
                            .unwrap()
                            .0;
        self.devices.push((interface, device));
    }

    pub(super) fn request(&mut self, files: &mut crate::toolbox::files::Manager,
                          call: &mut power::PowerCall)
                          -> Result<(), Status>
    {
        struct Host<'a>
        {
            state: &'a mut State,
            files: &'a mut crate::toolbox::files::Manager
        }
        impl crate::toolbox::power::Host for Host<'_>
        {
            fn applications_ready(&mut self) -> Result<(), Status>
            {
                if self.state.applications { Err(SERVICE_BUSY) } else { Ok(()) }
            }

            fn select(&mut self, capability: u64) -> Result<Option<u32>, Status>
            {
                self.state
                    .registry
                    .select(mac_power_interface::CLASS, mac_power_interface::VERSION, capability)
                    .map_err(crate::drivers::status)
            }

            fn flush(&mut self) -> Result<(), Status>
            {
                self.files.prepare_power()
            }

            fn trigger(&mut self, provider: u32, action: Action) -> Result<(), Status>
            {
                let ticket = self.state
                                 .registry
                                 .begin(0, provider, action.operation(), 0, 0)
                                 .map_err(crate::drivers::status)?;
                self.state.registry.enter_request(0, ticket).map_err(crate::drivers::status)?;
                let device = &mut self.state
                                      .devices
                                      .iter_mut()
                                      .find(|(id, _)| *id == provider)
                                      .ok_or(NOT_FOUND)?
                                      .1;
                let result = core::mem::replace(&mut device.status, OK);
                self.state.triggered.push(action);
                self.state.registry.leave_request(0, ticket).map_err(crate::drivers::status)?;
                self.state.registry.complete(0, ticket, 0).map_err(crate::drivers::status)?;
                if result == OK { Ok(()) } else { Err(result) }
            }
        }
        crate::toolbox::power::request(&mut Host { state: self,
                                                   files },
                                       call)
    }
}
pub fn install(devices: &[Device])
{
    super::SESSION.with(|session| {
                      let mut session = session.borrow_mut();
                      let state = &mut session.as_mut().unwrap().power;
                      *state = State::default();
                      for device in devices
                      {
                          state.add(*device);
                      }
                  });
}
pub fn triggered() -> Vec<Action>
{
    super::SESSION.with(|session| session.borrow().as_ref().unwrap().power.triggered.clone())
}
pub fn applications_running(value: bool)
{
    super::SESSION.with(|session| {
                      session.borrow_mut().as_mut().unwrap().power.applications = value
                  });
}
