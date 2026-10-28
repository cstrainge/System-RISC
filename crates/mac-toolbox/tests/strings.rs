#![cfg(not(target_arch = "riscv64"))]
use mac_abi::{files::*, traps::*, *};
fn raw<T>(gate: TrapGate, number: u32, call: &mut T) -> i64
{
    unsafe { gate(number, core::ptr::from_mut(call).cast(), size_of::<T>()) }
}
#[test]
fn legacy_file_names_and_paths_roundtrip_through_real_hfs()
{
    mac_system::testing::with_disk(include_bytes!("data/files/dates-2026.hfs"), |services| {
        let system = unsafe { mac_toolbox::System::from_raw(services) }.unwrap();
        let native =
            system.files()
                  .lookup(system.files().volume_at(0).unwrap().id,
                          mac_toolbox::Cnid(2),
                          "Folder/Café")
                  .unwrap();
        let name = b"\x0bFolder/Caf\x8e";
        let mut call = FileCall { operation: LOOKUP,
                                  volume: (system.files().volume_at(0).unwrap().id).0,
                                  cnid: 2,
                                  name: name.as_ptr(),
                                  name_length: name.len(),
                                  ..Default::default() };
        assert_eq!(raw(services.trap, PSTRING_FILE, &mut call), 0);
        assert_eq!(call.entry.id, (native.id).0);
        assert_eq!(&call.entry.name[..call.entry.name_length as usize], b"\x04Caf\x8e");
        assert_eq!(call.entry.created_at(), native.created_at());
        let mut output = [0xa5; 128];
        call.operation = PATH;
        call.cnid = (native.id).0;
        call.output = output.as_mut_ptr();
        call.capacity = output.len();
        assert_eq!(raw(services.trap, PSTRING_FILE, &mut call), 0);
        assert_eq!(&output[..call.transferred], b"\x0c/Folder/Caf\x8e");
        assert_eq!(output[call.transferred], 0xa5);
        output.fill(0xa5);
        call.capacity = 2;
        assert_eq!(raw(services.trap, PSTRING_FILE, &mut call), NO_MEMORY as i64);
        assert_eq!(call.length, 13);
        assert_eq!(output, [0xa5; 128]);
    })
}

/// Fixture setup uses the actual big-endian resource-fork writer. Expected
/// Unicode/Pascal bytes below are literal independent mapping observations.
fn maximum_resource_name_forks(byte: u8) -> (Vec<u8>, Vec<u8>)
{
    use mac_formats::resource::{Resource, encode};
    let fork = |payload: &[u8]| {
        encode(&[Resource { kind:       *b"NAME",
                            id:         -19,
                            name:       Some(vec![byte; 255]),
                            attributes: 0x20,
                            data:       payload.to_vec() }]).unwrap()
    };
    (fork(b"System resource payload"), fork(b"Application override payload"))
}
#[test]
fn maximum_macroman_resource_name_roundtrips_765_utf8_bytes_and_256_pascal_bytes()
{
    use mac_abi::resources::{APPLICATION, CHAIN, INFO, NAMED, ResourceCall, SYSTEM};
    // Apple mapping: 0xDE -> U+FB01 FI, 0xDF -> U+FB02 FL.
    // https://www.unicode.org/Public/MAPPINGS/VENDORS/APPLE/ROMAN.TXT
    for (byte, character) in [(0xde, "ﬁ"), (0xdf, "ﬂ")]
    {
        let (system_fork, app_fork) = maximum_resource_name_forks(byte);
        mac_system::testing::with_resource_forks(&system_fork, &app_fork, |services| {
            let system = unsafe { mac_toolbox::System::from_raw(services) }.unwrap();
            let native_name = character.repeat(255);
            assert_eq!(native_name.len(), 765);
            let key = mac_toolbox::resources::ResourceKey { kind: mac_toolbox::TypeCode::new(*b"NAME"),
                                                            id:   -19 };
            let application = system.resources().named(mac_toolbox::TypeCode::new(*b"NAME"), &native_name).unwrap();
            assert_eq!(application.name(), native_name);
            assert_eq!(application.info().name.len(), 765);
            assert_eq!(application.name().chars().count(), 255);
            assert_eq!(application.key(), key);
            assert_eq!(application.home(), mac_toolbox::ResourceScope::from_raw(APPLICATION).unwrap());
            let mut payload = [0xa5; 64];
            let length = application.read(0, &mut payload).unwrap();
            assert_eq!(&payload[..length], b"Application override payload");
            assert!(payload[length..].iter().all(|&b| b == 0xa5));
            let system_resource = system.resources().system().get(key).unwrap();
            assert_eq!(system_resource.name(), native_name);
            assert_eq!(system_resource.info().name.len(), 765);
            assert_eq!(system_resource.home(), mac_toolbox::ResourceScope::from_raw(SYSTEM).unwrap());
            let mut pascal = [byte; 256];
            pascal[0] = 255;
            let mut legacy = ResourceCall { operation: NAMED,
                                            scope: CHAIN,
                                            key: key.into(),
                                            name: pascal.as_ptr(),
                                            name_length: pascal.len(),
                                            ..Default::default() };
            assert_eq!(raw(services.trap, PSTRING_RESOURCE, &mut legacy), 0);
            assert_eq!(legacy.info.key, (key).into());
            assert_eq!(legacy.info.origin, APPLICATION);
            assert_eq!(legacy.info.name_length, 256);
            assert_eq!(&legacy.info.name[..256], &pascal);
            assert_eq!(legacy.name, pascal.as_ptr());
            assert_eq!(legacy.name_length, 256);
            legacy.operation = INFO;
            legacy.scope = SYSTEM;
            legacy.name = core::ptr::null();
            legacy.name_length = 0;
            legacy.info.name.fill(0xa5);
            legacy.info.name_length = 0;
            assert_eq!(raw(services.trap, PSTRING_RESOURCE, &mut legacy), 0);
            assert_eq!(legacy.info.origin, SYSTEM);
            assert_eq!(legacy.info.name_length, 256);
            assert_eq!(&legacy.info.name[..256], &pascal);
            for operation in [mac_abi::resources::GET,
                              mac_abi::resources::READ,
                              mac_abi::resources::LOAD,
                              mac_abi::resources::RELEASE]
            {
                legacy.operation = operation;
                assert_eq!(raw(services.trap, PSTRING_RESOURCE, &mut legacy), 0);
                assert_eq!(legacy.info.name_length, 256);
                assert_eq!(&legacy.info.name[..256], &pascal);
            }
            let native_again = system.resources().system().named(mac_toolbox::TypeCode::new(*b"NAME"), &native_name).unwrap();
            assert_eq!(native_again.info().name.len(), 765);
            assert_eq!(native_again.name(), native_name);
        }).unwrap();
    }
}
