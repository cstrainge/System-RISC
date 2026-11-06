#!/usr/bin/env python3
"""Derive the Pipes icon families from the System's own standard artwork.

The `Pipes` folder family (-30002) is the original standard folder family
(-3999) with a pipe motif added; the named-pipe document family (-30003) is the
original standard document family (-4000) with the same motif. Both source
families are the project's own resources, extracted into
`system/extracted_assets`, so nothing here comes from outside the repository.

Writing the PNGs as well as the resource payloads keeps the artwork reviewable;
the PNGs are the source, the `.bin` files are the payloads the manifest names.

    python3 scripts/make_pipes_icons.py --repo .
"""
import argparse, os, sys

from PIL import Image, ImageDraw

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__))))
import iconconvert

TYPES = {"icn": "ICN#", "icl8": "icl8", "ics": "ics#", "ics8": "ics8"}


def payload(repo, rtype, iid):
    name = {("ICN#", -3999): "ICN_--3999-ORIGINAL_VISUAL_49434E23_N3999.bin",
            ("icl8", -3999): "icl8--3999-ORIGINAL_VISUAL_69636C38_N3999.bin",
            ("ics#", -3999): "ics_--3999-ORIGINAL_VISUAL_69637323_N3999.bin",
            ("ics8", -3999): "ics8--3999-ORIGINAL_VISUAL_69637338_N3999.bin",
            ("ICN#", -4000): "ICN_--4000-ORIGINAL_VISUAL_49434E23_N4000.bin",
            ("icl8", -4000): "icl8--4000-ORIGINAL_VISUAL_69636C38_N4000.bin",
            ("ics#", -4000): "ics_--4000-ORIGINAL_VISUAL_69637323_N4000.bin",
            ("ics8", -4000): "ics8--4000-ORIGINAL_VISUAL_69637338_N4000.bin"}[rtype, iid]
    return open(os.path.join(repo, "system", "extracted_assets", name), "rb").read()


def source_images(repo, iid):
    """The family as four `(size, mono)` images, decoded from the originals."""
    pal = iconconvert.load_palette(repo)
    out = {}
    for size, mono_type, color_type in ((32, "ICN#", "icl8"), (16, "ics#", "ics8")):
        mono = payload(repo, mono_type, iid)
        color = payload(repo, color_type, iid)
        out[size, True] = iconconvert.bits_to_alpha(mono[: size * size // 8],
                                                    mono[size * size // 8:],
                                                    size, size, size // 8)
        out[size, False] = iconconvert.color_icon(color, mono, pal, size)
    return out


def pipe(draw, size, mono):
    """A pipe spanning the icon body: two flanges joined by a tube.

    Both variants use the same geometry; only the interior fill differs, because
    the 1-bit family has no palette and can express just black and white.
    """
    body = (0, 0, 0) if mono else (68, 68, 68)
    edge = (0, 0, 0)
    shine = (255, 255, 255)
    if size == 32:
        left, right, top = 8, 24, 15
    else:
        left, right, top = 4, 12, 7
    height = 3 if size == 32 else 2
    # Flanges first, so the tube's ends read as open pipe mouths.
    draw.rectangle([left, top - 1, left + 1, top + height], fill=edge)
    draw.rectangle([right - 1, top - 1, right, top + height], fill=edge)
    draw.rectangle([left + 2, top, right - 2, top + height - 1], fill=body)
    draw.line([left + 2, top, right - 2, top], fill=shine)


def build(repo, base, iid):
    out = os.path.join(repo, "system", "assets", "icons", base)
    os.makedirs(out, exist_ok=True)
    images = source_images(repo, iid)
    pngs = {}
    for size in (32, 16):
        for mono in (True, False):
            image = images[size, mono].copy()
            pipe(ImageDraw.Draw(image), size, mono)
            pngs[size, mono] = image
    return out, pngs


def emit(repo, base, target_id, iid):
    out, pngs = build(repo, base, iid)
    pal = iconconvert.load_palette(repo)
    written = []
    for size in (32, 16):
        for mono, suffix in ((True, "1bit"), (False, "color")):
            name = f"{base}-{size}-{suffix}.png"
            pngs[size, mono].save(os.path.join(out, name))
            written.append(name)
    # Monochrome payloads come from the 1-bit artwork, colour payloads from the
    # colour artwork, so neither is derived from the other by conversion.
    _, mono32 = iconconvert.png_to_bins(os.path.join(out, f"{base}-32-1bit.png"), pal, 32)
    _, mono16 = iconconvert.png_to_bins(os.path.join(out, f"{base}-16-1bit.png"), pal, 16)
    color32, _ = iconconvert.png_to_bins(os.path.join(out, f"{base}-32-color.png"), pal, 32)
    color16, _ = iconconvert.png_to_bins(os.path.join(out, f"{base}-16-color.png"), pal, 16)
    bins = {"ICN#": mono32, "ics#": mono16, "icl8": color32, "ics8": color16}
    for rtype, data in bins.items():
        name = f"{iconconvert.TYPEHEX[rtype]}--{abs(target_id)}.bin"
        open(os.path.join(out, name), "wb").write(data)
        written.append(name)
    return written


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--repo", required=True)
    args = ap.parse_args()
    written = emit(args.repo, "pipes", -30002, -3999)
    written += emit(args.repo, "named-pipe", -30003, -4000)
    print("\n".join(written))


if __name__ == "__main__":
    main()
