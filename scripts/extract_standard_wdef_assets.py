#!/usr/bin/env python3
"""Extract reference WDEF colors/widgets/fonts without changing original images.

This reads the installed System 7.6.1 MacBinary snapshot and the pinned Quadra
ROM. It creates analysis assets in working/wdef-reference; --install-fonts also
copies Chicago 12 and Geneva 9 into resident System's QuickDraw assets directory.
"""
import argparse
import hashlib
import json
from pathlib import Path
import struct

ROOT = Path(__file__).resolve().parents[2]


def resources(blob):
    data_start, map_start, data_size, map_size = struct.unpack_from('>4I', blob)
    assert data_start + data_size <= len(blob)
    assert map_start + map_size <= len(blob)
    types_offset = struct.unpack_from('>H', blob, map_start + 24)[0]
    types_start = map_start + types_offset
    count = struct.unpack_from('>H', blob, types_start)[0] + 1
    for index in range(count):
        kind, last, refs = struct.unpack_from('>4sHH', blob, types_start + 2 + index * 8)
        for entry in range(last + 1):
            reference = types_start + refs + entry * 12
            resource_id = struct.unpack_from('>h', blob, reference)[0]
            attributes = blob[reference + 4]
            start = data_start + int.from_bytes(blob[reference + 5:reference + 8], 'big')
            length = struct.unpack_from('>I', blob, start)[0]
            assert start + 4 + length <= data_start + data_size
            yield kind, resource_id, attributes, start + 4, blob[start + 4:start + 4 + length]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--install-fonts', action='store_true')
    args = parser.parse_args()
    source = ROOT / 'working/vm/installed-System.bin'
    binary = source.read_bytes()
    assert hashlib.sha256(binary).hexdigest() == '1150d6a3f74df612cc41e7d0dc045242b6e8d92d9a899eb39586e662e7b49dea'
    data_size, resource_size = struct.unpack_from('>II', binary, 83)
    resource_start = 128 + (data_size + 127) // 128 * 128
    resource_blob = binary[resource_start:resource_start + resource_size]
    out = ROOT / 'working/wdef-reference'
    out.mkdir(exist_ok=True)
    manifest = []
    geneva = None
    for kind, resource_id, attrs, start, data in resources(resource_blob):
        wanted = kind in (b'WDEF', b'wctb', b'pmap') or (kind == b'pixs' and resource_id in (-14335, -14334))
        if kind == b'FONT' and resource_id == 393:
            geneva = data
            wanted = True
        if not wanted:
            continue
        assert not attrs & 1, 'Compressed resources require decompression first'
        name = f'{kind.decode("mac_roman")}-{resource_id}.bin'
        (out / name).write_bytes(data)
        manifest.append({'type': kind.decode('mac_roman'), 'id': resource_id,
                         'source_file_offset': resource_start + start,
                         'path': str((out / name).relative_to(ROOT)), 'length': len(data),
                         'sha256': hashlib.sha256(data).hexdigest()})
    rom_path = ROOT / 'downloads/roms/Quadra-650.ROM'
    rom = rom_path.read_bytes()
    assert hashlib.sha256(rom).hexdigest() == '05ad753fb594e656cf078023ec189e09e2a7655a780de993b75b8c51ed6b09ca'
    assert rom[0x7DBA0:0x7DBA6] == b'FONT\x00\x0c'
    font_length = struct.unpack_from('>I', rom, 0x7DBB8)[0]
    assert font_length == 0xE2A
    chicago = rom[0x7DBC0:0x7DBC0 + font_length]
    (out / 'Chicago12.FONT').write_bytes(chicago)
    manifest.append({'type': 'FONT', 'id': 12, 'source': str(rom_path.relative_to(ROOT)),
                     'source_file_offset': 0x7DBC0, 'length': len(chicago),
                     'path': str((out / 'Chicago12.FONT').relative_to(ROOT)),
                     'sha256': hashlib.sha256(chicago).hexdigest()})
    if args.install_fonts:
        assets = ROOT / 'System7/system/src/toolbox/quickdraw/assets'
        assets.mkdir(exist_ok=True)
        (assets / 'chicago-12.font').write_bytes(chicago)
        assert geneva is not None
        (assets / 'geneva-9.font').write_bytes(geneva)
    (out / 'asset-extraction.json').write_text(json.dumps(manifest, indent=2) + '\n')
    print(f'Extracted {len(manifest)} resources; original System and ROM unchanged.')


if __name__ == '__main__':
    main()
