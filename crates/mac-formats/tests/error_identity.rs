//! The same detail can accompany different causes; prose is never identity.
use mac_formats::{Error,
                  native::{ABI_VERSION, Descriptor, Role, image}};

#[test]
fn diagnostic_text_is_preserved_without_defining_the_error_family()
{
    for message in ["limit exceeded", "absent resource", "alloc failed",
                    "invalid allocation size field", "missing translation budget"]
    {
        for error in [Error::BudgetExceeded(message),
                      Error::AllocationFailed(message),
                      Error::Missing(message),
                      Error::TranslationRequired(message),
                      Error::Malformed(message)]
        {
            assert_eq!(error.message(), message);
            assert_eq!(error.to_string(), message);
        }
    }
}

#[test]
fn native_reader_retains_each_typed_cause_through_the_wire_codec_adapter()
{
    let descriptor = Descriptor { role:        Role::CodeResource,
                                  abi_version: ABI_VERSION,
                                  load_base:   0x82000000,
                                  load_limit:  0x82001000,
                                  entry:       0x82000000 };
    for error in [Error::BudgetExceeded("limit exceeded"),
                  Error::AllocationFailed("alloc failed"),
                  Error::Missing("absent input"),
                  Error::TranslationRequired("needs adapter"),
                  Error::Malformed("allocation budget missing translation")]
    {
        assert_eq!(image::inspect(8, &descriptor, |_, _| Err(error), |_| {}), Err(error));
    }
    let bytes = image::encode(&descriptor, [(0, 5, [0x82, 0x80].as_slice())]).unwrap();
    let error=image::LoadPlan::new(&bytes,&descriptor,None,descriptor.load_base)
        .err().expect("definition without relocation metadata must fail");
    assert!(matches!(error, Error::Missing(_)));
    assert!(!error.message().is_empty());
    assert!(matches!(mac_formats::code::CodeRegistry::new().prepare(
        &mac_formats::resource::Resource{kind:*b"WDEF",id:0,name:None,attributes:0,data:vec![0x4e,0x75]},
        0,&mut mac_formats::code::NoTranslator),Err(Error::TranslationRequired(_))));
}
