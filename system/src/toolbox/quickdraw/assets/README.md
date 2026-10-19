# Classic font strikes

`chicago-12.font` is FONT 12 from the pinned Quadra 650 ROM. `geneva-9.font`
is FONT 393 from the installed System 7.6.1 snapshot. Their source hashes,
byte offsets, extracted hashes and metrics are retained in
[`provenance/system7-font-assets.json`](../../../../../../provenance/system7-font-assets.json).
That historical manifest records the former client-crate paths. The strikes now
live here in resident System; applications and definitions receive font services
through the trap gate and do not embed these bitmap resources.
Re-extract with `python3 scripts/extract_standard_wdef_assets.py --install-fonts`
from the repository root. Extraction verifies the original file identities first.

These are original bitmap data, rendered by our Rust implementation. They keep
case, integer advances, bearings and baseline placement. The renderer validates
FONT/NFNT headers and table bounds, maps UTF-8 characters to Mac Roman and
preserves the drawing port's origin and clip. It supports one-bit strikes;
outline scaling, styled text and a complete Font Manager remain future work.
The format follows Apple's
[bitmapped font resource specification](https://dev.os9.ca/techpubs/mac/Text/Text-250.html).
