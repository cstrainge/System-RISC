//! Stage the real diagnostic declaration using the existing package helper.
#[path = "scheduler_manifest.rs"]
mod declaration;
pub fn prepare(root: &std::path::Path, stage: &std::path::Path) -> std::path::PathBuf
{
    // The existing helper stages a genuine Cargo manifest link for vers. Only
    // fixture declaration files change; normal entry validation stays active.
    let result = declaration::prepare(root, stage);
    for (source, target) in
        [("module.smp.json", "module.json"), ("resources.smp.json", "resources.json")]
    {
        std::fs::copy(root.join(source), stage.join(target)).expect("stage actual SMP fixture \
                                                                     declaration");
    }
    result
}
