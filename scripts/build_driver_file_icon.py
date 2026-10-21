#!/usr/bin/env python3
"""Compose the native driver family from the original puzzle and supplied chip.

This edits classic resource pixels directly: preserve the original puzzle's
bitmap/mask outside the badge, and use the user's separate color/mono chip
pixels at each native size. No interpolation, luminance conversion or redrawing.
"""
from pathlib import Path
import hashlib
import json
import iconconvert

ROOT = Path(__file__).resolve().parents[1]
DEST = ROOT / 'system/assets/icons/driver-file'
IDENTITY = -30001
BASE = -16415
CHIP = -30000


def covered(size, x, y):
    if size == 32:
        return ((11 <= x <= 19 and 18 <= y <= 26)
                or (x in (12, 15, 18) and y in (16, 17, 27, 28))
                or (x in (9, 10, 20, 21) and y in (19, 22, 25)))
    return ((5 <= x <= 9 and 8 <= y <= 12)
            or (x in (6, 8) and y in (7, 13))
            or (x in (4, 10) and y in (9, 11)))


def bit(data, size, x, y):
    return (data[y * (size // 8) + x // 8] >> (7 - x % 8)) & 1


def set_bit(data, size, x, y, value):
    at, mask = y * (size // 8) + x // 8, 1 << (7 - x % 8)
    data[at] = (data[at] & ~mask) | (mask if value else 0)


def main():
    paths = iconconvert.resource_paths(str(ROOT), 'system')
    DEST.mkdir(parents=True, exist_ok=True)
    sources = []
    for size, mono, color in [(32, 'ICN#', 'icl8'), (16, 'ics#', 'ics8')]:
        base_bits = Path(paths[(mono, str(BASE))]).read_bytes()
        chip_bits = Path(paths[(mono, str(CHIP))]).read_bytes()
        base_color = Path(paths[(color, str(BASE))]).read_bytes()
        chip_color = Path(paths[(color, str(CHIP))]).read_bytes()
        result_bits, result_color = bytearray(base_bits), bytearray(base_color)
        bitmap_size = size * size // 8
        shift = -6 if size == 32 else -3
        for y in range(size):
            for x in range(size):
                if not covered(size, x, y):
                    continue
                to_y = y + shift
                assert bit(base_bits[bitmap_size:], size, x, to_y), 'badge escapes puzzle'
                set_bit(result_bits, size, x, to_y, bit(chip_bits, size, x, y))
                result_color[to_y * size + x] = chip_color[y * size + x]
        assert result_bits[bitmap_size:] == base_bits[bitmap_size:]
        for typ, data in [(mono, result_bits), (color, result_color)]:
            (DEST / f'{iconconvert.TYPEHEX[typ]}-{IDENTITY}.bin').write_bytes(data)
        for typ, identity in [(mono, BASE), (color, BASE), (mono, CHIP), (color, CHIP)]:
            path = Path(paths[(typ, str(identity))])
            sources.append({'path': str(path.relative_to(ROOT)),
                            'sha256': hashlib.sha256(path.read_bytes()).hexdigest()})
    (DEST / 'sources.json').write_text(json.dumps({'family': IDENTITY,
        'puzzle_family': BASE, 'chip_family': CHIP,
        'placement': {'32': [0, -6], '16': [0, -3]}, 'sources': sources}, indent=2) + '\n')
    # Preview export uses the same resource decoder as the ordinary icon tool.
    iconconvert.export_family(str(ROOT), 'system', IDENTITY, str(DEST))


if __name__ == '__main__':
    main()
