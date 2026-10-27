//! Boot catalog admission has no public trap; exercise its resident ownership
//! transaction with the same refusal transport used by canonical SDK tests.
#![cfg(not(target_arch = "riscv64"))]
#[path = "../../crates/mac-toolbox/tests/support/allocations.rs"]
mod allocations;
use allocations::Observe;
use mac_abi::NO_MEMORY;
use mac_formats::resource::Resource;
use mac_system::toolbox::quickdraw::fonts::FontManager;

fn resources() -> Vec<Resource>
{
    let mut rows = Vec::new();
    let ordinary = |id| {
        Resource { kind: *b"TEST",
                   id,
                   name: None,
                   attributes: 0x20,
                   data: vec![1, 2, 3] }
    };
    rows.push(ordinary(1));
    let mut fond = include_bytes!("../assets/fonts/FOND-0.bin").to_vec();
    fond[2..4].copy_from_slice(&3i16.to_be_bytes());
    rows.push(Resource { kind:       *b"FOND",
                         id:         3,
                         name:       Some(b"Caf\x8e".to_vec()),
                         attributes: 0,
                         data:       fond });
    rows.push(Resource { kind:       *b"FONT",
                         id:         12,
                         name:       None,
                         attributes: 0,
                         data:       include_bytes!("../assets/fonts/FONT-12.bin").to_vec() });
    rows.push(Resource { kind:       *b"FOND",
                         id:         0,
                         name:       None,
                         attributes: 0,
                         data:       include_bytes!("../assets/fonts/FOND-0.bin").to_vec() });
    let mut duplicate = rows[1].clone();
    duplicate.name = Some(b"Duplicate must not replace Cafe".to_vec());
    rows.push(duplicate);
    let mut smallest = rows[3].clone();
    smallest.id = i16::MIN;
    smallest.data[2..4].copy_from_slice(&i16::MIN.to_be_bytes());
    rows.push(smallest);
    rows.push(ordinary(2));
    rows
}
fn assert_catalog(manager: &FontManager)
{
    assert_eq!(manager.catalog().iter().map(|family| family.id).collect::<Vec<_>>(),
               [i16::MIN, 0, 3]);
    assert_eq!(manager.family_name(i16::MIN), Some("Font -32768"));
    assert_eq!(manager.family_name(0), Some("Font 0"));
    assert_eq!(manager.family_name(3), Some("Café"));
    assert_eq!(manager.resources().len(), 5);
}
#[test]
fn every_refused_catalog_admission_retains_original_resources_and_can_retry()
{
    let mut rows = resources();
    let observe = Observe::begin(false, None);
    let manager = FontManager::take_resources(&mut rows);
    let count = observe.finish();
    assert_catalog(&manager.unwrap());
    assert!(count > 0);
    for ordinal in 1..=count
    {
        let mut rows = resources();
        let original = rows.clone();
        let capacity = rows.capacity();
        let base = rows.as_ptr();
        let payloads = rows.iter().map(|r| r.data.as_ptr()).collect::<Vec<_>>();
        let observe = Observe::fail_at(ordinal);
        let refused = FontManager::take_resources(&mut rows);
        let observed = observe.finish();
        assert_eq!(refused.err(),
                   Some(NO_MEMORY),
                   "allocation {ordinal}/{count}; observed {observed}");
        assert_eq!(rows, original, "refusal changed or lost resource ownership");
        assert_eq!(rows.as_ptr(), base);
        assert_eq!(rows.capacity(), capacity);
        for (row, pointer) in rows.iter().zip(&payloads)
        {
            assert_eq!(row.data.as_ptr(), *pointer, "refusal replaced resource backing");
        }
        let fonts =
            rows.iter().filter(|r| r.kind != *b"TEST").map(|r| r.data.as_ptr()).collect::<Vec<_>>();
        let manager = FontManager::take_resources(&mut rows).unwrap();
        assert_catalog(&manager);
        assert_eq!(rows.iter().map(|r| r.id).collect::<Vec<_>>(), [1, 2]);
        assert_eq!(rows.as_ptr(), base);
        assert_eq!(rows.capacity(), capacity);
        assert_eq!(manager.resources().iter().map(|r| r.data.as_ptr()).collect::<Vec<_>>(),
                   fonts,
                   "font payloads must move without cloning");
    }
    assert!(mac_toolbox::Error::NoMemory.to_string().contains("memory"));
}
#[test]
fn absent_fonts_leave_ordinary_resources_in_place_without_allocating()
{
    let mut rows = vec![Resource { kind:       *b"TEST",
                                   id:         7,
                                   name:       None,
                                   attributes: 0,
                                   data:       vec![8] }];
    let base = rows.as_ptr();
    let capacity = rows.capacity();
    let observe = Observe::begin(true, None);
    let manager = FontManager::take_resources(&mut rows);
    let count = observe.finish();
    assert_eq!(count, 0);
    assert!(manager.unwrap().catalog().is_empty());
    assert_eq!(rows.as_ptr(), base);
    assert_eq!(rows.capacity(), capacity);
    assert_eq!(rows[0].data, [8]);
}
