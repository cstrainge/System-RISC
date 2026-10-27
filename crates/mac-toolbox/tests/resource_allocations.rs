//! Resource access stays usable under heap pressure through the public SDK.
#![cfg(not(target_arch = "riscv64"))]
#[path = "support/allocations.rs"]
mod allocations;
use allocations::Observe;
use mac_formats::resource::{Resource, encode};
use mac_toolbox::{Error, ResourceKey, ResourceScope, System, TypeCode};
fn row(kind: &[u8; 4], id: i16, name: Option<&[u8]>, data: &[u8]) -> Resource
{
    Resource { kind: *kind,
               id,
               name: name.map(<[u8]>::to_vec),
               attributes: 0x20,
               data: data.to_vec() }
}
#[test]
fn named_metadata_index_count_and_string_reads_need_no_heap_storage()
{
    let system = encode(&[row(b"ZZZZ", 1, Some(b"System"), b"system")]).unwrap();
    let app = encode(&[row(b"ZZZZ", 1, Some(&[b'C', b'a', b'f', 0x8e]), b"app"),
                       row(b"STR ", 2, None, &[2, 0x8e, b'!']),
                       row(b"STR#", 3, None, &[0, 2, 1, b'A', 1, 0x8e])]).unwrap();
    mac_system::testing::with_resource_forks(&system, &app, |services| {
        let system = unsafe { System::from_raw(services) }.unwrap();
        let resources = system.resources();
        let kind = TypeCode::new(*b"ZZZZ");
        let mut bytes = [0; 8];
        let mut text = [0; 8];
        let mut indexed = [0; 8];
        let observed = Observe::begin(true, None);
        let resource = resources.named(kind, "Café");
        let at = resources.at(kind, 1);
        let count = resources.count(kind);
        let read = resource.as_ref().map(|r| r.read(0, &mut bytes)).map_err(|error| *error);
        let value = resources.string(2, &mut text);
        let indexed_value = resources.indexed_string(3, 1, &mut indexed);
        let missing = resources.named(kind, "café");
        let mut short_output = [0; 1];
        let short = resources.string(2, &mut short_output);
        let allocations = observed.finish();
        assert_eq!(allocations, 0, "borrowed lookup and fixed UTF-8 outputs must not allocate");
        let resource = resource.unwrap();
        assert_eq!(resource.name(), "Café");
        assert_eq!(resource.home(), ResourceScope::Application);
        assert_eq!(resource.attributes().bits(), 0x20);
        assert_eq!(count, Ok(2));
        assert_eq!(read.unwrap(), Ok(3));
        assert_eq!(&bytes[..3], b"app");
        assert_eq!(at.unwrap().name(), "System");
        assert_eq!(value, Ok("é!"));
        assert_eq!(indexed_value, Ok("é"));
        assert_eq!(missing.err(), Some(Error::NotFound));
        assert_eq!(short.err(), Some(Error::NoMemory));
        assert!(Error::NotFound.to_string().contains("found"));
        assert!(Error::NoMemory.to_string().contains("memory"));
        resource.release().unwrap();
    }).unwrap();
}
#[test]
fn type_enumeration_refusal_is_retryable_and_retains_chain_first_appearance_order()
{
    let system = encode(&[row(b"BBBB", 1, None, b""), row(b"ZZZZ", 1, None, b"")]).unwrap();
    let app = encode(&[row(b"AAAA", 1, None, b""),
                       row(b"ZZZZ", 1, None, b""),
                       row(b"ZZZZ", 2, None, b"")]).unwrap();
    mac_system::testing::with_resource_forks(&system, &app, |services| {
        let system = unsafe { System::from_raw(services) }.unwrap();
        let resources = system.resources();
        for index in 0..3
        {
            let observed = Observe::begin(true, None);
            let count = resources.type_count();
            let kind = resources.type_at(index);
            let allocations = observed.finish();
            assert_eq!(count, Err(Error::NoMemory));
            assert_eq!(kind, Err(Error::NoMemory));
            assert!(allocations >= 2, "both enumerations must refuse their catalogue admission");
        }
        assert_eq!(resources.type_count(), Ok(3));
        for (index, kind) in [*b"AAAA", *b"ZZZZ", *b"BBBB"].into_iter().enumerate()
        {
            assert_eq!(resources.type_at(index as u32), Ok(TypeCode::new(kind)));
        }
        assert_eq!(resources.type_at(3), Err(Error::NotFound));
        assert_eq!(resources.get(ResourceKey { kind: TypeCode::new(*b"ZZZZ"),
                                               id:   2 })
                            .unwrap()
                            .home(),
                   ResourceScope::Application);
    }).unwrap();
}
#[test]
fn string_output_is_atomic_on_short_buffers_and_malformed_pascal_data()
{
    let app = encode(&[row(b"STR ", 1, None, &[2, 0x8e, b'!']),
                       row(b"STR ", 2, None, &[3, b'A']),
                       row(b"STR#", 3, None, &[0, 2, 1, b'A', 2, b'B'])]).unwrap();
    mac_system::testing::with_resource_forks(&encode(&[]).unwrap(), &app, |services| {
        let system = unsafe { System::from_raw(services) }.unwrap();
        let resources = system.resources();
        let mut short = [0x55; 2];
        assert_eq!(resources.string(1, &mut short), Err(Error::NoMemory));
        assert_eq!(short, [0x55; 2]);
        let mut output = [0x55; 8];
        assert_eq!(resources.string(2, &mut output), Err(Error::BadArgument));
        assert_eq!(resources.indexed_string(3, 1, &mut output), Err(Error::BadArgument));
        assert_eq!(output, [0x55; 8]);
        assert!(Error::BadArgument.to_string().contains("argument"));
        assert_eq!(resources.string(1, &mut output), Ok("é!"));
    }).unwrap();
}
