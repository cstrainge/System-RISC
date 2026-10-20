#!/usr/bin/env python3
"""Remove obsolete generated System RISC disks/ROM copies; retain reports and references."""
import json
import os
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


def prune():
    results = ROOT / 'results/system7'
    results.mkdir(parents=True, exist_ok=True)
    build = ROOT / 'build/system7'
    dist = build / 'dist'
    protected = []
    # Keep an entire capture while any of its QEMU processes is still alive.
    # A reused live PID can conservatively retain a little extra media.
    for base in (results, build):
        for pid in base.rglob('pid'):
            try:
                os.kill(int(pid.read_text().strip()), 0)
            except (OSError, ValueError):
                continue
            relative = pid.relative_to(base)
            protected.append(base / relative.parts[0])
    candidates = []
    for base in (results, build):
        for path in base.rglob('*'):
            if path.is_symlink() or not path.is_file() or path.is_relative_to(dist):
                continue
            if path.suffix not in ('.hfs', '.qcow2', '.raw', '.img', '.hda', '.dsk') and path.name != 'bootrom.bin':
                continue
            if any(path.is_relative_to(directory) for directory in protected):
                continue
            candidates.append(path)
    # The current desktop disk and original Macintosh media are outside this list.
    candidates.extend((ROOT / 'working/system7-vm').glob('system7-previous-*.qcow2'))
    removed = []
    freed = 0
    for path in candidates:
        stat = path.stat()
        removed.append(str(path.relative_to(ROOT)))
        freed += stat.st_blocks * 512
        path.unlink()
    report = {'removed_files': len(removed), 'allocated_bytes_freed': freed,
              'protected_live_captures': [str(p.relative_to(ROOT)) for p in protected],
              'files': removed}
    (results / 'image-cleanup.json').write_text(json.dumps(report, indent=2) + '\n')
    print(f'Removed {len(removed)} obsolete generated images; freed {freed / 2**30:.2f} GiB.')


if __name__ == '__main__':
    prune()
