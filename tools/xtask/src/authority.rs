//! Compile trusted build policy into a System resource bound to exact content.
use mac_formats::{authority, macbinary, resource};
use serde::Deserialize;

use super::*;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Policy
{
    version: u32,
    grants:  Vec<Grant>
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Grant
{
    package:      String,
    capabilities: Vec<String>
}

pub(super) fn install(workspace: &Workspace, reports: &mut [Value]) -> Result<()>
{
    let policy: Policy =
        serde_json::from_slice(&fs::read(workspace.root
                                                  .join("system/application-authority.json"))?)?;
    if policy.version != 1
    {
        return Err(error("unsupported application authority policy version; rebuild System \
                          policy"));
    }
    let dist = workspace.build.join("dist");
    let mut grants = Vec::new();
    for grant in policy.grants
    {
        // Resolve only packages just built by this invocation. Never permit a
        // policy path to read arbitrary/stale host artifacts.
        if !reports.iter().any(|r| r["name"].as_str() == Some(&grant.package) && r["role"] == "application")
        { return Err(error(format!("authority policy package '{}' is not a freshly built application", grant.package))); }
        let mut capabilities = 0;
        for capability in grant.capabilities
        {
            capabilities |= match capability.as_str()
            {
                "inspect-domains" => authority::INSPECT_DOMAINS,
                _ =>
                {
                    return Err(error(format!("unknown authority capability '{capability}' for \
                                              '{}'",
                                             grant.package)));
                }
            };
        }
        let file = macbinary::decode(&fs::read(dist.join(format!("{}.bin", grant.package)))?)?;
        let resources = resource::decode(&file.resources)?;
        grants.push((authority::image_digest(&resources)?, capabilities));
    }
    let bytes = authority::encode(&grants)?;
    let path = dist.join("System.bin");
    let mut file = macbinary::decode(&fs::read(&path)?)?;
    let mut resources = resource::decode(&file.resources)?;
    if resources.iter().any(|r| r.kind == authority::RESOURCE_KIND && r.id == 0)
    {
        return Err(error("System manifest duplicates the generated application authority \
                          policy resource"));
    }
    resources.push(resource::Resource { kind:       authority::RESOURCE_KIND,
                                        id:         0,
                                        name:       Some(b"Application Authority".to_vec()),
                                        attributes: 0,
                                        data:       bytes });
    file.resources = resource::encode_native(&resources)?;
    let binary = macbinary::encode(&file)?;
    let original =
        reports.iter_mut()
               .find(|r| r["role"] == "system")
               .ok_or_else(|| error("System package missing from authority policy build"))?;
    let mut report = package::inspect(&binary)?;
    for field in ["packaged_at_unix", "source_date_epoch", "cargo_version"]
    {
        report[field] = original[field].clone();
    }
    fs::write(path, binary)?;
    fs::write(dist.join("System.rsrc"), &file.resources)?;
    fs::write(dist.join("System.json"), serde_json::to_vec_pretty(&report)?)?;
    *original = report;
    Ok(())
}
