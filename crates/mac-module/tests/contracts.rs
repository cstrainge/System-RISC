use mac_module::*;
const OFFER: Offer = Offer { class:          ClassId(*b"TEST"),
                             version:        1,
                             request_schema: 1,
                             reply_schema:   1,
                             max_request:    32,
                             max_reply:      64,
                             flags:          Offer::SERIALIZED,
                             capabilities:   0x80 };
fn spec<'a>() -> Spec<'a>
{
    Spec { kind:         PackageKind::Driver,
           kernel_abi:   7,
           target:       Target::RV64IMAC,
           identity:     *b"test-module-uuid",
           code_id:      0,
           code_digest:  [0x73; 32],
           heap_bytes:   4096,
           stack_bytes:  8192,
           name:         "Test device",
           version:      "0.1.0",
           provenance:   "test",
           offers:       &[OFFER],
           dependencies: &[],
           services:     &[],
           devices:      &[] }
}
fn encoded() -> Vec<u8>
{
    let mut data = vec![0; MAX_BYTES];
    let length = spec().encode(&mut data).unwrap().len();
    data.truncate(length);
    data
}
fn policy<'a>() -> Policy<'a>
{
    Policy { directory:  PackageKind::Driver,
             file_type:  *b"DRVR",
             kernel_abi: 7,
             target:     Target::RV64IMAC,
             max_heap:   65536,
             max_stack:  65536,
             services:   &[],
             providers:  &[] }
}
#[test]
fn roundtrip_open_classes_and_complete_contract()
{
    let offers = [OFFER,
                  Offer { class: ClassId(*b"NETW"),
                          version: 93,
                          ..OFFER }];
    let dependencies = [Dependency { module:   [0; 16],
                                     class:    ClassId(*b"BUS!"),
                                     version:  7,
                                     optional: true }];
    let services = [Service { service:  ClassId(*b"TIME"),
                              version:  17,
                              optional: true }];
    let devices = [DeviceMatch { bus:   ClassId(*b"VIRT"),
                                 value: [0x12; 16],
                                 mask:  [0xff; 16] }];
    let mut value = spec();
    value.offers = &offers;
    value.dependencies = &dependencies;
    value.services = &services;
    value.devices = &devices;
    let mut data = [0; MAX_BYTES];
    let contract = Contract::decode(value.encode(&mut data).unwrap()).unwrap();
    assert_eq!(contract.identity, value.identity);
    assert_eq!(contract.name, value.name);
    assert_eq!(contract.target, value.target);
    assert_eq!(contract.code_digest, value.code_digest);
    assert_eq!(contract.offers().collect::<Result<Vec<_>>>().unwrap(), offers);
    assert_eq!(contract.dependencies().collect::<Result<Vec<_>>>().unwrap(), dependencies);
    assert_eq!(contract.services().collect::<Result<Vec<_>>>().unwrap(), services);
    assert_eq!(contract.devices().collect::<Result<Vec<_>>>().unwrap(), devices);
    policy().validate(contract).unwrap();
}
#[test]
fn every_truncation_and_unknown_header_is_rejected()
{
    let data = encoded();
    for end in 0..data.len()
    {
        assert!(Contract::decode(&data[..end]).is_err(), "{end}");
    }
    for offset in [0, 4, 6, 8, 12, 20, 26, 27, 28, 56, 70, 86, 120, 127]
    {
        let mut bad = data.clone();
        bad[offset] ^= 0x80;
        let result = Contract::decode(&bad).and_then(|c| policy().validate(c));
        assert!(result.is_err(), "offset {offset}");
    }
    let mut extra = data;
    extra.push(0);
    assert!(Contract::decode(&extra).is_err());
}
#[test]
fn duplicate_and_conflicting_claims_are_not_negotiation()
{
    let mut data = [0; MAX_BYTES];
    let mut value = spec();
    let offers = [OFFER,
                  Offer { version: 2,
                          ..OFFER }];
    value.offers = &offers;
    assert_eq!(value.encode(&mut data), Err(Error::Duplicate));
    value.offers = &[OFFER];
    let dependency = Dependency { module:   [0; 16],
                                  class:    ClassId(*b"BUS!"),
                                  version:  1,
                                  optional: false };
    let dependencies = [dependency,
                        Dependency { version: 2,
                                     optional: true,
                                     ..dependency }];
    value.dependencies = &dependencies;
    assert_eq!(value.encode(&mut data), Err(Error::Duplicate));
    value.dependencies = &[];
    let service = Service { service:  ClassId(*b"TIME"),
                            version:  1,
                            optional: false };
    let services = [service,
                    Service { version: 2,
                              ..service }];
    value.services = &services;
    assert_eq!(value.encode(&mut data), Err(Error::Duplicate));
}
#[test]
fn admission_checks_exact_services_dependencies_and_target_before_entry()
{
    let service = Service { service:  ClassId(*b"TIME"),
                            version:  1,
                            optional: false };
    let dependency = Dependency { module:   *b"provider-module!",
                                  class:    ClassId(*b"BUS!"),
                                  version:  3,
                                  optional: false };
    let provider = Available { module:  dependency.module,
                               class:   dependency.class,
                               version: 3 };
    let mut value = spec();
    value.services = core::slice::from_ref(&service);
    value.dependencies = core::slice::from_ref(&dependency);
    let mut data = [0; MAX_BYTES];
    let contract = Contract::decode(value.encode(&mut data).unwrap()).unwrap();
    let mut admission = policy();
    assert_eq!(admission.validate(contract), Err(Error::MissingService));
    admission.services = core::slice::from_ref(&service);
    assert_eq!(admission.validate(contract), Err(Error::MissingDependency));
    admission.providers = core::slice::from_ref(&provider);
    assert_eq!(admission.validate(contract), Ok(()));
    let wrong = Available { version: 2,
                            ..provider };
    admission.providers = core::slice::from_ref(&wrong);
    assert_eq!(admission.validate(contract), Err(Error::MissingDependency));
    admission.providers = core::slice::from_ref(&provider);
    admission.target.architecture = Target::X86_64;
    assert_eq!(admission.validate(contract), Err(Error::Target));
    admission.target = Target::RV64IMAC;
    admission.target.features &= !Target::RV_A;
    assert_eq!(admission.validate(contract), Err(Error::Target));
    admission.target = Target::RV64IMAC;
    admission.directory = PackageKind::Extension;
    assert_eq!(admission.validate(contract), Err(Error::WrongLocation));
}
#[test]
fn size_limits_and_masked_device_matches()
{
    let mut value = spec();
    let mut output = [0; MAX_BYTES];
    let offers = [OFFER; MAX_RECORDS + 1];
    value.offers = &offers;
    assert_eq!(value.encoded_len(), Err(Error::Limit));
    value.offers = &[OFFER];
    assert_eq!(value.encode(&mut [0; 8]), Err(Error::Limit));
    let devices = [DeviceMatch { bus:   ClassId(*b"VIRT"),
                                 value: [0x10; 16],
                                 mask:  [0xf0; 16] }];
    value.devices = &devices;
    let contract = Contract::decode(value.encode(&mut output).unwrap()).unwrap();
    let device = contract.devices().next().unwrap().unwrap();
    assert!(device.matches(ClassId(*b"VIRT"), [0x1f; 16]));
    assert!(!device.matches(ClassId(*b"PCI!"), [0x1f; 16]));
    let invalid = [DeviceMatch { value: [0x11; 16],
                                 ..device }];
    value.devices = &invalid;
    assert_eq!(value.encode(&mut output), Err(Error::Contract));
}
