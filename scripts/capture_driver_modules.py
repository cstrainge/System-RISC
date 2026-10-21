#!/usr/bin/env python3
"""Private native-module transport. Rust creates malformed packages and owns
acceptance; this script compiles fixtures, imports requested files, and boots.
"""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import subprocess

from hfs_b_tool import Image
import system7_vm as vm


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def prepare(output):
    output.mkdir(parents=True, exist_ok=True)
    target = vm.build_dir() / 'fixtures/modules'
    for fixture, binary in [('driver-provider', 'ModuleEcho'), ('driver-client', 'ModuleClient')]:
        manifest = vm.ROOT / 'System7/crates/mac-toolbox/tests/fixtures' / fixture / 'Cargo.toml'
        subprocess.run(['cargo', 'build', '--locked', '--manifest-path', str(manifest),
                        '--release', '--target', 'riscv64imac-unknown-none-elf', '--bin', binary,
                        '--target-dir', str(target)], cwd=vm.ROOT, check=True)
        subprocess.run(['cargo', 'run', '--locked', '--manifest-path', str(manifest),
                        '--bin', 'package', '--target-dir', str(target), '--',
                        str(target / 'riscv64imac-unknown-none-elf/release' / binary),
                        str(output / 'packages')], cwd=vm.ROOT, check=True)


def capture(output, cases):
    original = vm.DIST
    before = {name: digest(original / name) for name in ['system7.qcow2', 'bootrom.bin', 'System.rsrc']}
    observations = []
    for case in cases:
        name = case['name']
        if not name or not all(c.isascii() and (c.isalnum() or c == '-') for c in name):
            raise ValueError('invalid case directory')
        state = output / name
        dist = state / 'dist'
        dist.mkdir(parents=True, exist_ok=False)
        raw = dist / 'module-test.raw'
        process = None
        try:
            subprocess.run(['qemu-img', 'convert', '-f', 'qcow2', '-O', 'raw', '-S', '4k',
                            str(original / 'system7.qcow2'), str(raw)], check=True)
            image = Image(raw)
            for file in case['files']:
                image.import_macbinary(Path(file['source']), file['directory'])
            shutil.copyfile(original / 'bootrom.bin', dist / 'bootrom.bin')
            subprocess.run(['qemu-img', 'convert', '-f', 'raw', '-O', 'qcow2', str(raw),
                            str(dist / 'system7.qcow2')], check=True)
            vm.DIST = dist
            process, endpoint = vm.launch(state / 'vm')
            observations.append({'name': name, 'termination': 'desktop-ready',
                                 'serial': str(state / 'vm/serial.log')})
        finally:
            if process is not None:
                if process.poll() is None:
                    vm.qmp('quit', endpoint=endpoint)
                process.wait(timeout=10)
            # Failed launches also clean their private media; the base and live
            # user desktop are never altered by this transport.
            for folder in [dist, state / 'vm']:
                for pattern in ['*.raw', '*.qcow2', '*.bin']:
                    for path in folder.glob(pattern):
                        path.unlink()
            vm.DIST = original
    report = {'cases': observations, 'original_before': before,
              'original_after': {name: digest(original / name) for name in before}}
    (output / 'capture.json').write_text(json.dumps(report, indent=2) + '\n')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', required=True, type=Path)
    parser.add_argument('--prepare', action='store_true')
    parser.add_argument('--cases', type=Path)
    args = parser.parse_args()
    if args.prepare:
        prepare(args.output.resolve())
    if args.cases:
        capture(args.output.resolve(), json.loads(args.cases.read_text()))
