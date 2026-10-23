#!/usr/bin/env python3
"""Project-owned classic-SIT stored/RLE outer wrappers for a real nested SIT.

This independent byte writer uses the classic 22-byte/112-byte header layout
and IBM CRC16. It does not import or execute the production parser/decoder.
It wraps nested-v1.sit (see README for that independent writer's provenance).
"""
from pathlib import Path
import struct


def crc16(data):
    value = 0
    for byte in data:
        value ^= byte
        for _ in range(8):
            value = (value >> 1) ^ (0xa001 if value & 1 else 0)
    return value


def wrap(data, method):
    # Literal RLE90 encoding is intentional: forcing escapes exercises the
    # compressed backing-file path without sharing a decoder implementation.
    packed = data.replace(b'\x90', b'\x90\x00') if method else data
    entry = bytearray(112)
    entry[1] = method
    name = b'Inner.sit'
    entry[2] = len(name)
    entry[3:3 + len(name)] = name
    entry[66:74] = b'SIT!SIT!'
    struct.pack_into('>II', entry, 76, 3875171696, 3875171700)
    struct.pack_into('>I', entry, 88, len(data))
    struct.pack_into('>I', entry, 96, len(packed))
    struct.pack_into('>H', entry, 102, crc16(data))
    struct.pack_into('>H', entry, 110, crc16(entry[:110]))
    header = bytearray(22)
    header[:4] = b'SIT!'
    struct.pack_into('>HI', header, 4, 1, 22 + len(entry) + len(packed))
    header[10:14] = b'rLau'
    header[14] = 1
    return header + entry + packed


if __name__ == '__main__':
    root = Path(__file__).resolve().parent
    payload = (root / 'nested-v1.sit').read_bytes()
    for method, name in [(0, 'archive-in-archive-stored.sit'),
                         (1, 'archive-in-archive-rle.sit')]:
        (root / name).write_bytes(wrap(payload, method))
