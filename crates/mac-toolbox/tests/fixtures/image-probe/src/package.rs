mod wdef_fault;

/// Deliberately fault in the actual application-owned executable mapping. Only
/// this private APPL resource changes; no kernel fault-injection branch exists.
fn breakpoint_at_entry(resource: &mut mac_formats::resource::Resource)
{
    use mac_formats::{code::{CodeRegistry, NoTranslator},
                      native::image};
    let registry = CodeRegistry::new();
    let mut native = registry.prepare(resource, 0, &mut NoTranslator)
                             .expect("validate original WDEF envelope and relocations");
    let image = image::decode(&native.code, &native.descriptor).expect("decode WDEF segments");
    let entry = native.descriptor
                      .entry
                      .checked_sub(native.descriptor.load_base)
                      .expect("validated WDEF entry is relative to its actual link origin");
    let segment = image.segments
                       .iter()
                       .find(|segment| {
                           segment.flags & image::EXECUTE != 0
                           && entry.checked_sub(segment.offset).is_some_and(|offset| {
                                                                   offset.checked_add(4)
                                                                         .is_some_and(|end| {
                                                                             end
                                                                             <= segment.length
                                                                                as u64
                                                                         })
                                                               })
                       })
                       .expect("WDEF entry has four initialized executable bytes");
    let at = segment.data_offset
             + usize::try_from(entry - segment.offset).expect("WDEF entry offset fits host \
                                                               address space");
    // RV64 EBREAK. The production loader still performs its normal format,
    // relocation, mapping and user-callback entry validation.
    native.code[at..at + 4].copy_from_slice(&0x0010_0073u32.to_le_bytes());
    resource.data = native.descriptor.encode().expect("preserve WDEF descriptor").to_vec();
    resource.data.extend_from_slice(&native.code);
    resource.data.extend_from_slice(&native.relocations);
    registry.prepare(resource, 0, &mut NoTranslator)
            .expect("fault workload remains a valid native code resource");
}

fn main()
{
    let mut raw = std::env::args_os();
    let mut args = vec![raw.next().expect("host process supplies its program argument")];
    let mut window_definition = None;
    let mut controls = None;
    while let Some(argument) = raw.next()
    {
        let destination = match argument.to_str()
        {
            Some("--window-definition") => Some(&mut window_definition),
            Some("--controls") => Some(&mut controls),
            Some(text) if text.starts_with("--") =>
            {
                panic!("unknown private packager option: {text}")
            }
            _ => None
        };
        if let Some(destination) = destination
        {
            assert!(destination.is_none(), "duplicate private packager option {argument:?}");
            let value = raw.next().expect("private packager flag requires one path value");
            assert!(!value.to_str().is_some_and(|value| value.starts_with("--")),
                    "private packager flag requires a path before another flag");
            *destination = Some(std::path::PathBuf::from(value));
        }
        else
        {
            args.push(argument);
        }
    }
    assert!(args.len() >= 4 && args.len() <= 6,
            "expected IMAGE_PROBE_ELF OUTPUT_DIR CDEF_PANIC_ELF [MODE [TARGET]] \
             [--window-definition WDEF_ELF] [--controls ARTIFACT_DIR]");
    if args.get(4).is_some_and(|mode| mode == "capture-cancel")
    {
        assert!(controls.is_some(),
                "capture-cancel requires paired production --controls ARTIFACT_DIR");
        assert!(window_definition.is_none(),
                "capture-cancel uses a stock WDEF so its first owned definition proves the CDEF \
                 source");
    }
    let output = std::path::Path::new(&args[2]);
    mac_build::package::build(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("resources.json"),
        std::path::Path::new(&args[1]), output).expect("package private image probe");
    let bytes = std::fs::read(output.join("ImageProbe.bin")).expect("read probe package");
    let mut file = mac_formats::macbinary::decode(&bytes).expect("decode probe package");
    let definition_path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../definition-panic/resources.json");
    let definition_manifest = mac_build::Manifest::read(&definition_path).expect("read native \
                                                                                  disposal-counter \
                                                                                  CDEF manifest");
    let definition_elf = std::path::Path::new(&args[3]);
    assert_eq!(definition_elf.file_name(),
               Some(std::ffi::OsStr::new("CDEF_PANIC")),
               "CDEF ELF must be the actual CDEF_PANIC artifact read by its native_code payload");
    // Compile the ordinary native code resource from its actual target ELF.
    // Its bytes become an APPL resource; System resources are never replaced.
    let definitions = definition_manifest.compile_resources(
        &definition_path,
        &std::fs::read(definition_elf).expect("read target CDEF_PANIC ELF"),
        definition_elf.parent()).expect("package native disposal-counter CDEF");
    let mut resources = mac_formats::resource::decode(&file.resources).expect("decode probe fork");
    let definition = definitions.into_iter()
                                .find(|r| r.kind == *b"CDEF" && r.id == 18)
                                .expect("native disposal actor supplies the Static Text CDEF");
    resources.push(definition);
    if let Some(elf) = &window_definition
    {
        assert_eq!(elf.file_name(),
                   Some(std::ffi::OsStr::new("WDEF")),
                   "WDEF must be the actual target production artifact");
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let system_path = root.join("../../../../../system/resources.json");
        let system = mac_build::Manifest::read(&system_path).expect("read System WDEF definition");
        let path = root.join("../../../../../definitions/wdef/resources.json");
        let mut manifest = mac_build::Manifest::read(&path).expect("read WDEF build manifest");
        manifest.resources.push(system.resources
                                      .into_iter()
                                      .find(|r| r.kind == "WDEF" && r.id == 0)
                                      .expect("System declares standard WDEF"));
        let compiled = manifest.compile_resources(&path,
                                                  &std::fs::read(elf).expect("read actual \
                                                                              target WDEF ELF"),
                                                  elf.parent())
                               .expect("compile application-owned production WDEF resource");
        let mut definition = compiled.into_iter()
                                     .find(|r| r.kind == *b"WDEF" && r.id == 0)
                                     .expect("production WDEF resource compiled");
        if args.get(4).is_some_and(|mode| mode == "window-definition-fault")
        {
            breakpoint_at_entry(&mut definition);
        }
        if let Some(message) =
            args.get(4).and_then(|mode| mode.to_str()).and_then(wdef_fault::message)
        {
            let receipt =
                wdef_fault::append(&mut definition, message).expect("checked private WDEF \
                                                                     message trampoline must \
                                                                     preserve original segments");
            std::fs::write(output.join("ImageProbe.wdef-fault.json"),receipt)
                .expect("write exact private WDEF trampoline arithmetic receipt");
        }
        resources.push(definition);
    }
    let mode = args.get(4).and_then(|mode| mode.to_str());
    assert!(mode != Some("offscreen-controls") || controls.is_some(),
            "offscreen-controls requires paired production --controls ARTIFACT_DIR");
    assert!(mode != Some("offscreen-controls-stock") || controls.is_none(),
            "offscreen-controls-stock forbids application control overrides");
    if let Some(directory) = &controls
    {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        for (id, family, artifact) in
            [(0, "buttons", "CDEF_BUTTONS"), (1, "scrollbar", "CDEF_SCROLL_BAR")]
        {
            let system_path = root.join("../../../../../system/resources.json");
            let system =
                mac_build::Manifest::read(&system_path).expect("read original System CDEF entries");
            let path = root.join(format!("../../../../../definitions/{family}/resources.json"));
            let mut manifest =
                mac_build::Manifest::read(&path).expect("read production CDEF build manifest");
            assert!(manifest.resources.is_empty(),
                    "production definition manifest must be resource-neutral before the original \
                     System entry is added");
            manifest.resources.push(system.resources
                                          .into_iter()
                                          .find(|resource| {
                                              resource.kind == "CDEF" && resource.id == id
                                          })
                                          .expect("System declares original CDEF0/CDEF1"));
            let elf = directory.join(artifact);
            let compiled = manifest.compile_resources(&path,
                                                      &std::fs::read(&elf).expect("read actual \
                                                                                   production \
                                                                                   CDEF ELF"),
                                                      Some(directory))
                                   .expect("compile original production CDEF resource");
            let definition =
                compiled.into_iter()
                        .find(|resource| resource.kind == *b"CDEF" && resource.id == id)
                        .expect("production CDEF compiled with its original ID");
            assert!(!resources.iter()
                              .any(|resource| {
                                  resource.kind == definition.kind && resource.id == definition.id
                              }),
                    "private probe already contains selected CDEF resource");
            resources.push(definition);
        }
    }
    if let Some(mode) = args.get(4)
    {
        let mode = mode.to_str().expect("isolation mode must be UTF-8");
        assert!(matches!(mode,
                         "capture-cancel"
                         | "checked-transport"
                         | "state"
                         | "window-definition"
                         | "window-definition-fault"
                         | "window-definition-draw-fault"
                         | "window-definition-calculate-fault"
                         | "window-definition-dispose-fault"
                         | "control-definition-panic"
                         | "control-definition-dispose-panic"
                         | "offscreen-controls"
                         | "offscreen-controls-stock"
                         | "privileged"
                         | "breakpoint"
                         | "null-read"
                         | "kernel-read"
                         | "kernel-write"
                         | "text-write"
                         | "heap-execute"
                         | "stack-guard"),
                "unknown isolation mode");
        assert!(!(matches!(mode, "window-definition" | "window-definition-fault")
                  || wdef_fault::message(mode).is_some())
                || window_definition.is_some(),
                "window-definition needs --window-definition and the paired production WDEF ELF");
        resources.push(mac_formats::resource::Resource { kind: *b"STR ", id: 2001,
            name: None, attributes: 0, data: mac_formats::text::pstring(mode).expect("encode mode") });
        if let Some(target) = args.get(5)
        {
            let target = target.to_str().expect("target address must be UTF-8");
            let digits = target.strip_prefix("0x").expect("target address must start with 0x");
            let _ = u64::from_str_radix(digits, 16).expect("target address must be a u64 \
                                                            hexadecimal value");
            resources.push(mac_formats::resource::Resource { kind: *b"STR ", id: 2002,
                name: None, attributes: 0, data: mac_formats::text::pstring(target).expect("encode address") });
        }
        else
        {
            assert!(!matches!(mode, "checked-transport" | "kernel-read" | "kernel-write"),
                    "selected isolation mode needs the exact paired System virtual entry address");
        }
    }
    file.resources =
        mac_formats::resource::encode_native(&resources).expect("encode probe definition");
    std::fs::write(output.join("ImageProbe.bin"), mac_formats::macbinary::encode(&file)
        .expect("encode base probe")).expect("write base probe");
    // Multi-instance cases need distinct files; single destructive/graphics
    // cases need only their actual package, not sixteen unused duplicates.
    if mode.is_some_and(|mode| mode != "state")
    {
        return;
    }
    for index in 0..16
    {
        file.name = format!("ImageProbe {index:02}").into_bytes();
        let bytes = mac_formats::macbinary::encode(&file).expect("encode probe copy");
        std::fs::write(output.join(format!("ImageProbe {index:02}.bin")), bytes).expect("write probe copy");
    }
}
