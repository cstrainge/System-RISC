# RISCy System boot ROM

Native RV64IMAC firmware for QEMU `virt`, executing from a read-only parallel
flash bank at **0x20000000**. QEMU's reset stub transfers control directly to
our firmware. There is no OpenSBI, Linux, `-kernel`, or host extraction of
System code in this boot path.

From the `System7` workspace:

```sh
cargo xtask rom
cargo xtask boot
cargo xtask boot ../build/system7/dist/system7.hfs
cargo xtask boot-check
```

`rom` produces `../build/system7/dist/bootrom.elf` and `bootrom.bin`. The
executable payload is about 40 KiB; the raw image is padded to the **32 MiB**
bank size required by QEMU. Debug information stays in the ELF.

`boot` without a disk path rebuilds System/Finder and the generated 500 MiB
HFS/qcow2 volume. With a disk path it builds just the ROM and opens that disk.
Both flash and disk are attached **read-only**. QEMU runs headless with its
UART connected to the terminal; Ctrl+C stops it. It does not change the
currently visible QuickDraw or classic-Mac QEMU sessions.

## Startup sequence

1. Park secondary harts, initialize the stack and trap handler, copy initialized
   data from flash to RAM, clear BSS, and initialize the firmware allocator.
2. Scan the board's eight virtio-MMIO slots for block devices. Negotiate modern
   virtio, initialize a split request queue, and read sectors with bounded waits.
3. Look for an unpartitioned classic HFS volume or `Apple_HFS` entries in a
   512-byte Apple partition map. Try eligible volumes in disk/partition order.
4. Read the master directory block's `drFndrInfo[0]` blessed directory ID. Find
   that actual directory record in the catalog. Its name is not significant.
5. Find `System` inside that directory, requiring file type `zsys`. Read its
   resource fork through its extents, including ordinary overflow extent records.
6. Validate `NABI` resource 0 as a compatible native System. Validate `RV64`
   resource 0 as a supported ELF, including segment bounds and permissions.
7. Copy loadable segments to RAM, zero BSS, execute `fence.i`, and call the ELF
   entry with `BootInfo`, logging and a read-only startup-volume callback.
8. System reads Finder from that same blessed folder through the callback,
   validates and loads it, initializes graphics/input, and loads native WDEF/CDEF
   executables from resources. It prints `BOOT_SYSTEM_READY`, launches Finder,
   and remains in the application's event loop. Finder prints `DESKTOP_READY`
   after opening and painting its windows. If System returns, firmware validates
   its resident service table and then parks.

Invalid volumes or startup files produce a specific UART rejection and the
search continues. If nothing boots, `BOOT_NO_SYSTEM` is printed and the ROM
parks. Traps, allocation failures and panics have distinct diagnostic markers.
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
- The System reservation must fit `0x80200000..0x87000000`. System resource
  forks are limited to 4 MiB to bound firmware memory use. The ROM borrows the
  resource map and loads only the executable; fonts and visual resources are
  interpreted by System after startup.
- HFS access is read-only. The shared `mac-hfs` crate follows checked linked
  leaf records, supports fragmented resource/catalog forks with overflow
  extents, and rejects invalid bounds, cycles and missing records.
- Limits: 128 APM entries, 65,536 leaf-scan iterations per search, and 1,024
  overflow extents. Growth of the extents-overflow tree beyond its own initial
  three extents is unsupported. HFS+, GPT/MBR, SCSI and legacy virtio are not
  implemented. Unsupported media produces a rejection, not a partial boot.
- ELF segment permissions are validated; no MMU/PMP protection is installed
  in this initial M-mode firmware.

The driver issues only block read requests and resets a device before freeing
its DMA queue. The filesystem and executable parsing code is reusable by
future native disk tools; it has no host subprocess or filesystem dependency.

## Verification

`cargo xtask boot-check` cold-starts QEMU using the ROM and attached disks. Its
cases cover no disk, raw HFS, qcow2 HFS, a renamed blessed folder, no blessing,
missing System or Finder, invalid ELF, wrong executable role, a firmware-overlapping
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

The ABI5 firmware opens supported block devices before choosing a startup
volume, retains their queues, and lends System disk-info/read/write/flush
callbacks. System mounts non-startup HFS volumes through the same interface.
The startup callback still reads executable resource forks from the blessed
folder. Resident File Manager calls are serialized by the firmware-context owner.
Virtio OUT and negotiated FLUSH support durable catalog/Trash operations; readonly
media stay readonly. An uncertain completion poisons the device queue to prevent
reusing descriptors while DMA may still be active. The desktop launcher attaches
its private working disk writable; test launches default to readonly snapshots.
See [HFS write scope](../docs/HFS-WRITES.md).
