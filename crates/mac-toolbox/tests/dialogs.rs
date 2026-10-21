#![cfg(not(target_arch = "riscv64"))]
use mac_formats::resource::{Resource, encode};
use mac_toolbox::{Error, System,
                  abi::{BAD_ARGUMENT, NOT_SUPPORTED}};
fn fork(kind: [u8; 4], id: i16, data: &[u8]) -> Resource
{
    Resource { kind,
               id,
               data: data.to_vec(),
               name: None,
               attributes: 0 }
}
fn resources(ditl: &[u8]) -> Vec<u8>
{
    encode(&[fork(*b"ALRT",
                  6012,
                  include_bytes!("../../../apps/finder/assets/visual/414c5254-6012.bin")),
             fork(*b"DITL", 6012, ditl)]).unwrap()
}
#[test]
fn original_alert_items_are_loaded_through_public_resource_and_dialog_services()
{
    let app = resources(include_bytes!("../../../apps/finder/assets/visual/4449544c-6012.bin"));
    mac_system::testing::with_resource_forks(&encode(&[]).unwrap(), &app, |services| {
        let system = unsafe { System::from_raw(services) }.unwrap();
        let dialogs = system.dialogs();
        assert_eq!(dialogs.validate_alert(mac_toolbox::ResourceScope::Application,
                                          6012,
                                          "Permanently remove Café?"),
                   Ok(4));
        assert_eq!(dialogs.validate_alert(mac_toolbox::ResourceScope::System, 6012, ""),
                   Err(Error::NotFound));
        let mut legacy = mac_abi::dialogs::DialogCall { operation:
                                                            mac_abi::dialogs::VALIDATE_ALERT,
                                                        scope: 1,
                                                        resource: 6012,
                                                        text_length: 5,
                                                        ..Default::default() };
        legacy.text[..5].copy_from_slice(b"\x04Caf\x8e");
        assert_eq!(unsafe {
                       (services.trap)(mac_abi::traps::PSTRING_DIALOG,
                                       core::ptr::from_mut(&mut legacy).cast(),
                                       size_of_val(&legacy))
                   },
                   0);
        assert_eq!(legacy.item, 4);
    }).unwrap();
}
#[test]
fn malformed_or_unsupported_items_fail_before_creating_a_window()
{
    for (data, expected) in [(vec![0, 3], BAD_ARGUMENT),
                             ({
                                  let mut d = include_bytes!("../../../apps/finder/assets/visual/\
                                                              4449544c-6012.bin").to_vec();
                                  d[14] = 7;
                                  d
                              },
                              NOT_SUPPORTED)]
    {
        mac_system::testing::with_resource_forks(&encode(&[]).unwrap(),
                                                 &resources(&data),
                                                 |services| {
                                                     let system = unsafe {
                                                                      System::from_raw(services)
                                                                  }.unwrap();
                                                     assert_eq!(system.dialogs()
                                                                      .validate_alert(mac_toolbox::ResourceScope::Application, 6012, ""),
                                                                Err(Error::from_status(expected)));
                                                 }).unwrap();
    }
}
#[test]
fn clock_uses_current_native_time_and_checked_classic_epoch()
{
    let before = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
                                             .unwrap()
                                             .as_secs() as i64;
    mac_system::testing::with_services(|services| {
        let system = unsafe { System::from_raw(services) }.unwrap();
        let now = system.now().unwrap();
        let after = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
                                                .unwrap()
                                                .as_secs() as i64;
        assert!((before..=after).contains(&now.unix_seconds()));
        assert_eq!(mac_toolbox::Timestamp::from_mac_seconds(now.mac_seconds().unwrap()).unix_seconds(), now.unix_seconds());
        assert_eq!(mac_toolbox::Timestamp::from_mac_microseconds(now.mac_microseconds().unwrap()),
                   now);
    });
}

#[test]
fn string_resources_are_utf8_in_rust_and_pascal_only_at_legacy_trap()
{
    let app = encode(&[fork(*b"STR#", 2000, b"\0\x02\x04Caf\x8e\x03One")]).unwrap();
    mac_system::testing::with_resource_forks(&encode(&[]).unwrap(), &app, |services| {
        let system = unsafe { System::from_raw(services) }.unwrap();
        let mut out = [0xa5; 16];
        assert_eq!(system.resources().application().indexed_string(2000, 0, &mut out).unwrap(),
                   "Café");
        assert_eq!(system.resources().application().indexed_string(2000, 1, &mut out).unwrap(),
                   "One");
        let mut short = [0xa5; 2];
        assert_eq!(system.resources().application().indexed_string(2000, 0, &mut short),
                   Err(Error::NoMemory));
        assert_eq!(short, [0xa5; 2]);
        let mut call = mac_abi::resources::ResourceCall { operation: mac_abi::resources::STRING,
                                                          scope: 1,
                                                          key: mac_abi::ResourceKey { kind:
                                                                                          *b"STR#",
                                                                                      id:   2000 },
                                                          output: out.as_mut_ptr(),
                                                          capacity: out.len(),
                                                          ..Default::default() };
        assert_eq!(unsafe {
                       (services.trap)(mac_abi::traps::PSTRING_RESOURCE,
                                       core::ptr::from_mut(&mut call).cast(),
                                       size_of_val(&call))
                   },
                   0);
        assert_eq!(&out[..call.transferred], b"\x04Caf\x8e");
        assert_eq!(system.resources().application().indexed_string(2000, 2, &mut out),
                   Err(Error::NotFound));
    }).unwrap();
}
