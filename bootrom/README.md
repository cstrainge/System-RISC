# System RISC boot ROM

Native RV64IMAC firmware for QEMU `virt`, executing from a read-only parallel
flash bank at **0x20000000**. QEMU's reset stub transfers control directly to
our firmware. There is no OpenSBI, Linux, `-kernel`, or host extraction of
System code in this boot path.

From the `System7` workspace:

```sh
cargo xtask rom
cargo xtask boot
cargo xtask boot ../build/system7/dist/system7.hfsb
cargo xtask boot-check
```

`rom` produces `../build/system7/dist/bootrom.elf` and `bootrom.bin`. The
executable payload is about 220 KiB; the raw image is padded to the **32 MiB**
bank size required by QEMU. Debug information stays in the host ELF artifact.

`boot` without a disk path rebuilds System/Finder and the generated 500 MiB
HFS-B/qcow2 volume. With a disk path it builds just the ROM and opens that disk.
Both flash and disk are attached **read-only**. QEMU runs headless with its
UART connected to the terminal; Ctrl+C stops it. It does not change the
currently visible QuickDraw or classic-Mac QEMU sessions.

## Startup sequence

1. Park secondary harts, initialize the stack and trap handler, copy initialized
   data from flash to RAM, clear BSS, and initialize the firmware allocator.
2. Scan the board's eight virtio-MMIO slots for block devices. Negotiate modern
   virtio, initialize a split request queue, and read sectors with bounded waits.
3. Find unpartitioned HFS-B volumes or `System_RISC_HFS_B` entries in a512-byte
   Apple partition map. Classic HFS remains mountable by System but is rejected
   as the native startup format with a recreate-disk diagnostic.
4. Read the checked HFS-B header's blessed catalog ID. Resolve that actual folder;
   its name is not significant. Disk version1 fixes the Unicode17.0.0 contract.
5. Find `System` (`zsys`) and `Finder` (`FNDR`) within the blessed folder.
   Read their resource maps and selected NABI/RV64/RLOC payloads through bounded
   fork ranges, never by copying an entire resource fork.
6. Validate both executable descriptors, segment bounds, roles, ABI and relocation
   plans. Display the original Happy Mac, copy/zero the System reservation,
   execute `fence.i`, and enter System with firmware block callbacks and the
   selected disk/partition identity.
7. System mounts the same volume using its resident File Manager, loads resources
   and Finder, and configures its framebuffer. Its resource-based welcome panel
   reuses the desktop background. Seven completed initialization stages fill a
   striped progress bar by changing only newly filled pixels. No artificial
   boot delay or per-tick full-screen redraw is introduced.
8. System loads definition resources, publishes services and launches Finder in
   its OS thread. Valid boots reach `DESKTOP_READY`. Firmware callbacks remain
   available for resident disk I/O; there is no whole-fork startup callback.

Invalid volumes or startup files produce a specific UART rejection and the
search continues. If nothing boots, `BOOT_NO_SYSTEM` is printed and the ROM
shows the original question-mark disk and parks. Traps, allocation failures and panics have distinct diagnostic markers.
An original 68k System file does not satisfy this native executable contract.

## Hardware and initial limits

- QEMU `virt`, RV64, 128 MiB RAM, one active hart, M-mode, identity mapping.
- UART at `0x10000000`, virtio-MMIO at `0x10001000..0x10008000`, 10 MHz CLINT
  timer, and flash at `0x20000000`.
- The low 2 MiB of RAM is reserved for firmware state and its 256 KiB stack.
  Firmware uses a separate 8 MiB scratch heap at `0x87000000..0x87800000`;
  System loading excludes this entire high-memory region and the FDT.
  System's own 8 MiB heap is within its executable reservation;
  `BootInfo` currently reports total RAM, not a general memory map.
- The System reservation must fit `0x80200000..0x87000000`. ROM owns one pinned
  640×480 RAM framebuffer inside its low2MiB reservation until System takes over.
- Resource maps are limited to256KiB and selected payloads to4MiB per load. A
  resource fork may be larger: unselected bytes/padding are not read or allocated.
  Explicit metadata/resource budgets bound work rather than total fork length.
  Resident sector caching bounds repeated metadata I/O, including native Windows
  QEMU reads over the WSL filesystem; bootstrap need not extend its25-second
  launcher deadline to compensate for redundant reads.
- `mac-filesystem` dispatches classic HFS and HFS-B through shared block traits.
  HFS-B headers, catalog/extent/attribute trees, names and writes are implemented
  by `mac-hfs-b`, also used by the Linux host tool and image builder. Boot uses
  read-only operations. APM enumeration is bounded to128 entries. Unsupported
  format/Unicode versions, dirty headers and malformed maps are rejected.
- The ROM has no HFS+, GPT/MBR, SCSI, legacy virtio or general memory-map support.
- Segment permissions are validated; no MMU/PMP protection is installed
  in this initial M-mode firmware.

The driver also exposes sector writes and explicit flushes to the resident File
Manager when the device is writable and supports FLUSH. It resets a device before
freeing its DMA queue. The filesystem and executable parsing code is reusable by
future native disk tools; it has no host subprocess or filesystem dependency.

Requests have bounded waits. A timeout, failed device, or invalid completion
poisons the queue permanently: its descriptors and data cannot be reused while
DMA completion is uncertain. `DISK_REQUEST_FAILED` records the first failure's
operation, sector, submitted/used indices, completion head/length, request/device
status, elapsed microseconds and poison decision. The firmware callbacks also
record write/flush errors; later rollback errors must not hide the initial cause.

## Verification

`cargo xtask boot-check` cold-starts QEMU using the ROM and attached disks. Its
cases cover no disk, raw HFS-B, qcow2 HFS-B, a renamed blessed folder, no blessing,
a System resource fork beyond4MiB, classic-HFS startup rejection,
missing System or Finder, malformed segments, obsolete ELF/NABI layouts, wrong
executable role, a firmware-overlapping
memory reservation, an Apple partition map, and fallback after an unblessed
disk or invalid System. Each VM is stopped after reaching its expected UART
marker. Valid boots wait for Finder's `DESKTOP_READY`, including loaded native
definitions and first window updates. The original generated qcow2 is checked
for changes. `python3 ../scripts/system7_vm.py start` opens the separate
graphical machine; its `check --no-build` exercises desktop input and compositing.

The shared HFS crate additionally has host tests for fragmented resource forks
spanning overflow extents, linked catalog leaves, out-of-range partitions,
dangling blessings, invalid geometry, bad record offsets and cyclic chains.

Results: `../results/system7/boot-checks.json`; per-case serial output:
`../build/system7/boot-checks/`. The full `cargo xtask check` includes these ROM
checks plus the original packaging, disk roundtrip and native ABI tests.

## References

- [QEMU RISC-V virt firmware and flash boot](https://www.qemu.org/docs/master/system/riscv/virt.html)
- [QEMU 10.1 reset-vector selection](https://github.com/qemu/qemu/blob/v10.1.0/hw/riscv/virt.c)
- [Virtio 1.2 specification](https://docs.oasis-open.org/virtio/virtio/v1.2/virtio-v1.2.html)
- [Apple: HFS data organization](https://dev.os9.ca/techpubs/mac/Files/Files-99.html)

The ABI6 firmware opens supported block devices before choosing a startup
volume, retains their queues, and lends System disk-info/read/write/flush
callbacks. System mounts HFS-B and non-startup classic HFS volumes through the
same interface. Resident startup reads use the File Manager; there is no
whole-fork startup callback. File Manager calls are serialized by the
firmware-context owner.
Virtio OUT and negotiated FLUSH support durable catalog/Trash operations; readonly
media stay readonly. An uncertain completion poisons the device queue to prevent
reusing descriptors while DMA may still be active. The desktop launcher attaches
its private working disk writable; test launches default to readonly snapshots.
See [HFS write scope](../docs/HFS-WRITES.md).
