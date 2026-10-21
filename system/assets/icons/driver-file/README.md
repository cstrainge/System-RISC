# Native driver document icon — family -30001

The default DRVR icon is the original System 7 extension puzzle (family -16415)
with the chip from the user's Drivers folder (family -30000) centered on it. The
original System and supplied folder resources are preserved unchanged. This is
resource-pixel composition, not generated replacement artwork.

32×32 and 16×16 each use their original-size chip pixels. Color indices are copied
verbatim from the supplied `icl8`/`ics8`; the separate `ICN#`/`ics#` chip drawing
is copied verbatim for monochrome, including its solid center. Puzzle silhouettes
and transparency masks are unchanged. No resampling or color-to-mono conversion.
The four PNGs are inspectable previews of the actual resource payloads.

Regenerate with `python3 scripts/build_driver_file_icon.py`. `sources.json` records
source payload hashes and placement. The script's explicit chip coverage excludes
the folder background and preserves puzzle pixels between pins.

The accompanying MACS BNDL/FREF adds the generic DRVR type mapping without
rewriting original System resources. Resident icon lookup first resolves a
creator-specific bundle within its resource fork; if absent it consults System's
generic type bundle. This serves Finder and system file dialogs, for driver files
of any creator, without app-local icon special cases.

## Resource ID reservation

System RISC reserves icon family IDs **-30031 through -30000** for native System
additions: Drivers folder **-30000**, driver file **-30001**. These do not reuse or
continue Apple's standard icon assignments. Positive application resource IDs
remain available in their own resource forks; no old positive aliases are kept.

The checked reference set is recorded in
`crates/mac-toolbox/tests/data/classic-system-icon-ids.json`: complete icon maps
from the original Mac OS 7.6.1 System and Finder, and the published Apple icon
constants in Universal Interfaces 3.4 (2001) and the later 10.4u header retaining
classic definitions. None intersects the reservation. Full private System/Finder
resource inventories for OS 8/9 have not been checked; published SDK constants
are not an inventory of every localized release's private resources.

The canonical SDK tests verify the reservation against those references and the
current System manifest; mac-build rejects duplicate type/ID imports. Importing
more original resources must preserve this collision gate and their source IDs,
never silently replace original or native artwork. REA's active artifact opener
rejected the MacBinary reference format; original map IDs were inventoried with
the existing resource-fork parser, without executing any reference application.
