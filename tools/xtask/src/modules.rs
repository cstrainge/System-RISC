//! Installation geometry for the platform's supervisor module execution VAs.
//! This is virtual reservation admission; real RAM and original module Kernel
//! charges are admitted per actual memory segment at runtime.
use std::{collections::BTreeMap, fs, path::Path};

use mac_build::{Result, error};
use serde_json::Value;

/// Check the actual packaged contracts, including independently loaded INIT
/// parts. System DRVR/INIT share one registry; Finder parts have their own.
pub(super) fn validate_identities<'a>(dist: &Path, names: impl IntoIterator<Item = &'a str>)
                                      -> Result<()>
{
    let mut installed = BTreeMap::new();
    for name in names
    {
        let bytes = fs::read(dist.join(format!("{name}.bin")))?;
        let file = mac_formats::macbinary::decode(&bytes)?;
        if !matches!(&file.file_type, b"DRVR" | b"INIT")
        {
            continue;
        }
        let resources = mac_formats::resource::decode(&file.resources)?;
        let mut register = |package: mac_formats::module::Package<'_>| -> Result<()> {
            let key = (package.contract.code_id, package.contract.identity);
            if let Some(previous) = installed.insert(key, name.to_owned())
            {
                let identity: String = key.1.iter().map(|byte| format!("{byte:02x}")).collect();
                return Err(error(format!("installed modules '{previous}' and '{name}' both \
                                          declare identity {identity} in EABI/{}; assign \
                                          distinct module identities and rebuild the packages \
                                          before creating the image",
                                         key.0)));
            }
            Ok(())
        };
        if file.file_type == *b"DRVR"
        {
            register(mac_formats::module::Package::inspect(&resources, file.file_type)?)?;
        }
        else
        {
            let mut found = false;
            for target in mac_module::ExtensionTarget::ALL
            {
                if resources.iter().any(|r| r.kind == *b"EABI" && r.id == target.resource_id())
                {
                    register(mac_formats::module::Package::inspect_extension(&resources, target)?)?;
                    found = true;
                }
            }
            if !found
            {
                // Keep the shared validator's actionable missing-EABI cause.
                register(mac_formats::module::Package::inspect(&resources, file.file_type)?)?;
            }
        }
    }
    Ok(())
}

pub(super) fn footprint(reports: &[Value], capacity: u64) -> Result<u64>
{
    let mut used = 0u64;
    for report in reports
    {
        let (base, limit) = match report["file_type"].as_str()
        {
            Some("DRVR") => (&report["load_base"], &report["load_limit"]),
            Some("INIT") =>
            {
                let parts = report["extension_parts"].as_array().ok_or_else(|| {
                                                                     error("installed extension \
                                                                            report lacks its \
                                                                            target descriptors; \
                                                                            rebuild the package \
                                                                            with current tools")
                                                                 })?;
                let mut system =
                    parts.iter().filter(|part| part["resource_id"].as_i64() == Some(0));
                let Some(part) = system.next()
                else
                {
                    continue;
                };
                if system.next().is_some()
                {
                    return Err(error("installed extension repeats its System/0 target \
                                      descriptor; rebuild the package"));
                }
                (&part["load_base"], &part["load_limit"])
            }
            _ => continue
        };
        let name = report["name"].as_str().ok_or_else(|| {
                                               error("installed module report lacks a package \
                                                      name; rebuild the package")
                                           })?;
        let span = base.as_u64()
                       .zip(limit.as_u64())
                       .and_then(|(base, limit)| {
                           (base.is_multiple_of(4096) && limit.is_multiple_of(4096))
                    .then(|| limit.checked_sub(base)).flatten().filter(|bytes| *bytes != 0)
                       })
                       .ok_or_else(|| {
                           error(format!("module '{name}' has a missing, reversed or unaligned \
                                          resident descriptor span; rebuild the package"))
                       })?;
        used = used.checked_add(span).ok_or_else(|| {
                                          error(format!("installed module footprint overflows \
                                                         while admitting '{name}'; correct its \
                                                         descriptor and rebuild"))
                                      })?;
        if used > capacity
        {
            return Err(error(format!("installed module execution VA range is too small: \
                                      '{name}' needs {span} bytes, total {used} exceeds \
                                      {capacity} by {} bytes; increase the checked platform \
                                      virtual_memory.modules range and rebuild System, ROM \
                                      and every package",
                                     used - capacity)));
        }
    }
    Ok(used)
}

#[cfg(test)]
mod tests
{
    use serde_json::json;

    use super::*;
    #[test]
    fn counts_full_resident_spans_and_only_the_system_extension_part()
    {
        let reports = [
                       json!({"name":"Driver", "file_type":"DRVR", "load_base":4096, "load_limit":16384}),
                       json!({"name":"Extension", "file_type":"INIT", "extension_parts":[
                {"resource_id":1,"load_base":4096,"load_limit":1048576},
                {"resource_id":0,"load_base":8192,"load_limit":16384}]}),
                       json!({"name":"Finder only", "file_type":"INIT", "extension_parts":[{"resource_id":1}]}),
                       json!({"name":"App", "file_type":"APPL", "load_base":0,"load_limit":u64::MAX})
        ];
        assert_eq!(footprint(&reports, 20480).unwrap(), 20480);
        let cause = footprint(&reports, 20479).unwrap_err().to_string();
        for required in
            ["Extension", "total 20480", "exceeds 20479 by 1", "checked platform", "ROM"]
        {
            assert!(cause.contains(required), "{cause}");
        }
    }
    #[test]
    fn malformed_and_oversized_installed_modules_are_refused_with_a_rebuild_cause()
    {
        for (base, limit) in [(8192, 4096), (4097, 8192), (4096, 4096)]
        {
            let report =
                json!({"name":"Broken", "file_type":"DRVR", "load_base":base,"load_limit":limit});
            let cause = footprint(&[report], u64::MAX).unwrap_err().to_string();
            assert!(cause.contains("Broken")
                    && cause.contains("descriptor")
                    && cause.contains("rebuild"),
                    "{cause}");
        }
        let report = json!({"name":"Too large", "file_type":"DRVR", "load_base":0,"load_limit":u64::MAX-4095});
        let cause = footprint(&[report.clone(), report], u64::MAX).unwrap_err().to_string();
        assert!(cause.contains("overflows")
                && cause.contains("Too large")
                && cause.contains("rebuild"),
                "{cause}");
        for report in [json!({"name":"Missing", "file_type":"INIT"}),
                       json!({"name":"Duplicate", "file_type":"INIT", "extension_parts":[{"resource_id":0},{"resource_id":0}]})]
        {
            let cause = footprint(&[report], u64::MAX).unwrap_err().to_string();
            assert!(cause.contains("extension") && cause.contains("rebuild"), "{cause}");
        }
    }
}
