# Panic Test assets

`application.icn` uses the exact 32×32 bomb bitmap from the original System
DSAT resource 0, item -16397. Its mask repeats the bitmap, leaving surrounding
white transparent. `application-small.icn` derives a 16×16 version by retaining
a black pixel when any pixel in its corresponding 2×2 source block is black;
its mask likewise repeats the image. These are resource-fork ICN#/ics# payloads.

The original payload and source offsets/hashes are recorded in
[`system/assets/crashes/classic-bomb.provenance.json`](../../../system/assets/crashes/classic-bomb.provenance.json).
Original bitmap SHA-256:
`2fb9435fea3c42af185d791c415fb26b1529e5763dab47637dc469adc06e60b4`.

`long-panic.txt` is generated diagnostic text, deliberately larger than 20 KiB.
Its numbered UTF-8 lines and final `END_OF_LONG_PANIC` marker expose truncation,
wrapping, and scrolling defects in the shared panic report viewer.
