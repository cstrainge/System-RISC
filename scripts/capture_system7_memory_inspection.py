#!/usr/bin/env python3
"""Private native public-SDK mapping probe; Rust owns acceptance assertions."""
import argparse
import json
from pathlib import Path
import time
import capture_system7_conformance as packaging
import system7_vm as vm


def capture(output):
    output.mkdir(parents=True, exist_ok=False)
    original = vm.DIST
    before = {name: packaging.sha(original / name) for name in ['system7.qcow2', 'System.rsrc', 'bootrom.bin']}
    private, binary = packaging.private_disk(output, features=('memory-inspection',),
        package_arguments=(original / 'System.bin',), replace_system=True)
    process = endpoint = None
    try:
        vm.DIST = private
        process, endpoint = vm.launch(output / 'vm')
        serial = output / 'vm/serial.log'
        deadline = time.monotonic() + 45
        while time.monotonic() < deadline:
            text = serial.read_text(errors='replace')
            if 'TOOLBOX_USERSPACE_PASS ' in text or 'TOOLBOX_USERSPACE_FAIL ' in text:
                break
            if process.poll() is not None:
                break
            time.sleep(.02)
        report = {'original_before': before,
            'original_after': {name: packaging.sha(original / name) for name in before},
            'fixture_elf_sha256': packaging.sha(binary),
            'launch': json.loads((output / 'vm/command.json').read_text())}
    finally:
        vm.DIST = original
        if process is not None and process.poll() is None:
            vm.qmp('quit', endpoint=endpoint)
            process.wait(timeout=10)
    report['owned_vm_exited'] = process.poll() is not None
    (output / 'capture.json').write_text(json.dumps(report, indent=2) + '\n')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    capture(parser.parse_args().output.resolve())
