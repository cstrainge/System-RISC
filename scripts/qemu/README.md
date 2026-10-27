# QEMU Cocoa scaling

The macOS launcher uses `-display cocoa,zoom-to-fit=off,scale=2` only when the
whole 1600×1200 window content area fits on the active screen. It queries
AppKit once at launch, excludes the menu bar and Dock, and reserves the Cocoa
window's title bar. Otherwise it requests `scale=1` for an 800×600 content area.
If screen measurement fails, it reports the cause and selects 1×. The decision
uses the selected platform's guest dimensions. Sizes are macOS window points;
Retina backing pixels follow the display's own scale. Guest geometry and native
tablet coordinates remain unchanged. Headless launches do not use this option.

Upstream QEMU's Cocoa backend does not expose a fixed window scale. The adjacent
`cocoa-scale.patch` adds that option to the original QEMU 10.1.0 sources from
[QEMU's release archive](https://download.qemu.org/qemu-10.1.0.tar.xz).
It scales the existing fixed-size window path, preserving Cocoa's dirty-region
drawing and absolute-input mapping. The scale defaults to 1 and must be between
1 and 4; invalid values report an error before creating a Cocoa window.

Apply the patch before building a Cocoa-enabled standalone QEMU installation:

```sh
patch -d /path/to/qemu-10.1.0 -p1 < /path/to/System-RISC/scripts/qemu/cocoa-scale.patch
ninja -C /path/to/qemu-build qemu-system-riscv64
ninja -C /path/to/qemu-build install
```

Put that installation's `qemu-system-riscv64` and `qemu-img` on `PATH`.
An unpatched Cocoa binary rejects the requested `scale` option; the launcher
reports QEMU's error and reaps the failed guest. It does not silently open a
different-sized window. The running guest must be relaunched to use a replaced
QEMU binary.
