# Resource Manager

The `toolbox-resources` crate is a safe, allocation-free Rust client. All search,
metadata and byte access run in `system/src/toolbox/resources/manager.rs` through
the native Resource Manager trap. `mac_toolbox::System::resources()` supplies a
client for the current foreground application.

```rust,ignore
let resources = system.resources();
let picture = resources.application().get(ResourceKey { kind: *b"PICT", id: 24000 })?;
picture.load()?;
let mut header = [0; 10];
let received = picture.read(0, &mut header)?;
picture.release()?;
```

Supported operations:

- `get` and `named`: type/ID or case-sensitive UTF-8 name lookup. Disk names
  are decoded from MacRoman. Signed classic IDs are preserved.
- `count`, `at`, `type_count`, `type_at`: zero-based Rust enumeration. Types
  are unique; resources with the same ID in separate files remain separate.
- `info`, `size`, `key`, `name`, `attributes`, `home`: original metadata and
  System/application origin. A resource object pins its lookup scope to its
  origin so a later read addresses the selected file.
- `read`: copies a bounded range into caller storage and returns the number
  transferred. End-of-resource reads return zero; offsets beyond it fail.
- `load` and `release`: validate the resource and release the caller's logical
  reference. Current bundles stay resident and immutable.

Default lookup searches application before System. `.application()` and
`.system()` restrict lookup; `.search_chain()` restores it. The System scope
includes the Font Manager's FOND, FONT/NFNT and sfnt store, so moving font data
into resident font ownership does not remove it from resource lookup. Icon and
PICT services resolve through this same manager; System color tables provide
the shared icon palette fallback.

This slice supports bundled icons, PICTs and fonts. Opening arbitrary resource
files, editing/writing resources, mutable/purgeable handles and original 68k
Resource Manager trap marshaling remain unimplemented. See
`system/tests/resource_manager_api.rs` for SDK/ABI tests and
`scripts/check_system7_resources.py` at the repository root for packaged-payload
and FOND-reference verification.
