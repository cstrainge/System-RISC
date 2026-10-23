//! Generate public library documentation without compiling guest executables.

use super::*;

pub(super) fn generate(workspace: &Workspace) -> Result<()>
{
    // --lib covers the facade, its sub-crates, shared formats, resident library
    // and host-tool libraries. Guest entry binaries require the native linker
    // and do not contribute a public library API.
    run(workspace.cargo()
                 .args(["doc", "--locked", "--workspace", "--lib", "--no-deps", "--target-dir"])
                 .arg(workspace.build.join("host")))?;
    let source = workspace.build.join("host/doc");
    let destination = workspace.root.join("docs/api");
    sync_tree(&source, &destination)?;
    fs::write(destination.join("index.html"),
              "<!doctype html><html lang=\"en\"><meta charset=\"utf-8\"><title>System RISC Rust \
               API</title><h1>System RISC Rust API</h1><p><a \
               href=\"mac_toolbox/index.html\">Toolbox application SDK</a></p><p><a \
               href=\"mac_driver/index.html\">Kernel module SDK</a></p><p><a \
               href=\"mac_definition/index.html\">Definition author SDK</a></p><p><a \
               href=\"mac_module/index.html\">Native module contracts</a></p><p><a \
               href=\"mac_hfs_b/index.html\">HFS-B filesystem</a></p><p><a \
               href=\"mac_fork_file/index.html\">Host fork containers</a></p><p><a \
               href=\"mac_sit/index.html\">StuffIt archive formats</a></p><p><a \
               href=\"mac_kernel_model/index.html\">Kernel model structures \
               (staged)</a></p><p>Library documentation uses the host configuration. Guest-only \
               hardware code and application entry binaries are not included. Regenerate with \
               <code>cargo xtask doc</code>.</p></html>\n")?;
    println!("Rust API documentation: {}", destination.join("index.html").display());
    Ok(())
}

/// Retain unchanged files and remove pages for libraries/items no longer built.
fn sync_tree(source: &Path, destination: &Path) -> Result<()>
{
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(destination)?
    {
        let entry = entry?;
        if !source.join(entry.file_name()).exists()
        {
            if entry.file_type()?.is_dir()
            {
                fs::remove_dir_all(entry.path())?;
            }
            else
            {
                fs::remove_file(entry.path())?;
            }
        }
    }
    for entry in fs::read_dir(source)?
    {
        let entry = entry?;
        let target = destination.join(entry.file_name());
        if entry.file_type()?.is_dir()
        {
            sync_tree(&entry.path(), &target)?;
        }
        else if !target.is_file() || fs::read(entry.path())? != fs::read(&target)?
        {
            fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}
