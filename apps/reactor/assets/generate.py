#!/usr/bin/env python3
"""Original Reactor atom/gauge icon, explicitly drawn at both classic sizes."""
import math
import struct
from pathlib import Path

ROOT = Path(__file__).resolve().parent
PALETTE = (ROOT.parents[2] / "system/assets/visual/636c7574-8.bin").read_bytes()
COLORS = [tuple(v >> 8 for v in struct.unpack_from(">HHH", PALETTE, 10 + i * 8)) for i in range(256)]

def nearest(rgb):
    return min(range(256), key=lambda i: sum((a-b)**2 for a,b in zip(rgb, COLORS[i])))

for size in (16, 32):
    bitmap = bytearray(size * size // 8)
    mask = bytearray(len(bitmap))
    color = bytearray(size * size)
    center = (size-1)/2
    for y in range(size):
        for x in range(size):
            px, py = (x-center)/size, (y-center)/size
            radius = math.hypot(px,py)
            orbit = False
            for angle in (0, math.pi/3, 2*math.pi/3):
                u = px*math.cos(angle)+py*math.sin(angle)
                v = -px*math.sin(angle)+py*math.cos(angle)
                distance = math.sqrt((u/.43)**2 + (v/.17)**2)
                orbit |= abs(distance-1) < (0.13 if size == 16 else 0.09)
            nucleus = radius < .09
            if orbit or nucleus:
                bit = y*size+x
                bitmap[bit//8] |= 1 << (7-bit%8)
                mask[bit//8] |= 1 << (7-bit%8)
                color[bit] = nearest((0,0,0) if nucleus else (0,51,153))
    (ROOT/f"icon{size}-mono.bin").write_bytes(bitmap+mask)
    (ROOT/f"icon{size}-color.bin").write_bytes(color)
