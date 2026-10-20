#!/usr/bin/env python3
"""iconconvert.py — round-trip classic Mac icon resources <-> PNG for System RISC.

The System file and Finder store icons as raw resource payloads (.bin):
  ICN#  32x32 1-bit: 256 bytes = 128 bitmap + 128 mask
  icl8  32x32 8-bit: 1024 bytes of indices into the system palette (clut 8)
  ics#  16x16 1-bit: 64 bytes = 32 bitmap + 32 mask
  ics8  16x16 8-bit: 256 bytes of indices

Usage:
  # export one icon family (all four variants) from a resources tree
  python3 iconconvert.py export --repo ~/system-risc --app system --id -3999 --out .
  # export every icl8 in an app as 32x32 color PNGs
  python3 iconconvert.py export-all --repo ~/system-risc --app system --out ./png
  # convert edited PNGs back to .bin payload files (writes into --out)
  python3 iconconvert.py import --png32 color32.png --png16 color16.png --out ./bins --id -3999

The palette is read from the repo's own clut 8 resource, so exports are
index-faithful: PNG -> icl8 round-trips byte-exact as long as edited pixels
use palette colors (Aseprite: load the palette from the exported PNG and
paint with it). The 1-bit bitmap is regenerated on import from luminance
(outlines/badges survive; interior shading does not) — if you only changed
colors inside the silhouette, keep the original ICN#/ics# .bin and ship
only the new icl8/ics8.
"""
import argparse, json, os, struct, sys
from PIL import Image

TYPEHEX = {"ICN#": "49434e23", "icl8": "69636c38", "ics#": "69637323",
           "ics8": "69637338", "clut": "636c7574"}

def load_palette(repo):
    path = os.path.join(repo, "system/assets/visual", TYPEHEX["clut"] + "-8.bin")
    data = open(path, "rb").read()
    pal = []
    for i in range(256):
        _, r, g, b = struct.unpack(">HHHH", data[8 + i * 8: 16 + i * 8])
        pal.append((r >> 8, g >> 8, b >> 8))
    assert pal[0] == (255, 255, 255) and pal[255] == (0, 0, 0)
    return pal

def resource_paths(repo, app):
    man = json.load(open(os.path.join(repo, app, "resources.json")))
    out = {}
    for r in man["resources"]:
        p = r.get("payload", {})
        if isinstance(p, dict) and "path" in p:
            out[(r["type"], str(r["id"]))] = os.path.join(repo, app, p["path"])
    return out

def bits_to_alpha(bmp, mask, w, h, rowbytes):
    img = Image.new("RGBA", (w, h), (0, 0, 0, 0))
    px = img.load()
    for y in range(h):
        for x in range(w):
            m = (mask[y * rowbytes + x // 8] >> (7 - x % 8)) & 1
            b = (bmp[y * rowbytes + x // 8] >> (7 - x % 8)) & 1
            if m:
                v = 0 if b else 255
                px[x, y] = (v, v, v, 255)
    return img

def color_icon(idx_bytes, icn_bytes, pal, size):
    rb = size // 8
    img = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    px = img.load()
    bmp = icn_bytes[: size * rb]
    mask = icn_bytes[size * rb: 2 * size * rb]
    for y in range(size):
        for x in range(size):
            m = (mask[y * rb + x // 8] >> (7 - x % 8)) & 1
            if m:
                px[x, y] = pal[idx_bytes[y * size + x]] + (255,)
    return img

def export_family(repo, app, iid, outdir):
    pal = load_palette(repo)
    paths = resource_paths(repo, app)
    os.makedirs(outdir, exist_ok=True)
    written = []
    def save(img, name):
        img.save(os.path.join(outdir, name)); written.append(name)
    tag = f"m{-iid}" if iid < 0 else str(iid)
    for typ, size in (("icl8", 32), ("ics8", 16)):
        key = (typ, str(iid)); bkey = ("ICN#" if size == 32 else "ics#", str(iid))
        if key in paths and bkey in paths:
            img = color_icon(open(paths[key], "rb").read(),
                             open(paths[bkey], "rb").read(), pal, size)
            save(img, f"icon-{tag}-{size}-color.png")
    for typ, size in (("ICN#", 32), ("ics#", 16)):
        key = (typ, str(iid))
        if key in paths:
            raw = open(paths[key], "rb").read()
            rb = size // 8
            save(bits_to_alpha(raw[:size * rb], raw[size * rb:], size, size, rb),
                 f"icon-{tag}-{size}-1bit.png")
    return written

def nearest(pal, rgb):
    best, bd = 0, None
    for i, c in enumerate(pal):
        d = sum((a - b) ** 2 for a, b in zip(c, rgb))
        if bd is None or d < bd:
            best, bd = i, d
    return best

def png_to_bins(png_path, pal, size):
    img = Image.open(png_path).convert("RGBA").resize((size, size), Image.NEAREST)
    px = img.load()
    idx = bytearray(size * size)
    rb = size // 8
    bmp = bytearray(size * rb); mask = bytearray(size * rb)
    for y in range(size):
        for x in range(size):
            r, g, b, a = px[x, y]
            if a >= 128:
                mask[y * rb + x // 8] |= 1 << (7 - x % 8)
                idx[y * size + x] = nearest(pal, (r, g, b))
                if (0.299 * r + 0.587 * g + 0.114 * b) < 170:
                    bmp[y * rb + x // 8] |= 1 << (7 - x % 8)
    return bytes(idx), bytes(bmp) + bytes(mask)

def main():
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="cmd", required=True)
    e = sub.add_parser("export"); e.add_argument("--repo", required=True)
    e.add_argument("--app", default="system"); e.add_argument("--id", type=int, required=True)
    e.add_argument("--out", required=True)
    ea = sub.add_parser("export-all"); ea.add_argument("--repo", required=True)
    ea.add_argument("--app", default="system"); ea.add_argument("--out", required=True)
    im = sub.add_parser("import"); im.add_argument("--repo", required=True)
    im.add_argument("--id", type=int, required=True); im.add_argument("--out", required=True)
    im.add_argument("--png32"); im.add_argument("--png16")
    a = ap.parse_args()
    if a.cmd == "export":
        print("\n".join(export_family(a.repo, a.app, a.id, a.out)))
    elif a.cmd == "export-all":
        pal = load_palette(a.repo); paths = resource_paths(a.repo, a.app)
        os.makedirs(a.out, exist_ok=True)
        n = 0
        for (typ, iid), p in sorted(paths.items()):
            if typ == "icl8" and ("ICN#", iid) in paths:
                img = color_icon(open(p, "rb").read(),
                                 open(paths[("ICN#", iid)], "rb").read(), pal, 32)
                img.save(os.path.join(a.out, f"icl8-{iid}.png")); n += 1
        print(f"exported {n} icons")
    elif a.cmd == "import":
        pal = load_palette(a.repo)
        os.makedirs(a.out, exist_ok=True)
        jobs = []
        if a.png32: jobs.append((a.png32, 32, "69636c38", "49434e23"))
        if a.png16: jobs.append((a.png16, 16, "69637338", "69637323"))
        for png, size, chex, bhex in jobs:
            idx, bits = png_to_bins(png, pal, size)
            open(os.path.join(a.out, f"{chex}-{a.id}.bin"), "wb").write(idx)
            open(os.path.join(a.out, f"{bhex}-{a.id}.bin"), "wb").write(bits)
            print(f"wrote {chex}-{a.id}.bin (color) and {bhex}-{a.id}.bin (1-bit+mask)")

if __name__ == "__main__":
    main()
