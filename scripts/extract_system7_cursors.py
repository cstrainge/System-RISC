#!/usr/bin/env python3
"""Import or verify original cursor resources without modifying reference media.

The original broad visual importer lives outside this repository and omitted
acur. This scoped importer supplements its output, retaining the original IDs,
attributes and bytes of CURS, crsr and acur resources.
"""
import argparse
import hashlib
import json
from pathlib import Path
import struct

from extract_standard_wdef_assets import resources

ROOT = Path(__file__).resolve().parents[1]
MEDIA = ROOT.parent
KINDS = {b'CURS', b'crsr', b'acur'}
SOURCES = {
    'System': '1150d6a3f74df612cc41e7d0dc045242b6e8d92d9a899eb39586e662e7b49dea',
    'Finder': '647b19dba89dc0167ae45dda20fd02bd8e44c91252e7513c9c584f1588eb4d3f',
}
ROM_SHA = '05ad753fb594e656cf078023ec189e09e2a7655a780de993b75b8c51ed6b09ca'
ARROW_SHA = 'f7861225708284b299d2bbf5bc659472df5c8708b3789c3c5c2d341d78313e7d'


def sha(data):
    return hashlib.sha256(data).hexdigest()


def store(path, value, check):
    if check:
        assert path.read_bytes() == value, f'cursor asset differs: {path}'
    else:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(value)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--check', action='store_true', help='verify without writing')
    parser.add_argument('--package-dir', type=Path,
                        help='also verify cursors in built System/Finder resource forks')
    args = parser.parse_args()
    records = []
    for package, expected in SOURCES.items():
        source = MEDIA / 'working/icon-reference' / (package + '.bin')
        binary = source.read_bytes()
        assert sha(binary) == expected, f'original source changed: {source}'
        data_length, fork_length = struct.unpack_from('>II', binary, 83)
        fork_start = 128 + (data_length + 127) // 128 * 128
        fork = binary[fork_start:fork_start + fork_length]
        manifest_path = ROOT / package.lower() / 'resources.json'
        manifest = json.loads(manifest_path.read_text())
        entries = {(r['type'], r['id']): r for r in manifest['resources']}
        packed = None
        if args.package_dir:
            packed = {(k.decode('mac_roman'), i): (a, d)
                      for k, i, a, _, d in resources(
                          (args.package_dir / (package + '.rsrc')).read_bytes())}
        for kind, resource_id, attrs, offset, payload in resources(fork):
            if kind not in KINDS:
                continue
            assert not attrs & 1, 'compressed cursor requires original decompression'
            kind_text = kind.decode('mac_roman')
            filename = f'{kind.hex()}-{resource_id}.bin'
            asset_path = ROOT / package.lower() / 'assets/visual' / filename
            store(asset_path, payload, args.check)
            key = kind_text, resource_id
            if key in entries:
                entry = entries[key]
                assert entry.get('attributes', 0) == attrs
                assert entry['payload'] == {'format': 'file', 'path': 'assets/visual/' + filename}
            else:
                assert not args.check, f'missing manifest cursor: {package} {key}'
                manifest['resources'].append({
                    'type': kind_text,
                    'id': resource_id,
                    'symbol': f'ORIGINAL_VISUAL_{kind.hex().upper()}_'
                              + (f'N{-resource_id}' if resource_id < 0 else str(resource_id)),
                    'attributes': attrs,
                    'payload': {'format': 'file', 'path': 'assets/visual/' + filename},
                })
            if packed is not None:
                assert packed[key] == (attrs, payload), f'packaged cursor differs: {package} {key}'
            record = {
                'package': package, 'type': kind_text, 'id': resource_id,
                'attributes': attrs, 'asset': str(asset_path.relative_to(ROOT)),
                'length': len(payload), 'sha256': sha(payload),
                'source': str(source.relative_to(MEDIA)), 'source_sha256': expected,
                'source_resource_fork_offset': offset,
                'source_file_offset': fork_start + offset,
            }
            if kind == b'CURS':
                assert len(payload) == 68
                v, h = struct.unpack_from('>hh', payload, 64)
                record['hotspot'] = {'v': v, 'h': h}
            elif kind == b'acur':
                count, _ = struct.unpack_from('>HH', payload)
                assert len(payload) == 4 + count * 4
                record['frames'] = [struct.unpack_from('>h', payload, 4 + i * 4)[0]
                                    for i in range(count)]
            records.append(record)
        if not args.check:
            manifest_path.write_text(json.dumps(manifest, indent=2) + '\n')

    rom_path = MEDIA / 'downloads/roms/Quadra-650.ROM'
    rom = rom_path.read_bytes()
    assert sha(rom) == ROM_SHA, 'original ROM changed'
    arrow_offset = 0x3164c
    arrow = rom[arrow_offset:arrow_offset + 68]
    assert sha(arrow) == ARROW_SHA
    arrow_path = ROOT / 'system/assets/cursors/arrow.curs'
    store(arrow_path, arrow, args.check)
    provenance = {
        'format_version': 1,
        'source_path_base': 'Parent of the System7 repository; original media is read only.',
        'scope': 'Complete CURS/crsr/acur inventories of the pinned System and Finder forks, '
                 'plus the standard arrow record from the pinned ROM. No crsr is present '
                 'in either source resource map. No runtime behavior is inferred.',
        'arrow': {
            'asset': str(arrow_path.relative_to(ROOT)), 'length': 68, 'sha256': ARROW_SHA,
            'source': str(rom_path.relative_to(MEDIA)), 'source_sha256': ROM_SHA,
            'source_file_offset': arrow_offset, 'hotspot': {'v': 1, 'h': 1},
            'note': 'QuickDraw global arrow record; not an original CURS resource with ID 0.',
        },
        'resources': sorted(records, key=lambda r: (r['package'], r['type'], r['id'])),
    }
    store(ROOT / 'system/assets/cursors/provenance.json',
          (json.dumps(provenance, indent=2) + '\n').encode(), args.check)
    print(f'{"Verified" if args.check else "Imported"} {len(records)} original cursor '
          'resources and the ROM arrow; original media unchanged.')


if __name__ == '__main__':
    main()
