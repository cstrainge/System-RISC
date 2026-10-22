#!/usr/bin/env python3
"""Author the classic archive document directly in native icon pixels.

Independent 16/32 geometry and explicit mono ink preserve the zipper at either
size. This generates resource payloads; PNGs are decoder previews, not inputs.
"""
import hashlib
import json
from pathlib import Path
import struct
import iconconvert

ROOT = Path(__file__).resolve().parents[1]
DEST = ROOT / 'drivers/stuffit-filesystem/assets/icons'
FAMILY = 128


def draw(size):
    pixels = {}
    def pixel(x, y, color, ink):
        assert 0 <= x < size and 0 <= y < size
        pixels[x, y] = (color, ink)
    def rect(left, top, right, bottom, color, ink):
        for y in range(top, bottom + 1):
            for x in range(left, right + 1):
                pixel(x, y, color, ink)
    if size == 32:
        left, top, right, bottom, fold = 5, 2, 25, 29, 18
    else:
        left, top, right, bottom, fold = 2, 0, 12, 14, 8
    def page(x, y):
        return left <= x <= right and top <= y <= bottom and (x <= fold or y >= top + x - fold)
    # One-pixel silhouette shadow; monochrome uses deliberate checker ink.
    for y in range(size):
        for x in range(size):
            if page(x-1, y-1):
                pixel(x, y, (153, 153, 153), (x+y) % 2 == 0)
    for y in range(size):
        for x in range(size):
            if page(x, y):
                edge = any(not page(x+dx, y+dy) for dx, dy in [(1,0),(-1,0),(0,1),(0,-1)])
                pixel(x, y, (0,0,0) if edge else (255,255,255), edge)
    corner_y = top + right - fold
    for y in range(top+1, corner_y):
        for x in range(fold+1, right):
            if page(x,y): pixel(x,y,(204,204,255),False)
    rect(fold,top+1,fold,corner_y,(0,0,0),True)
    rect(fold,corner_y,right-1,corner_y,(0,0,0),True)
    if size == 32:
        # Compressed contents remain legible on either side of the zipper.
        for y in (17,21,25):
            rect(8,y,12,y,(51,102,153),True)
            rect(20,y,22,y,(51,102,153),True)
        rect(15,14,17,26,(102,102,102),True)
        for y in range(16,27,2):
            pixel(14,y,(0,0,0),True)
            pixel(15,y,(255,255,255),False)
        for y in range(15,26,2):
            pixel(18,y,(0,0,0),True)
            pixel(17,y,(255,255,255),False)
        rect(14,10,18,14,(0,0,0),True)
        rect(15,11,17,13,(255,204,102),False)
        pixel(16,12,(0,0,0),True)
    else:
        # Small family is drawn independently, without shrinking fine detail.
        rect(7,7,8,12,(102,102,102),True)
        for y in (8,10,12):
            pixel(6,y,(0,0,0),True)
            pixel(7,y,(255,255,255),False)
        for y in (7,9,11):
            pixel(9,y,(0,0,0),True)
            pixel(8,y,(255,255,255),False)
        for y in (9,12):
            rect(4,y,5,y,(51,102,153),True)
            rect(10,y,10,y,(51,102,153),True)
        rect(6,4,9,6,(0,0,0),True)
        rect(7,5,8,5,(255,204,102),False)
    return pixels


def build():
    palette = iconconvert.load_palette(str(ROOT))
    DEST.mkdir(parents=True, exist_ok=True)
    for size, mono, color in [(32, 'ICN#', 'icl8'), (16, 'ics#', 'ics8')]:
        pixels = draw(size)
        plane = size * size // 8
        bits = bytearray(2 * plane)
        indices = bytearray(size * size)
        for (x,y),(rgb,ink) in pixels.items():
            at, bit = y*(size//8)+x//8, 1 << (7-x%8)
            bits[plane+at] |= bit
            if ink: bits[at] |= bit
            indices[y*size+x] = iconconvert.nearest(palette,rgb)
        (DEST / f'{iconconvert.TYPEHEX[mono]}-{FAMILY}.bin').write_bytes(bits)
        (DEST / f'{iconconvert.TYPEHEX[color]}-{FAMILY}.bin').write_bytes(indices)
    # A native association record, not a Finder filename convention. Exact
    # file codes win; the extension hint classifies raw/untyped host imports.
    rules = []
    for code in [b'SIT!', b'SITD', b'SIT2', b'SIT5']:
        rules.append(struct.pack('>4sHhI',code,1,FAMILY,0))
    (DEST.parent / 'file-associations.bin').write_bytes(b'FAS1'+struct.pack('>HH',1,len(rules))+b''.join(rules))
    (DEST/'sources.json').write_text(json.dumps({'author':'System RISC',
        'design':'folded document with compressed contents and zipper; independently drawn native sizes',
        'generator':'scripts/build_stuffit_icon.py','family':FAMILY,
        'variants':{path.name:hashlib.sha256(path.read_bytes()).hexdigest() for path in sorted(DEST.glob('*.bin'))}},indent=2)+'\n')


if __name__ == '__main__':
    build()
    iconconvert.export_family(str(ROOT),'drivers/stuffit-filesystem',FAMILY,str(DEST))
