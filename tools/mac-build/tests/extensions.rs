use mac_build::package;
use mac_formats::{macbinary::{self, MacFile},
                  native::{self, Descriptor, Role},
                  resource::{self, Resource}};
use mac_module::{ExtensionTarget, PackageKind, Spec, Target};
use sha2::{Digest, Sha256};

fn resources(target: ExtensionTarget) -> Vec<Resource>
{
    let descriptor = Descriptor { role:        Role::CodeResource,
                                  abi_version: native::ABI_VERSION,
                                  load_base:   0x82000000,
                                  load_limit:  0x82001000,
                                  entry:       0x82000000 };
    let mut code = descriptor.encode().unwrap().to_vec();
    code.extend(native::image::encode(&descriptor, [(0, 2, 5, b"\x82\x80".as_slice())]).unwrap());
    code.extend_from_slice(b"RLC1\0\0\0\0");
    let spec = Spec { kind:         PackageKind::Extension,
                      kernel_abi:   native::ABI_VERSION,
                      target:       Target::RVA23U64,
                      identity:     *b"extension-test!!",
                      code_id:      target.resource_id(),
                      code_digest:  Sha256::digest(&code).into(),
                      heap_bytes:   0,
                      stack_bytes:  4096,
                      name:         "Build extension fixture",
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
                    name:       None,
                    attributes: 0,
                    data:       code },
         Resource { kind:       *b"EABI",
                    id:         target.resource_id(),
                    name:       None,
                    attributes: 0,
                    data:       metadata }]
}
fn binary(resources: &[Resource]) -> Vec<u8>
{
    macbinary::encode(&MacFile { name:      b"Extension fixture".to_vec(),
                                 file_type: *b"INIT",
                                 creator:   *b"MACS",
                                 flags:     0,
                                 created:   0,
                                 modified:  0,
                                 data:      vec![],
                                 resources: resource::encode_native(resources).unwrap() }).unwrap()
}

#[test]
fn package_inspection_reports_each_extension_target_without_needing_resource_names()
{
    for target in ExtensionTarget::ALL
    {
        let report = package::inspect(&binary(&resources(target))).unwrap();
        assert_eq!(report["extension_parts"].as_array().unwrap().len(), 1);
        assert_eq!(report["extension_parts"][0]["target"], target.conventional_name());
        assert_eq!(report["extension_parts"][0]["resource_type"], "RV64");
        assert_eq!(report["extension_parts"][0]["resource_id"], target.resource_id());
    }
    let mut both = resources(ExtensionTarget::Finder);
    both.extend(resources(ExtensionTarget::System));
    let report = package::inspect(&binary(&both)).unwrap();
    let parts = report["extension_parts"].as_array().unwrap();
    assert_eq!(parts.len(), 2);
    assert_eq!(parts[0]["target"], "System");
    assert_eq!(parts[1]["target"], "Finder");
    both[1].data[60..62].copy_from_slice(&0i16.to_le_bytes());
    let cause = package::inspect(&binary(&both)).unwrap_err().to_string();
    assert!(cause.contains("wrong execution target") && cause.contains("rebuild"), "{cause}");
}
