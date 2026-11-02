#!/usr/bin/env python3
"""Private file-source DRVR + public SDK app transport; Rust owns assertions."""
from system7_fixture_build import (guest_env as fixture_guest_env,
                                  host_env as fixture_host_env, build_application)
import argparse
import hashlib
import json
import re
from pathlib import Path
import shutil
import subprocess
import time

from hfs_b_tool import Image
import system7_vm as vm
from system7_capture import OwnedProcesses


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def capture(output):
    vm.RAM = '2G'
    output.mkdir(parents=True, exist_ok=False)
    for fixture, binary in [('file-source-provider', 'FileSourceProvider'), ('file-source-app', 'Finder')]:
        target = vm.build_dir() / 'fixtures' / fixture
        manifest = vm.SOURCE / 'crates/mac-toolbox/tests/fixtures' / fixture / 'Cargo.toml'
        if fixture == 'file-source-app':
            build_application(manifest, output / 'packages')
            continue
        subprocess.run(['cargo', 'build', '--locked', '--manifest-path', str(manifest),
                        '--release', '--target', 'riscv64imac-unknown-none-elf', '--bin', binary,
                        '--target-dir', str(target)], env=fixture_guest_env(target), check=True)
        subprocess.run(['cargo', 'run', '--locked', '--manifest-path', str(manifest),
                        '--bin', 'package', '--target-dir', str(target), '--',
                        str(target / 'riscv64imac-unknown-none-elf/release' / binary),
                        str(output / 'packages')], env=fixture_host_env(target), check=True)
    base = vm.DIST
    before = {name: digest(base / name) for name in ['system7.qcow2', 'bootrom.bin', 'System.rsrc']}
    dist = output / 'dist'
    dist.mkdir()
    raw = dist / 'file-source.raw'
    process = None
    owners = OwnedProcesses()
    try:
        subprocess.run(['qemu-img', 'convert', '-f', 'qcow2', '-O', 'raw', '-S', '4k',
                        str(base / 'system7.qcow2'), str(raw)], check=True)
        image = Image(raw)
        image.remove('/System Folder/Drivers/StuffIt Filesystem')
        image.remove('/System Folder/Finder')
        image.import_macbinary(output / 'packages/Finder.bin', '/System Folder')
        image.import_macbinary(output / 'packages/File Source Provider.bin', '/System Folder/Drivers')
        subprocess.run(['qemu-img', 'convert', '-f', 'raw', '-O', 'qcow2', str(raw),
                        str(dist / 'system7.qcow2')], check=True)
        shutil.copyfile(base / 'bootrom.bin', dist / 'bootrom.bin')
        vm.DIST = dist
        process, endpoint = owners.launch(output / 'vm', writable=True)
        argv = json.loads((output / 'vm/command.json').read_text())
        ram = vm.qmp('query-memory-size-summary', endpoint=endpoint)
        boot = (output / 'vm/serial.log').read_text(errors='replace')
        detected = list(map(int, re.findall(r'^BOOT_RAM base=0x[0-9a-fA-F]+ bytes=(\d+) ', boot, re.M)))
        if (argv.count('-m') != 1 or argv[argv.index('-m') + 1] != '2G'
                or ram['base-memory'] != 2147483648 or not detected
                or set(detected) != {2147483648}):
            raise RuntimeError('private fixture guest must detect exactly 2 GiB')
        termination = 'timeout'
        original_exit = None
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            text = (output / 'vm/serial.log').read_text(errors='replace')
            if 'FILE_SOURCE_FAIL' in text:
                termination = 'guest-failure'
                break
            owner = re.search(r'^BOOT_FINDER_DOMAIN app=(\d+)$', text, re.M)
            if 'FILE_SOURCE_PASS' in text and owner:
                original_exit = re.search(r'^PROCESS_EXIT app=' + owner[1] + r' .+$', text, re.M)
                if original_exit:
                    termination = 'guest-complete'
                    break
            if process.poll() is not None:
                termination = 'process-exited'
                break
            time.sleep(.02)
        (output / 'capture.json').write_text(json.dumps({
            'termination': termination,
            'original_process_exit': original_exit[0] if original_exit else None,
            'ram_evidence': {'argv': argv, 'guest_boot_bytes': detected, 'qmp': ram},
            'serial': str(output / 'vm/serial.log'),
            'original_before': before, 'original_after': {name: digest(base / name) for name in before}
        }, indent=2) + '\n')
    finally:
        vm.DIST = base
        owners.cleanup(output, [dist, output / 'vm'])


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', required=True, type=Path)
    capture(parser.parse_args().output.resolve())
