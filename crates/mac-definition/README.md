# Native CDEF authors

Each native control definition exposes one safe Rust handler named `main`:

```rust
#![cfg_attr(target_arch = "riscv64", no_std)]
#![cfg_attr(target_arch = "riscv64", no_main)]
#![deny(unsafe_code)]
use mac_definition::{Button, Message, Reply, Result, Services};

#[mac_definition::cdef_main]
fn main<'a>(message: Message<'a, Button<'a>>, services: Services<'a>) -> Result<Reply>
{
    // Match the messages and implement this family's drawing and geometry.
    let _ = (message, services);
    Ok(Reply::Done)
}
```

The attribute takes no arguments. The state in `Message` selects the shared
binding: `Button`, `Scrollbar`, `Progress` or `StaticText`. Use the family name
directly rather than a local type alias; an unsupported name produces a compile
error naming the state. The shared binding type-checks the handler and owns all
wire validation, message decoding, clipping and reply encoding.

The host-compiled `mac-definition-macros` crate generates the C ABI entry, the
RV64 `_start` in `.text.entry`, and the definition panic handler. It adds no
guest allocator or loading path. The loader still enters `_start` in the
runtime-loaded resource. Panics use the existing bounded diagnostic and report
to System, then fault if reporting returns; they do not unwind across the ABI.
The native development ABI remains version 1.

Inspect the complete guest expansion with nightly Rust, without installing an
additional tool:

```sh
CARGO_TARGET_DIR=/tmp/system-risc-cdef-expand \
  cargo rustc --locked --release --target riscv64imac-unknown-none-elf \
  -p native-buttons-cdef --features guest --bin CDEF_BUTTONS \
  -- -Zunpretty=expanded
```

Control acceptance runs in the disk-booted RV64 public-SDK conformance fixture,
which uses the production resource loader. Host-linked CDEF validation is
retired. The separate definition-panic fixture exercises reporting and the
terminal Restart path in a private guest. Unit checks of diagnostic formatting
do not establish guest runtime behavior.

MDEF and WDEF retain their existing author entry forms. Their distinct decoders
require a separate migration.
