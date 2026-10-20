#!/usr/bin/env python3
"""Transport for the aggregate Rust durable File Manager application.

Build/package a private Finder, replace it only on a cloned disk, boot the real
ROM/System graphically (headless display), and retain serial/outcome evidence.
Rust owns every compatibility assertion and the acceptance marker.
"""
from hfs_b_tool import Image as NativeImage
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import time
import system7_vm as vm


def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def private_disk(output):
    fixture = vm.ROOT / 'System7/crates/mac-toolbox/tests/fixtures/file-writes'
    target = vm.ROOT / 'build/system7/file-writes-probe-target'
    manifest = fixture / 'Cargo.toml'
    subprocess.run(['cargo', 'build', '--manifest-path', str(manifest), '--locked',
                    '--release', '--target', 'riscv64imac-unknown-none-elf',
                    '--bin', 'Finder', '--target-dir', str(target)], cwd=vm.ROOT, check=True)
    binary = target / 'riscv64imac-unknown-none-elf/release/Finder'
    packaged = output / 'package'
    subprocess.run(['cargo', 'run', '--manifest-path', str(manifest), '--locked',
                    '--bin', 'package', '--target-dir', str(target), '--', str(binary),
                    str(packaged)], cwd=vm.ROOT, check=True)
    private = output / 'dist'
    private.mkdir()
    raw = private / 'file-writes.raw'
    subprocess.run(['qemu-img', 'convert', '-f', 'qcow2', '-O', 'raw', '-S', '4k',
                    str(vm.DIST / 'system7.qcow2'), str(raw)], check=True)

    fs = NativeImage(raw)
    fs.remove('/System Folder/Finder')
    fs.import_macbinary(packaged / 'Finder.bin', '/System Folder')
    # Generated MacBinary test file, no original payload is modified.
    import struct
    header = bytearray(128)
    name = b'Write Sample'
    header[1] = len(name)
    header[2:2+len(name)] = name
    header[65:69] = b'TEXT'
    header[69:73] = b'TEST'
    struct.pack_into('>II', header, 83, 1700, 900)
    struct.pack_into('>II', header, 91, 3875171696, 3875171696)
    import binascii
    header[122:124] = bytes([129, 129])
    struct.pack_into('>H', header, 124, binascii.crc_hqx(header[:124], 0))
    data = bytes((i*17)&255 for i in range(1700))
    resource = bytes((i*31)&255 for i in range(900))
    sample = output / 'write-sample.bin'
    sample.write_bytes(header + data + bytes((-len(data))%128) + resource + bytes((-len(resource))%128))
    fs.import_macbinary(sample, '/')
    shutil.copyfile(vm.DIST / 'bootrom.bin', private / 'bootrom.bin')
    subprocess.run(['qemu-img', 'convert', '-f', 'raw', '-O', 'qcow2', str(raw),
                    str(private / 'system7.qcow2')], check=True)
    return private, binary


def capture(output):
    output = Path(output).resolve()
    output.mkdir(parents=True, exist_ok=False)
    original = vm.DIST
    before = {name: sha(original / name) for name in ['system7.qcow2', 'bootrom.bin', 'System.rsrc']}
    private, binary = private_disk(output)
    vm.DIST = private
    observations = []
    for boot in range(1, 4):
        state = output / 'vm'
        process, endpoint = vm.launch(state, writable=True, preserve=True)
        serial = state / 'serial.log'
        termination = 'timeout'
        try:
            deadline = time.monotonic() + 60
            while time.monotonic() < deadline:
                text = serial.read_text(errors='replace')
                if 'FILE_WRITE_STAGE ' in text or 'FILE_WRITE_FAIL ' in text:
                    termination = 'guest-outcome'
                    break
                if process.poll() is not None:
                    termination = 'process-exited'
                    break
                time.sleep(.02)
            saved = output / f'boot-{boot}.log'
            saved.write_bytes(serial.read_bytes())
            observations.append({'boot': boot, 'termination': termination,
                                 'serial': saved.name, 'serial_sha256': sha(saved),
                                 'launch': json.loads((state / 'command.json').read_text())})
        finally:
            if process.poll() is None:
                vm.qmp('quit', endpoint=endpoint)
            process.wait(timeout=10)
    raw = output / 'persisted.hfs'
    subprocess.run(['qemu-img', 'convert', '-f', 'qcow2', '-O', 'raw', '-S', '4k',
                    str(output / 'vm/system7.qcow2'), str(raw)], check=True)
    report = {'transport': 'three real disk boots -> public SDK -> ECALL -> virtio OUT/FLUSH',
              'assertion_owner': 'mac-toolbox/tests/file_write_cases.rs',
              'boots': observations, 'original_before': before,
              'original_after': {name: sha(original / name) for name in before},
              'fixture_elf_sha256': sha(binary), 'persisted_hfs': str(raw),
              'serial': 'boot-3.log'}
    (output / 'capture.json').write_text(json.dumps(report, indent=2) + '\n')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', required=True)
    capture(parser.parse_args().output)
