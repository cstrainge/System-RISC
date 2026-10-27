//! Real allocator refusals at executable-resource admission/copy boundaries.
#[path = "../../mac-toolbox/tests/support/allocations.rs"]
mod allocations;

use allocations::Observe;
use mac_formats::{Error, Result,
                  code::{CallingConvention, CodeRegistry, CodeTranslator, NativeCode,
                         NoTranslator, STANDARD_CODE_TYPES, TranslationRequest},
                  native::{ABI_VERSION, DESCRIPTOR_SIZE, Descriptor, Role, image},
                  resource::Resource};

fn new_registry() -> CodeRegistry
{
    let mut registry = CodeRegistry::new();
    registry.register(*b"KEEP", CallingConvention::Custom).unwrap();
    registry
}
fn assert_registry(registry: &CodeRegistry)
{
    for &(kind, convention) in STANDARD_CODE_TYPES
    {
        assert_eq!(registry.convention(kind, 1), Some(convention));
    }
    assert_eq!(registry.convention(*b"CODE", 0), Some(CallingConvention::CodeJumpTable));
    assert_eq!(registry.convention(*b"KEEP", 0), Some(CallingConvention::Custom));
}
fn resource() -> Resource
{
    let descriptor = Descriptor { role:        Role::CodeResource,
                                  abi_version: ABI_VERSION,
                                  load_base:   0x82000000,
                                  load_limit:  0x82001000,
                                  entry:       0x82000000 };
    let pointer = (descriptor.load_base + 0x800).to_le_bytes();
    let mut data = descriptor.encode().unwrap().to_vec();
    data.extend(image::encode(&descriptor,
                              [(0, image::READ | image::EXECUTE, [0x82, 0x80].as_slice()),
                               (0x100, image::READ | image::WRITE, pointer.as_slice())]).unwrap());
    data.extend_from_slice(b"RLC1\x01\0\0\0\0\x01\0\0\x08\0\0\0");
    Resource { kind: *b"WDEF",
               id: 0,
               name: Some(b"Definition".to_vec()),
               attributes: 0,
               data }
}
fn assert_code(code: &NativeCode, original: &Resource)
{
    let descriptor = Descriptor::decode(&original.data[..DESCRIPTOR_SIZE]).unwrap();
    assert_eq!(code.descriptor, descriptor);
    let body = &original.data[DESCRIPTOR_SIZE..];
    assert_eq!([code.code.as_slice(), code.relocations.as_slice()].concat(), body);
    let plan = image::LoadPlan::new(&code.code,
                                    &code.descriptor,
                                    Some(&code.relocations),
                                    code.descriptor.load_base + 0x10000).unwrap();
    let mut destination = vec![0xff; plan.length()];
    plan.write(&mut destination).unwrap();
    assert_eq!(&destination[0x100..0x108], &(descriptor.load_base + 0x10800).to_le_bytes());
    assert_eq!(destination[0x900], 0);
}
struct Translation(Option<Vec<u8>>);
impl CodeTranslator for Translation
{
    fn translate(&mut self, _: TranslationRequest<'_>) -> Result<Vec<u8>>
    {
        self.0.take().ok_or(Error::Missing("fixture translation already consumed"))
    }
}
#[test]
fn static_profiles_and_existing_overrides_need_no_allocator()
{
    let observe = Observe::begin(true, None);
    let registry = CodeRegistry::new();
    let defaults = CodeRegistry::default();
    assert_eq!(observe.finish(), 0);
    for &(kind, convention) in STANDARD_CODE_TYPES
    {
        assert_eq!(registry.convention(kind, 1), Some(convention));
        assert_eq!(defaults.convention(kind, 1), Some(convention));
    }
    let mut registry = registry;
    let observe = Observe::fail_at(1);
    let refused = registry.register(*b"WDEF", CallingConvention::Custom);
    observe.finish();
    assert!(matches!(refused, Err(Error::AllocationFailed(_))));
    assert_eq!(registry.convention(*b"WDEF", 0), Some(CallingConvention::WindowDefinition));
    registry.register(*b"WDEF", CallingConvention::Custom).unwrap();
    assert_eq!(registry.convention(*b"WDEF", 0), Some(CallingConvention::Custom));
    let observe = Observe::begin(true, None);
    let result = registry.register(*b"WDEF", CallingConvention::WindowDefinition);
    assert_eq!(observe.finish(), 0);
    result.unwrap();
    assert_eq!(registry.convention(*b"WDEF", 0), Some(CallingConvention::WindowDefinition));
}
#[test]
fn translation_refusals_keep_the_original_typed_cause()
{
    struct Refuse(Error);
    impl CodeTranslator for Refuse
    {
        fn translate(&mut self, _: TranslationRequest<'_>) -> Result<Vec<u8>>
        {
            Err(self.0)
        }
    }
    let source = Resource { data: vec![0x4e, 0x75],
                            ..resource() };
    let registry = new_registry();
    for cause in [Error::AllocationFailed("translator cannot allocate its native output"),
                  Error::BudgetExceeded("translator's admitted output budget is exhausted")]
    {
        let observe = Observe::begin(true, None);
        let result = registry.prepare(&source, 123, &mut Refuse(cause));
        assert_eq!(observe.finish(), 0);
        assert_eq!(result.err(), Some(cause));
        assert_registry(&registry);
    }
}
#[test]
fn registration_growth_refuses_before_mutating_and_can_retry()
{
    let mut admissions = 0;
    for prefix in 0..40u8
    {
        let mut registry = new_registry();
        for id in 0..prefix
        {
            registry.register([b'T', b'E', b'S', id], CallingConvention::Procedure).unwrap();
        }
        let kind = [b'T', b'E', b'S', prefix];
        let observe = Observe::begin(false, None);
        let result = registry.register(kind, CallingConvention::Custom);
        let count = observe.finish();
        result.unwrap();
        if count == 0
        {
            continue;
        }
        admissions += 1;
        for ordinal in 1..=count
        {
            let mut registry = new_registry();
            for id in 0..prefix
            {
                registry.register([b'T', b'E', b'S', id], CallingConvention::Procedure).unwrap();
            }
            let observe = Observe::fail_at(ordinal);
            let refused = registry.register(kind, CallingConvention::Custom);
            observe.finish();
            assert!(matches!(refused, Err(Error::AllocationFailed(detail))
                if detail.contains("registration allocation failed")));
            assert_registry(&registry);
            for id in 0..prefix
            {
                assert_eq!(registry.convention([b'T', b'E', b'S', id], 0),
                           Some(CallingConvention::Procedure));
            }
            assert_eq!(registry.convention(kind, 0), None);
            registry.register(kind, CallingConvention::Custom).unwrap();
            assert_eq!(registry.convention(kind, 0), Some(CallingConvention::Custom));
        }
    }
    assert!(admissions > 1, "exercise initial admission and later registration growth");
}
#[test]
fn each_native_and_translated_prepare_allocation_refuses_without_mutation_and_retries()
{
    for translated in [false, true]
    {
        let original = resource();
        let mut source = original.clone();
        if translated
        {
            source.data = vec![0x4e, 0x75];
        }
        let registry = new_registry();
        let mut translator = Translation(Some(original.data.clone()));
        let observe = Observe::begin(false, None);
        let code = registry.prepare(&source, 123, &mut translator);
        let count = observe.finish();
        assert_code(&code.unwrap(), &original);
        assert!(count > 2, "include metadata admission before both payload copies");
        let saved = source.clone();
        let pointer = source.data.as_ptr();
        let capacity = source.data.capacity();
        let mut causes = Vec::new();
        for ordinal in 1..=count
        {
            let mut translator = Translation(Some(original.data.clone()));
            let observe = Observe::fail_at(ordinal);
            let code = registry.prepare(&source, 123, &mut translator);
            let observed = observe.finish();
            let error = code.err().expect("every observed prefix must refuse fallibly");
            assert!(matches!(error, Error::AllocationFailed(_)),
                    "{ordinal}/{count} ({observed} observed): {error:?}");
            assert!(!error.message().is_empty());
            causes.push(error.message());
            assert_eq!(source, saved);
            assert_eq!(source.data.as_ptr(), pointer);
            assert_eq!(source.data.capacity(), capacity);
            assert_registry(&registry);
            let mut translator = Translation(Some(original.data.clone()));
            assert_code(&registry.prepare(&source, 123, &mut translator).unwrap(), &original);
        }
        for expected in ["native segment metadata allocation failed",
                         "native relocation patch allocation failed",
                         "native code resource body allocation failed",
                         "native code resource relocation allocation failed"]
        {
            assert!(causes.contains(&expected), "copy/admission prefix not exercised: {expected}");
        }
    }
}
#[test]
fn malformed_segment_and_relocation_geometry_refuses_before_allocation()
{
    let resource = resource();
    let descriptor = Descriptor::decode(&resource.data[..DESCRIPTOR_SIZE]).unwrap();
    let mut malformed = resource.data[DESCRIPTOR_SIZE..].to_vec();
    malformed[8..16].copy_from_slice(&u64::MAX.to_be_bytes());
    let observe = Observe::begin(true, None);
    let result = image::decode(&malformed, &descriptor);
    assert_eq!(observe.finish(), 0);
    assert!(matches!(result, Err(Error::Malformed(_))));
    for table in [b"RLC1\x01\0\0\0\0\x10\0\0\x08\0\0\0".as_slice(),
                  b"RLC1\xff\xff\xff\xff".as_slice(),
                  b"RLC1\x01\0\0\0\0\x01\0\0\x03\0\0\0".as_slice()]
    {
        let observe = Observe::begin(true, None);
        let result = mac_formats::relocation::patches(table,
                                                      descriptor.load_base,
                                                      4096,
                                                      descriptor.load_base,
                                                      |_, _| {
                                                          panic!("malformed geometry reached \
                                                                  source reader")
                                                      });
        assert_eq!(observe.finish(), 0);
        assert!(matches!(result, Err(Error::Malformed(_))));
    }
    let observe = Observe::begin(true, None);
    let result = CodeRegistry::new().prepare(&Resource { data: vec![],
                                                         ..resource },
                                             0,
                                             &mut NoTranslator);
    assert_eq!(observe.finish(), 0);
    assert!(matches!(result, Err(Error::TranslationRequired(_))));
}
