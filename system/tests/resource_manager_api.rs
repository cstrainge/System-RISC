//! Native clients exercise the resident manager using original font/icon/PICT payloads.
use std::{collections::BTreeSet, sync::OnceLock};

use mac_abi::{BAD_ARGUMENT, Header, NOT_SUPPORTED, OK, ResourceKey, SystemServices, resources::*,
              traps};
use mac_formats::resource::Resource as ForkResource;
use mac_system::toolbox::{quickdraw::fonts::FontManager, resources::manager::Manager};
use mac_traps::{Client, Error, ResourceRequest};
use toolbox_resources::Resources;

const SYSTEM_PICTURE: &[u8] = include_bytes!("../assets/visual/50494354--32510.bin");
const FINDER_PICTURE: &[u8] = include_bytes!("../../apps/finder/assets/visual/50494354-128.bin");
const SYSTEM_ICON: &[u8] = include_bytes!("../assets/visual/49434e23--32512.bin");
const CHICAGO: &[u8] = include_bytes!("../assets/fonts/FONT-12.bin");

struct Fixture
{
    system: Vec<ForkResource>,
    app:    Vec<ForkResource>,
    fonts:  FontManager
}

fn resource(kind: &[u8; 4], id: i16, name: Option<&str>, attributes: u8, data: &[u8])
            -> ForkResource
{
    ForkResource { kind: *kind,
                   id,
                   name: name.map(|name| mac_formats::text::encode(name).unwrap()),
                   attributes,
                   data: data.to_vec() }
}

fn fixture() -> &'static Fixture
{
    static FIXTURE: OnceLock<Fixture> = OnceLock::new();
    FIXTURE.get_or_init(|| {
               let mut font_resources =
                   vec![resource(b"FOND",
                                 0,
                                 Some("Chicago"),
                                 0,
                                 include_bytes!("../assets/fonts/FOND-0.bin")),
                        resource(b"FONT", 12, None, 0, CHICAGO),
                        resource(b"sfnt",
                                 9217,
                                 None,
                                 0,
                                 include_bytes!("../assets/fonts/sfnt-9217.ttf"))];
               let fonts = FontManager::take_resources(&mut font_resources);
               assert!(font_resources.is_empty());
               Fixture { system: vec![resource(b"PICT",
                                               -32510,
                                               Some("System Picture"),
                                               96,
                                               SYSTEM_PICTURE),
                                      resource(b"ICN#", -32512, None, 96, SYSTEM_ICON)],
                         app: vec![resource(b"PICT",
                                            -32510,
                                            Some("Application Picture"),
                                            32,
                                            FINDER_PICTURE),
                                   resource(b"PICT", 128, Some("Café À"), 32, FINDER_PICTURE)],
                         fonts }
           })
}

fn manager() -> Manager<'static>
{
    let fixture = fixture();
    Manager::new(&fixture.system, &fixture.app, fixture.fonts.resources())
}

unsafe extern "C" fn gate(number: u32, request: *mut (), size: usize) -> i64
{
    if number != traps::RESOURCE
    {
        return NOT_SUPPORTED as i64;
    }
    if request.is_null() || size != core::mem::size_of::<ResourceCall>()
    {
        return BAD_ARGUMENT as i64;
    }
    // The public Rust client owns each request and its input/output slices for
    // this synchronous callback. This fixture adapts that real ABI to Manager.
    let call = unsafe { &mut *request.cast::<ResourceCall>() };
    if (call.name_length != 0 && call.name.is_null())
       || (call.capacity != 0 && call.output.is_null())
    {
        return BAD_ARGUMENT as i64;
    }
    let name = if call.name_length == 0
    {
        ""
    }
    else
    {
        match core::str::from_utf8(unsafe {
                  core::slice::from_raw_parts(call.name, call.name_length)
              })
        {
            Ok(name) => name,
            Err(_) => return BAD_ARGUMENT as i64
        }
    };
    let output = if call.capacity == 0
    {
        &mut []
    }
    else
    {
        unsafe { core::slice::from_raw_parts_mut(call.output, call.capacity) }
    };
    manager().request(call, name, output).map_or_else(i64::from, |_| OK as i64)
}

static SERVICES: SystemServices = SystemServices { header:       Header::for_type::<SystemServices>(),
                                                   trap:         gate,
                                                   capabilities: 0 };

fn client() -> Client<'static>
{
    Client::new(&SERVICES).unwrap()
}

fn resources() -> Resources<'static>
{
    Resources::new(client())
}

fn read_all(resource: &toolbox_resources::Resource<'_>) -> Vec<u8>
{
    let mut data = vec![0; resource.size()];
    let mut offset = 0;
    while offset < data.len()
    {
        let end = (offset + 13).min(data.len());
        let count = resource.read(offset, &mut data[offset..end]).unwrap();
        assert!(count > 0);
        offset += count;
    }
    data
}

#[test]
fn public_lookup_preserves_negative_ids_metadata_and_pinned_home()
{
    let key = ResourceKey { kind: *b"PICT",
                            id:   -32510 };
    let overridden = resources().get((key).into()).unwrap();
    assert_eq!(overridden.key(), (key).into());
    assert_eq!(overridden.home(), toolbox_resources::ResourceScope::from_raw(APPLICATION).unwrap());
    assert_eq!(overridden.attributes(),
               toolbox_resources::ResourceAttributes::from_bits_retain(32));
    assert_eq!(overridden.name(), "Application Picture");
    assert_eq!(read_all(&overridden), FINDER_PICTURE);
    let original = resources().system().get((key).into()).unwrap();
    assert_eq!(original.home(), toolbox_resources::ResourceScope::from_raw(SYSTEM).unwrap());
    assert_eq!(original.name(), "System Picture");
    assert_eq!(original.attributes(), toolbox_resources::ResourceAttributes::from_bits_retain(96));
    assert_eq!(original.info().length, SYSTEM_PICTURE.len());
    assert_eq!(read_all(&original), SYSTEM_PICTURE);
    // A named System resource retains its home even though its ID is overridden.
    let named = resources().named(mac_traps::TypeCode::new(*b"PICT"), "System Picture").unwrap();
    assert_eq!(named.home(), toolbox_resources::ResourceScope::from_raw(SYSTEM).unwrap());
    assert_eq!(read_all(&named), SYSTEM_PICTURE);
}

#[test]
fn names_roundtrip_mac_roman_to_utf8_and_remain_case_sensitive()
{
    let picture = resources().named(mac_traps::TypeCode::new(*b"PICT"), "Café À").unwrap();
    assert_eq!(picture.key(),
               (ResourceKey { kind: *b"PICT",
                              id:   128 }).into());
    assert_eq!(picture.name(), "Café À");
    assert!(matches!(resources().named(mac_traps::TypeCode::new(*b"PICT"), "café À"),
                     Err(Error::NotFound)));
    assert!(matches!(resources().named(mac_traps::TypeCode::new(*b"PICT"), "Café A"),
                     Err(Error::NotFound)));
    assert!(matches!(resources().named(mac_traps::TypeCode::new(*b"ICN#"), "Café À"),
                     Err(Error::NotFound)));
    let long = resource(b"TEXT", 1, Some(&"".repeat(255)), 0, b"");
    let info = Manager::info(SYSTEM, &long);
    assert_eq!(info.name_length, 765);
    assert_eq!(info.name(), "".repeat(255));
    assert_eq!(info.name().chars().count(), 255);
}

#[test]
fn partial_reads_eof_load_and_release_preserve_original_forks()
{
    let icon = resources().system()
                          .get((ResourceKey { kind: *b"ICN#",
                                              id:   -32512 }).into())
                          .unwrap();
    assert_eq!(icon.size(), 256);
    icon.load().unwrap();
    assert_eq!(read_all(&icon), SYSTEM_ICON);
    let mut tail = [0xa5; 12];
    assert_eq!(icon.read(253, &mut tail), Ok(3));
    assert_eq!(&tail[..3], &SYSTEM_ICON[253..]);
    assert!(tail[3..].iter().all(|&byte| byte == 0xa5));
    assert_eq!(icon.read(icon.size(), &mut tail), Ok(0));
    assert_eq!(icon.read(0, &mut []), Ok(0));
    assert_eq!(icon.read(icon.size() + 1, &mut tail), Err(Error::BadArgument));
    assert_eq!(icon.read(usize::MAX, &mut tail), Err(Error::BadArgument));
    let key = icon.key();
    icon.release().unwrap();
    assert_eq!(read_all(&resources().get(key).unwrap()), SYSTEM_ICON);
    // The mask half survives the public partial-read transport byte for byte.
    let mut mask = [0; 128];
    resources().get(key).unwrap().read(128, &mut mask).unwrap();
    assert_eq!(&mask, &SYSTEM_ICON[128..]);
}

#[test]
fn moved_font_resources_remain_in_the_system_search_chain()
{
    assert_eq!(fixture().fonts.family_id("Chicago"), Some(0));
    let fond = resources().named(mac_traps::TypeCode::new(*b"FOND"), "Chicago").unwrap();
    assert_eq!(fond.home(), toolbox_resources::ResourceScope::from_raw(SYSTEM).unwrap());
    assert_eq!(fond.key().id, 0);
    let strike = resources().get((ResourceKey { kind: *b"FONT",
                                                id:   12 }).into())
                            .unwrap();
    assert_eq!(strike.home(), toolbox_resources::ResourceScope::from_raw(SYSTEM).unwrap());
    assert_eq!(read_all(&strike), CHICAGO);
    assert!(matches!(resources().application().get(strike.key()), Err(Error::NotFound)));
    let info = client().resource(ResourceRequest { operation: INFO,
                                                   scope: SYSTEM,
                                                   key: (strike.key()).into(),
                                                   ..Default::default() },
                                 &mut [])
                       .unwrap();
    assert_eq!(info.info.length, CHICAGO.len());
    assert_eq!(resources().system().count(mac_traps::TypeCode::new(*b"sfnt")), Ok(1));
}

#[test]
fn enumeration_visits_shadowed_resources_and_unique_types_without_losing_home()
{
    assert_eq!(resources().count(mac_traps::TypeCode::new(*b"PICT")), Ok(3));
    assert_eq!(resources().application().count(mac_traps::TypeCode::new(*b"PICT")), Ok(2));
    assert_eq!(resources().system().count(mac_traps::TypeCode::new(*b"PICT")), Ok(1));
    assert_eq!(resources().count(mac_traps::TypeCode::new(*b"NONE")), Ok(0));
    let entries: Vec<_> =
        (0..resources().count(mac_traps::TypeCode::new(*b"PICT")).unwrap()).map(|index| {
                                                     resources().at(mac_traps::TypeCode::new(*b"PICT"), index).unwrap()
                                                 })
                                                 .collect();
    assert_eq!(entries.iter().map(|r| (r.home(), r.key().id)).collect::<Vec<_>>(),
               [(toolbox_resources::APPLICATION, -32510),
                (toolbox_resources::APPLICATION, 128),
                (toolbox_resources::SYSTEM, -32510)]);
    assert_eq!(read_all(&entries[2]), SYSTEM_PICTURE);
    assert!(matches!(resources().at(mac_traps::TypeCode::new(*b"PICT"), 3), Err(Error::NotFound)));
    let count = resources().type_count().unwrap();
    let types: BTreeSet<_> = (0..count).map(|index| resources().type_at(index).unwrap()).collect();
    assert_eq!(count as usize, types.len());
    assert_eq!(types, BTreeSet::from([*b"PICT", *b"ICN#", *b"FOND", *b"FONT", *b"sfnt"].map(mac_traps::TypeCode::new)));
    assert!(matches!(resources().type_at(count), Err(Error::NotFound)));
    assert_eq!(resources().application().type_count(), Ok(1));
}

#[test]
fn low_level_client_rejects_invalid_scopes_operations_and_absent_keys()
{
    let request = |operation, scope, key| {
        ResourceRequest { operation,
                          scope,
                          key,
                          ..Default::default() }
    };
    let known = ResourceKey { kind: *b"PICT",
                              id:   -32510 };
    for operation in [GET, INFO, READ, LOAD, RELEASE, COUNT, COUNT_TYPES, TYPE_INDEX, INDEX, NAMED]
    {
        assert!(matches!(client().resource(request(operation, 123, known), &mut []),
                         Err(Error::BadArgument)));
    }
    assert!(matches!(client().resource(request(999, CHAIN, known), &mut []),
                     Err(Error::NotSupported)));
    let absent = ResourceKey { kind: *b"PICT",
                               id:   i16::MAX };
    for operation in [GET, INFO, READ, LOAD, RELEASE]
    {
        assert!(matches!(client().resource(request(operation, CHAIN, absent), &mut []),
                         Err(Error::NotFound)));
    }
    assert!(matches!(resources().application()
                                .named(mac_traps::TypeCode::new(*b"FOND"), "Chicago"),
                     Err(Error::NotFound)));
}
