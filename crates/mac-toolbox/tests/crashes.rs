#![cfg(not(target_arch = "riscv64"))]
mod support;
use mac_toolbox::{Error, System, crashes::*, panic};

#[test]
fn native_report_round_trips_long_utf8_and_scrolls_without_losing_text()
{
    support::with_system(|system| {
        let reports = system.crashes();
        assert_eq!(reports.begin().unwrap().report_bytes, 0);
        let text = format!("message café 🦀\r\n{}\n{}\nEND_OF_REPORT",
                           "x".repeat(1800),
                           (0..320).map(|n| format!("line {n:03}: café\n")).collect::<String>());
        for text in text.split_inclusive('\n')
        {
            reports.append(text).unwrap();
        }
        let top = reports.show(0).unwrap();
        assert_eq!(top.report_bytes, text.len());
        assert!(top.line_count > 320);
        let bottom = reports.scroll(i32::MAX).unwrap();
        assert_eq!(bottom.first_line + bottom.page_lines, bottom.line_count);
        assert_eq!(reports.scroll(i32::MIN).unwrap().first_line, 0);
        let mut restored = Vec::new();
        let mut chunk = [0; 13]; // deliberately splits multi-byte UTF-8 scalars
        loop
        {
            let count = reports.read(restored.len(), &mut chunk).unwrap();
            if count == 0
            {
                break;
            }
            restored.extend_from_slice(&chunk[..count]);
        }
        assert_eq!(restored, text.as_bytes());
        assert!(reports.read(text.len() + 1, &mut chunk).is_err());
        reports.dismiss().unwrap();
        assert_eq!(reports.info().unwrap().dismissed, 1);
        reports.begin().unwrap();
        assert_eq!(reports.info().unwrap().report_bytes, 0);
        assert_eq!(reports.info().unwrap().dismissed, 0);
    });
}

#[test]
fn shared_handler_streams_every_byte_across_fixed_buffer_utf8_boundaries()
{
    support::with_system(|system| {
        let text = format!("{}café 🦀{}END", "a".repeat(511), "é".repeat(20_000));
        let report = panic::report(system.client(), format_args!("{text}")).unwrap();
        assert_eq!(report.flags, 0);
        let mut bytes = vec![0; report.report_bytes];
        assert_eq!(system.crashes().read(0, &mut bytes).unwrap(), text.len() + 1);
        assert_eq!(String::from_utf8(bytes).unwrap(), text + "\n");
    });
}

#[test]
fn retention_and_formatter_failures_are_explicit_and_do_not_destroy_prior_diagnostics()
{
    struct Fails;
    impl core::fmt::Display for Fails
    {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
        {
            f.write_str("partial diagnostic café")?;
            Err(core::fmt::Error)
        }
    }
    support::with_system(|system| {
        let report = panic::report(system.client(), format_args!("{Fails}")).unwrap();
        assert_ne!(report.flags & FORMAT_FAILED, 0);
        let mut bytes = vec![0; report.report_bytes];
        system.crashes().read(0, &mut bytes).unwrap();
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.starts_with("partial diagnostic café"));
        assert!(text.contains("Formatting the panic information failed"));
        let reports = system.crashes();
        reports.begin().unwrap();
        let block = "x".repeat(4096);
        for _ in 0..MAX_REPORT_BYTES / block.len()
        {
            reports.append(&block).unwrap();
        }
        assert_eq!(reports.append("overflow"), Err(Error::NoMemory));
        let report = reports.show(0).unwrap();
        assert_eq!(report.report_bytes, MAX_REPORT_BYTES);
        assert_ne!(report.flags & TRUNCATED, 0);
        let mut tail = [0; 4];
        assert_eq!(reports.read(MAX_REPORT_BYTES - 4, &mut tail).unwrap(), 4);
        assert_eq!(&tail, b"xxxx");
    });
}

#[test]
fn panic_service_uses_an_exclusive_native_number_and_rejects_invalid_utf8()
{
    assert!(mac_abi::traps::CRASH > u16::MAX as u32);
    mac_system::testing::with_services(|services| {
        let system = unsafe { System::from_raw(services) }.unwrap();
        system.crashes().begin().unwrap();
        let invalid = [0xffu8];
        let mut call = CrashCall { operation: APPEND,
                                   data: invalid.as_ptr(),
                                   length: 1,
                                   ..Default::default() };
        assert_eq!(unsafe {
                       (services.trap)(mac_abi::traps::CRASH,
                                       core::ptr::from_mut(&mut call).cast(),
                                       core::mem::size_of_val(&call))
                   },
                   mac_abi::BAD_ARGUMENT as i64);
        assert_eq!(system.crashes().info().unwrap().report_bytes, 0);
    });
}
