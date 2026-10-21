# Drivers folder icon

User-supplied artwork, created from the original standard folder family (`-3999`)
with `scripts/iconconvert.py`. The four PNGs are preserved source artwork copied
from `temp/icon-m3999-{16,32}-{color,1bit}.png` on 2026-10-20.

System resource family **-30000** contains `ICN#`/`icl8` at32×32 and `ics#`/`ics8`
at16×16. Color pixels use the existing System `clut`8 palette, with nearest-color
quantization by the converter. Monochrome bitmap and mask payloads come from the
supplied `1bit` PNGs, rather than a luminance conversion of the color artwork.
Normal, selected and open states use the existing Icon Utilities transformations.

Regenerate the color payloads with:

```sh
python3 scripts/iconconvert.py import --repo . --id -30000 \
  --out system/assets/icons/drivers \
  --png32 system/assets/icons/drivers/drivers-32-color.png \
  --png16 system/assets/icons/drivers/drivers-16-color.png
```

Then replace the generated monochrome payloads with the second result of
`iconconvert.png_to_bins` applied to each corresponding `1bit` PNG, using the
same palette and size. Keep the four resource types on the same family ID.
