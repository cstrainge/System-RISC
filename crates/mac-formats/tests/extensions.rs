//! Package/entry qualification; no extension code is executed by inspection.
use mac_formats::{code::{CallingConvention, CodeRegistry, NoTranslator},
                  module::Package,
                  native::{self, Descriptor, Role},
                  resource::Resource};
use mac_module::{ExtensionTarget, PackageKind, Spec, Target};
use sha2::{Digest, Sha256};

fn part(target: ExtensionTarget, name: Option<&[u8]>) -> Vec<Resource>
{
    let descriptor = Descriptor { role:        Role::CodeResource,
                                  abi_version: native::ABI_VERSION,
                                  load_base:   0x82000000,
                                  load_limit:  0x82001000,
                                  entry:       0x82000000 };
    let mut code = descriptor.encode().unwrap().to_vec();
    code.extend(native::image::encode(&descriptor, [(0, 5, b"\x82\x80".as_slice())]).unwrap());
    code.extend_from_slice(b"RLC1\0\0\0\0");
    let spec = Spec { kind:         PackageKind::Extension,
                      kernel_abi:   native::ABI_VERSION,
                      target:       Target::RVA23U64,
                      identity:     *b"extension-test!!",
                      code_id:      target.resource_id(),
                      code_digest:  Sha256::digest(&code).into(),
                      heap_bytes:   0,
                      stack_bytes:  4096,
                      name:         "Extension fixture",
                      version:      "0.1.0",
                      provenance:   "test",
                      offers:       &[],
                      dependencies: &[],
                      services:     &[],
                      devices:      &[] };
    let mut metadata = vec![0; spec.encoded_len().unwrap()];
    spec.encode(&mut metadata).unwrap();
    vec![Resource { kind:       *b"RV64",
                    id:         target.resource_id(),
                    name:       name.map(<[u8]>::to_vec),
                    attributes: 0,
                    data:       code },
         Resource { kind:       *b"EABI",
                    id:         target.resource_id(),
                    name:       None,
                    attributes: 0,
                    data:       metadata }]
}

#[test]
fn system_and_finder_are_selected_by_resource_id_with_optional_or_unconventional_names()
{
    for target in ExtensionTarget::ALL
    {
        for name in [None,
                     Some(b"Unconventional name".as_slice()),
                     Some(target.conventional_name().as_bytes())]
        {
            let resources = part(target, name);
            let package = Package::inspect_extension(&resources, target).unwrap();
            assert_eq!(package.contract.code_type, *b"RV64");
            assert_eq!(package.contract.code_id, target.resource_id());
            assert_eq!(package.code.descriptor.role, Role::CodeResource);
        }
    }
}

#[test]
fn a_dual_part_bundle_has_two_distinct_entries_and_never_uses_finder_as_kernel_fallback()
{
    let mut resources = part(ExtensionTarget::System, Some(b"Finder"));
    resources.extend(part(ExtensionTarget::Finder, Some(b"System")));
    assert_eq!(Package::inspect(&resources, *b"INIT").unwrap().contract.code_id, 0);
    assert_eq!(Package::inspect_extension(&resources, ExtensionTarget::Finder).unwrap()
                                                                              .contract
                                                                              .code_id,
               1);
    let finder_only = part(ExtensionTarget::Finder, Some(b"System"));
    let cause = Package::inspect(&finder_only, *b"INIT").err().unwrap().to_string();
    for detail in ["System", "Finder-only", "rebuild"]
    {
        assert!(cause.contains(detail), "{cause}");
    }
    let system_only = part(ExtensionTarget::System, Some(b"Finder"));
    let cause = Package::inspect_extension(&system_only, ExtensionTarget::Finder).err()
                                                                                 .unwrap()
                                                                                 .to_string();
    assert!(cause.contains("Finder") && cause.contains("rebuild"), "{cause}");
}

#[test]
fn duplicate_or_orphan_targets_old_init_envelopes_and_wrong_contract_selection_are_refused()
{
    let good = part(ExtensionTarget::System, None);
    let mut duplicate = good.clone();
    duplicate.push(good[0].clone());
    let cause = Package::inspect(&duplicate, *b"INIT").err().unwrap().to_string();
    assert!(cause.contains("duplicate") && cause.contains("rebuild"), "{cause}");
    for index in 0..2
    {
        let mut orphan = good.clone();
        orphan.remove(index);
        let cause = Package::inspect(&orphan, *b"INIT").err().unwrap().to_string();
        assert!(cause.contains("paired") && cause.contains("rebuild"), "{cause}");
    }
    let mut old = good.clone();
    old[0].kind = *b"INIT";
    let cause = Package::inspect(&old, *b"INIT").err().unwrap().to_string();
    assert!(cause.contains("RV64") && cause.contains("rebuild"), "{cause}");
    let mut wrong = good.clone();
    wrong[1].data[60..62].copy_from_slice(&1i16.to_le_bytes());
    let cause = Package::inspect(&wrong, *b"INIT").err().unwrap().to_string();
    assert!(cause.contains("wrong execution target") && cause.contains("rebuild"), "{cause}");
    for id in [-1, 2]
    {
        let mut invalid = good.clone();
        invalid[0].id = id;
        assert!(Package::inspect(&invalid, *b"INIT").is_err());
    }
    assert!(Package::inspect(&good, *b"DRVR").is_err());
    assert!(Package::inspect(&good, *b"INIT").is_ok(),
            "failed inspections must not mutate the package");
}

#[test]
fn rv64_extension_envelopes_are_native_only_and_application_segment_streams_are_not_entries()
{
    let registry = CodeRegistry::new();
    assert_eq!(registry.convention(*b"RV64", 0), Some(CallingConvention::SystemExtension));
    assert_eq!(registry.convention(*b"RV64", 1), Some(CallingConvention::FinderExtension));
    assert_eq!(registry.convention(*b"RV64", 2), None);
    let mut resources = part(ExtensionTarget::System, None);
    let code = registry.prepare(&resources[0], 0, &mut NoTranslator).unwrap();
    resources[0].data = code.code;
    let cause = registry.prepare(&resources[0], 0, &mut NoTranslator).err().unwrap().to_string();
    assert!(cause.contains("native code-resource envelope") && cause.contains("rebuild"),
            "{cause}");
}
