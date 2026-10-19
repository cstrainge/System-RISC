# Original expectations and generated no-std fixtures

Original bytes come from the independently compiled68k probes running original
System7.6.1, not from native renderer/manager output. Source/runtime provenance:
`provenance/memory-manager-reference.json`, `results/memory-reference/cases.json`,
`results/memory-reference/actions.json`,
`results/quickdraw-reference/edge-corpus.json` and its referenced runtime report.
The copies here make aggregate tests independent of live QEMU/reference disks.

`original-ovals.bin` format:

- Big-endian u16 record count267.
- For each record: signed big-endian i16 width,height; big-endian u16 byte length;
  that many original big-endian QuickDraw region bytes.
- Total length21,440 bytes. No padding or native reconstruction bytes.

`conformance/memory_oracle.rs` contains the48 handle rows from the normalized
original JSON. Sequence gaps1/3/5 are the three deferred NewPtrClear rows.
Host tests compare every compiled oracle field and known payload with JSON;
undefined size/state outputs on error are not converted into numeric expectations.

The geometry/region shared function has20 grouped contracts:8 captured original
region encodings,3 basic rectangle contracts,5 mapping/error contracts,1 boundary
contract and3 native validation/output-buffer contracts. These categories remain
separate in the ledger: not every native error/buffer contract is an independently
measured original Macintosh operation.

`initial-memory-red-run.json` records the intentional failing first run while
resident Memory Manager returned NOT_SUPPORTED. `conformance-ledger.json` records
the later host/actual ECALL results, precise case coverage and deferred behavior.
The physical original heap's allocation policy was not inferred from those tests.

The control corpus is packaged by `capture_controls.py` directly from independently captured original System 7.6.1 frames and hit logs. `original-controls.json` records source screenshot, crop and blob hashes. The binary pixel files contain palettes and run lengths, not native renderer output. The aggregate shared functions compare every pixel in each complete control rectangle and check borrowed storage outside it remains untouched. The 8,424 hit records include repeated probe passes; 1,296 sampled tuples are distinct. Static CDEF highlights do not establish `TrackControl` animation parity; `control-scope-ledger.json` records that separate gap.
