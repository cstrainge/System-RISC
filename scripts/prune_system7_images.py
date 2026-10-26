#!/usr/bin/env python3
"""Remove obsolete generated System RISC disks/ROM copies; retain reports and references."""
import json
import os
from pathlib import Path
from system7_host import process_exists

ROOT = Path(__file__).resolve().parents[2]


def prune():
    results = ROOT / 'results/system7'
    results.mkdir(parents=True, exist_ok=True)
    build = ROOT / 'build/system7'
    dist = build / 'dist'
    # Each xtask build profile has one current packaged installation. Preserve
    # that installation, including the separate check profile and user-chosen
    # profiles, rather than mistaking another profile's base disk for a fixture.
    current = {dist}
    for inventory in build.rglob('build.json'):
        if inventory.parent.name == 'dist':
            current.add(inventory.parent)
    protected = []
    # These contain user files, including lossless bundles whose original names
    # can themselves end in .raw/.img. They are not generated media.
    user_state = [p for p in build.rglob('user-state') if p.is_dir()]
    persistent = [p.parent for base in (build, results)
                  for p in base.rglob('persistent-user-state.json')]
    # Keep an entire capture while any of its VM or fixture owners is still alive.
    # A reused live PID can conservatively retain a little extra media.
    for base in (results, build):
        for pid in base.rglob('pid'):
            try:
                if not process_exists(int(pid.read_text().strip())):
                    continue
            except (OSError, ValueError):
                continue
            relative = pid.relative_to(base)
            protected.append(base if len(relative.parts) == 1 else base / relative.parts[0])
    candidates = []
    for base in (results, build):
        for path in base.rglob('*'):
            if path.suffix not in ('.hfs', '.hfsb', '.hfs-b', '.qcow2', '.raw', '.img', '.hda', '.dsk') and path.name != 'bootrom.bin':
                continue
            if path.parent in current and path.name in ('system7.hfsb', 'system7.qcow2', 'bootrom.bin'):
                continue
            if path.is_symlink() or not path.is_file():
                continue
            if any(path.is_relative_to(directory) for directory in protected + user_state + persistent):
                continue
            candidates.append(path)
    # The current desktop disk and original Macintosh media are outside this list.
    candidates.extend((ROOT / 'working/system7-vm').glob('system7-previous-*.qcow2'))
    removed = []
    freed = 0
    for path in candidates:
        try:
            stat = path.stat()
            path.unlink()
        except FileNotFoundError:
            # A fixture owner or another cleanup may finish between enumeration
            # and deletion. Already removed media needs no further work.
            continue
        removed.append(path.relative_to(ROOT).as_posix())
        freed += getattr(stat, 'st_blocks', (stat.st_size + 511) // 512) * 512
    report = {'removed_files': len(removed), 'allocated_bytes_freed': freed,
              'protected_live_captures': [p.relative_to(ROOT).as_posix() for p in protected],
              'protected_user_exports': [p.relative_to(ROOT).as_posix() for p in user_state],
              'protected_persistent_vms': [p.relative_to(ROOT).as_posix() for p in persistent],
              'files': removed}
    (results / 'image-cleanup.json').write_text(json.dumps(report, indent=2) + '\n')
    print(f'Removed {len(removed)} obsolete generated images; freed {freed / 2**30:.2f} GiB.')


if __name__ == '__main__':
    prune()
