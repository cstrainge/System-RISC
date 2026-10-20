# HFS-B host tool

Run these commands from the Rust workspace (`System7`). The formatter, host tool,
ROM and resident filesystem use the same `mac-hfs-b` crate. Images are created
outside the repository. All sizes below are bytes.

```sh
cargo run -p hfs-b -- create ../working/software.hfsb 524288000 'Software'
cargo run -p hfs-b -- copy-in ../working/software.hfsb /host/path/collection /
cargo run -p hfs-b -- list ../working/software.hfsb
cargo run -p hfs-b -- copy-out ../working/software.hfsb /collection ../working/saved-collection
```

`copy-in IMAGE HOST_PATH [GUEST_DIRECTORY] [--replace]` recursively imports an
ordinary host file/tree or an exported bundle. Paths inside the volume are UTF-8
slash paths. Ordinary host files become data forks, with host dates preserved;
they are not guessed to be MacBinary. Use `import IMAGE PARENT_ID FILE.bin` for
MacBinary, or `put IMAGE PARENT_ID NAME DATA_FILE RESOURCE_FILE` for explicit
forks. Directories merge. Existing files require `--replace`. Symlinks and special
host files are rejected. Copies stream in 64 KiB chunks, with bounded traversal.

`copy-out IMAGE GUEST_PATH HOST_DIRECTORY` produces a lossless bundle. Each file
has `data`, `resources` and versioned `metadata.json`; directories have metadata
and a `children` directory of entry bundles. Finder information, flags, comments,
full Unicode names and microsecond dates (including optional backup dates) survive
restoration. Catalog IDs are assigned by the destination volume. The host output
directory must not already exist, protecting earlier backups from overwrite.

## Preserve user state across a System rebuild

Export both Preferences and Documents from the old **offline** image before
rebuilding; restore into the fresh generated image afterward:

```sh
cargo run -p hfs-b -- copy-out ../build/system7/dist/system7.hfsb '/System Folder/Preferences' ../working/saved-preferences
cargo run -p hfs-b -- copy-out ../build/system7/dist/system7.hfsb /Documents ../working/saved-documents
# Rebuild the System volume here.
cargo run -p hfs-b -- copy-in ../build/system7/dist/system7.hfsb ../working/saved-preferences '/System Folder' --replace
cargo run -p hfs-b -- copy-in ../build/system7/dist/system7.hfsb ../working/saved-documents / --replace
cargo run -p hfs-b -- check ../build/system7/dist/system7.hfsb
```

Preferences remain versioned JSON owned by their services. These commands preserve
the user folders independently of regenerated System/application binaries. They do
not retain old disk images or migrate incompatible preference schemas. Recreate
the qcow2 from the updated raw image before launching it.

Never modify an image being used by a VM. Host writers take an exclusive file
lock; that does not replace coordination with external VM processes. Recursive
copies are not whole-tree transactions. On error, completed entries remain and
the command returns failure; an incomplete extraction lacks its final metadata.
`help` lists low-level format/check/fork/metadata operations.
