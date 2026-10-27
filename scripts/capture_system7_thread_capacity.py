#!/usr/bin/env python3
"""Build an owned ten-app guest and deliver real keyboard input to each worker.

The SDK Rust fixture owns behavioral assertions; this module transports input,
serial and trace evidence without touching the ordinary user's VM or disk.
"""
import argparse
import json
from pathlib import Path
import subprocess
import capture_system7_conformance as packaging
from capture_system7_processes import child
from capture_system7_applications_ui import Driver
from hfs_b_tool import Image
import system7_vm as vm


def capture(output):
    output = Path(output).resolve()
    output.mkdir(parents=True, exist_ok=False)
    original = vm.DIST
    before = {name: packaging.sha(original / name)
              for name in ['system7.qcow2', 'bootrom.bin', 'System.rsrc']}
    private = output / 'dist'
    raw = private / 'conformance.raw'
    process = None
    try:
        private, binary = packaging.private_disk(output, 'thread-capacity')
        worker, worker_binary = child(output, 'thread-capacity-worker', 'CapacityWorker')
        fs = Image(raw)
        for index in range(9):
            fs.import_macbinary(worker.parent / f'Capacity {index}.bin', '/Applications')
        subprocess.run(['qemu-img', 'convert', '-f', 'raw', '-O', 'qcow2',
                        str(raw), str(private / 'system7.qcow2')], check=True)
        vm.DIST = private
        process, endpoint = vm.launch(output / 'vm', writable=True)
        driver = Driver(endpoint, output / 'vm/serial.log')
        actions = []
        for phase in range(2):
            for index in reversed(range(9)):
                marker = f'THREAD_CAPACITY_INPUT phase={phase} index={index}\n'
                driver.wait(marker)
                driver.key('x')
                actions.append(marker.strip())
        driver.wait('THREAD_CAPACITY_PASS ')
        report = {'original_before': before,
                  'original_after': {name: packaging.sha(original / name) for name in before},
                  'fixture_elf_sha256': packaging.sha(binary),
                  'worker_elf_sha256': packaging.sha(worker_binary),
                  'input': actions, 'termination': 'guest-outcome'}
    finally:
        vm.DIST = original
        if process is not None:
            try:
                if process.poll() is None:
                    vm.qmp('quit', endpoint=endpoint)
            finally:
                process.wait(timeout=10)
        for media in [output / 'vm/system7.qcow2', output / 'vm/bootrom.bin',
                      private / 'system7.qcow2', private / 'bootrom.bin', raw]:
            media.unlink(missing_ok=True)
    report['owned_vm_exited'] = process.poll() is not None
    report['launch'] = json.loads((output / 'vm/command.json').read_text())
    (output / 'capture.json').write_text(json.dumps(report, indent=2) + '\n')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', required=True)
    capture(parser.parse_args().output)
