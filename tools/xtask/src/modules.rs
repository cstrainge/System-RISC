//! Installation admission for the platform's pinned native module code area.
//! Descriptor spans include zero-filled storage and private heaps, rather than
//! just the much smaller serialized resource fork or executable segments.
use mac_build::{Result, error};
use serde_json::Value;

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
            return Err(error(format!("installed module code reservation is too small: \
                                      '{name}' needs {span} bytes, total {used} exceeds \
                                      {capacity} by {} bytes; increase the checked platform \
                                      modules reservation and rebuild System, ROM and every \
                                      package",
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
