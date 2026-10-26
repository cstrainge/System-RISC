use mac_build::platform::Platform;

#[test]
fn display_policy_refuses_two_frames_when_the_resident_backbuffer_needs_a_third()
{
    let mut definition: serde_json::Value =
        serde_json::from_slice(include_bytes!("../../../platforms/QemuRISC-V.json")).unwrap();
    definition["memory"]["display_scratch"]["bytes"] = serde_json::json!("0x3aa000");
    let platform: Platform = serde_json::from_value(definition).unwrap();
    let error = platform.memory().err().expect("two-frame reservation must be refused");
    let message = error.to_string();
    assert!(message.contains("display_scratch must cover the 800x600 framebuffer")
            && message.contains("backbuffer"),
            "{message}");
}
