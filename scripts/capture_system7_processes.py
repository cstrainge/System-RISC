#!/usr/bin/env python3
"""Package public-SDK process fixtures on an owned disk and transport real keys.

Canonical contract assertions execute in Rust userspace on RV64. Python supplies
only build/package/clone/boot/input/frame/serial transport and artifact identities.
"""
from hfs_b_tool import Image as NativeImage
from system7_platform import guest_env
import argparse
import json
import os
from pathlib import Path
import subprocess
import time
import capture_system7_conformance as packaging
import system7_vm as vm


def child(output, fixture, binary):
    manifest = vm.ROOT / 'System7/crates/mac-toolbox/tests/fixtures' / fixture / 'Cargo.toml'
    target = vm.build_dir() / 'fixtures' / fixture
    subprocess.run(['cargo', 'build', '--manifest-path', str(manifest), '--locked',
                    '--release', '--target', 'riscv64imac-unknown-none-elf',
                    '--bin', binary, '--target-dir', str(target)], cwd=vm.ROOT, env=guest_env(), check=True)
    elf = target / 'riscv64imac-unknown-none-elf/release' / binary
    package = output / fixture
    subprocess.run(['cargo', 'run', '--manifest-path', str(manifest), '--locked',
                    '--bin', 'package', '--target-dir', str(target), '--', str(elf),
                    str(package)], cwd=vm.ROOT, check=True)
    return package / (binary + '.bin'), elf


def capture(output, frontend='headless', vector_bits=None, malformed_standard_file=False):
    output = Path(output).resolve()
    output.mkdir(parents=True, exist_ok=False)
    original = vm.DIST
    before = {name: packaging.sha(original / name)
              for name in ['system7.qcow2', 'bootrom.bin', 'System.rsrc']}
    extra = (original / 'System.bin',) if malformed_standard_file else ()
    private, observer = packaging.private_disk(output, 'process-observer', package_arguments=extra)
    children = [child(output, fixture, binary)
                for fixture, binary in [('process-worker', 'ProcessWorker'),
                                        ('process-spinner', 'ProcessSpinner')]]

    raw = private / 'conformance.raw'
    fs = NativeImage(raw)
    if malformed_standard_file:
        fs.remove('/System Folder/System')
        fs.import_macbinary(output / 'package' / 'Malformed System.bin', '/System Folder')
    fs.mkdir('/Applications', exist_ok=True)
    for package, _ in children:
        fs.import_macbinary(package, '/Applications')
    for name in ['Budget App', 'Corrupt App']:
        fs.import_macbinary(output / 'package' / (name + '.bin'), '/Applications')
    fs.mkdir('/Copies')
    fs.import_macbinary(children[0][0], '/Copies')
    subprocess.run(['qemu-img', 'convert', '-f', 'raw', '-O', 'qcow2', str(raw),
                    str(private / 'system7.qcow2')], check=True)
    vm.DIST = private
    process, endpoint = vm.launch(output / 'vm', frontend, writable=True, qmp_port=4457, vector_bits=vector_bits)
    serial = output / 'vm/serial.log'
    actions = []

    def wait(marker):
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            text = serial.read_text(errors='replace')
            if 'PROCESS_FAIL' in text or process.poll() is not None:
                raise RuntimeError('Process fixture stopped:\n' + text[-6000:])
            if marker in text:
                return text
            time.sleep(.01)
        raise TimeoutError('Process transport did not acknowledge ' + marker + ':\n'
                           + serial.read_text(errors='replace')[-6000:])

    try:
        for marker in ['PROCESS_READY_FOR_INPUT_1', 'PROCESS_READY_FOR_INPUT_2',
                       'PROCESS_READY_FOR_COPY_INPUT_1', 'PROCESS_READY_FOR_COPY_INPUT_2']:
            wait(marker + '\n')
            # A child is foreground. Input travels through the real virtio device.
            events = [{'type': 'key', 'data': {'key': {'type': 'qcode', 'data': 'x'},
                                             'down': down}} for down in [True, False]]
            vm.qmp('input-send-event', {'events': events}, endpoint)
            actions.append({'after': marker,
                            'input': events})
        text = wait('hide=passed appmenu=passed\n')
        vm.qmp('screendump', {'filename': str(output / 'processes.ppm')}, endpoint)
        report = {'transport': 'private blessed disk -> System -> independent RV64 SDK apps -> ECALL',
                  'assertion_owner': 'mac-toolbox/tests/process_cases.rs',
                  'malformed_standard_file': malformed_standard_file,
                  'termination': 'guest-outcome',
                  'original_before': before,
                  'original_after': {name: packaging.sha(original / name) for name in before},
                  'observer_elf_sha256': packaging.sha(observer),
                  'child_elf_sha256': [packaging.sha(elf) for _, elf in children],
                  'launch': json.loads((output / 'vm/command.json').read_text()),
                  'serial': 'vm/serial.log', 'serial_sha256': packaging.sha(serial),
                  'input': actions}
    finally:
        try:
            if process.poll() is None:
                vm.qmp('quit', endpoint=endpoint)
        finally:
            process.wait(timeout=10)
            vm.DIST = original
            for media in [output / 'vm/system7.qcow2', output / 'vm/bootrom.bin',
                          private / 'system7.qcow2', private / 'bootrom.bin', raw]:
                media.unlink(missing_ok=True)
    report['owned_vm_exited'] = process.returncode == 0
    (output / 'capture.json').write_text(json.dumps(report, indent=2) + '\n')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', required=True)
    parser.add_argument('--frontend', choices=['headless', 'native-headless'], default='headless')
    parser.add_argument('--vector-bits', type=int)
    parser.add_argument('--malformed-standard-file', action='store_true')
    args = parser.parse_args()
    capture(args.output, args.frontend, args.vector_bits, args.malformed_standard_file)
