# Native host fork tools

The `risc-forks` host tool formats and populates a native shared-directory root.
It uses the same `no_std`, safe `mac-fork-file` codec as the filesystem provider.
This tool is working host-side infrastructure; live guest mounting remains a
separate driver/File Manager acceptance gate.

```sh
cargo run -p risc-forks -- init /tmp/mac-share "External Software"
cargo run -p risc-forks -- import-macbinary /tmp/mac-share application.bin
cargo run -p risc-forks -- import /tmp/mac-share some-host-folder
cargo run -p risc-forks -- list /tmp/mac-share /
cargo run -p risc-forks -- export /tmp/mac-share /Documents /tmp/saved-documents
cargo run -p risc-forks -- check /tmp/mac-share
```

Import accepts ordinary files/directories or exported `system-risc-forks` bundles.
Use `import-macbinary` for classic app packages so their resource forks and
Finder information survive. Bundle exports round-trip through the HFS-B tool;
identities are newly assigned at the destination, while names, content and native
metadata are preserved.

Every regular guest file occupies one portable host container; directories retain
the host's directory structure. Host components are stable identity tokens.
Display names, including Unicode and Windows reserved names, live in metadata.
Do not manually duplicate or edit raw containers: `check` reports conflicting
identities, malformed bytes, unwrapped entries and interrupted stages explicitly.
The tool follows no source symlinks and confines managed paths to its opened root.

A writable root requires file and directory sync plus atomic publication. A flush
failure is an error. The tool does not claim Windows host-filesystem durability
from the separately successful Windows QEMU channel test. See
[FORMAT.md](../../FORMAT.md) and [HOST-FOLDERS.md](../../docs/HOST-FOLDERS.md).
