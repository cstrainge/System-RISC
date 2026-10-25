#!/usr/bin/env python3
"""Transport public SDK event wait/latency assertions on an owned RV64 guest.

Only the guest's monotonic clock is used for dispatch samples. Host QMP arrival,
Windows/WSL transport and host display presentation are outside those intervals.
Python supplies input and observations; Rust owns correctness acceptance.
"""
from system7_platform import tablet_axis
from hfs_b_tool import Image as NativeImage
import argparse
import json
import os
import re
from collections import defaultdict, deque
from pathlib import Path
import subprocess
import time
import capture_system7_conformance as packaging
from capture_system7_processes import child
import system7_vm as vm


def capture(output, windows=False):
    output = Path(output).resolve()
    output.mkdir(parents=True, exist_ok=False)
    original = vm.DIST
    before = {name: packaging.sha(original / name)
              for name in ['system7.qcow2', 'bootrom.bin', 'System.rsrc']}
    private, binary = packaging.private_disk(output, 'event-latency')
    spinner, spinner_elf = child(output, 'process-spinner', 'ProcessSpinner')

    raw = private / 'conformance.raw'
    fs = NativeImage(raw)
    fs.import_macbinary(spinner, '/Applications')
    subprocess.run(['qemu-img', 'convert', '-f', 'raw', '-O', 'qcow2', str(raw),
                    str(private / 'system7.qcow2')], check=True)
    vm.DIST = private
    process, endpoint = vm.launch(output / 'vm',
                                 'native-headless' if windows else 'headless',
                                 qmp_port=4457)
    serial = output / 'vm/serial.log'
    actions = []

    def text():
        return serial.read_text(errors='replace')

    def wait(marker):
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            current = text()
            if marker in current or 'EVENT_LATENCY_FAIL ' in current:
                return current
            if process.poll() is not None:
                raise RuntimeError('Owned VM exited:\n' + current[-8000:])
            time.sleep(.005)
        raise TimeoutError('No acknowledgement for ' + marker + ':\n' + text()[-8000:])

    def send(events, after):
        vm.qmp('input-send-event', {'events': events}, endpoint)
        actions.append({'after': after, 'events': events})

    try:
        current = wait('EVENT_LATENCY_READY_POINTER\n')
        if 'EVENT_LATENCY_FAIL ' not in current:
            # Transport handshake lets the guest register its wait before motion;
            # this host pause is not included in any guest latency measurement.
            time.sleep(.05)
            send([{'type': 'abs', 'data': {'axis': 'x', 'value': tablet_axis(241, 'x')}},
                  {'type': 'abs', 'data': {'axis': 'y', 'value': tablet_axis(180, 'y')}}],
                 'EVENT_LATENCY_READY_POINTER')
        for scenario in ['IDLE', 'MASKED_KEY', 'BUSY']:
            marker = ('EVENT_LATENCY_READY_MASKED_KEY' if scenario == 'MASKED_KEY'
                      else 'EVENT_LATENCY_READY_KEYS_' + scenario) + '\n'
            current = wait(marker)
            if 'EVENT_LATENCY_FAIL ' in current:
                break
            time.sleep(.05)
            send([{'type': 'key', 'data': {'key': {'type': 'qcode', 'data': key}, 'down': down}}
                  for key in ('z' if scenario == 'MASKED_KEY' else 'abcd')
                  for down in [True, False]], marker.strip())
        wait('EVENT_LATENCY_USERSPACE_PASS\n')
        current = text()
        physical_timing = []
        trace = re.search(r'EVENT_TRACE address=(\d+) bytes=(\d+) version=2', current)
        if trace:
            from capture_system7_event_trace import decode
            address, length = map(int, trace.groups())
            path = output / 'event-timing.bin'
            vm.qmp('stop', endpoint=endpoint)
            try:
                vm.qmp('pmemsave', {'val': address, 'size': length, 'filename': str(path)}, endpoint)
            finally:
                vm.qmp('cont', endpoint=endpoint)
            snapshot = decode(path)
            pending = defaultdict(deque)
            for row in snapshot['records']:
                if row[2] == 1 and row[4] == 3 and row[5] & 255 in b'abcd':
                    kind = 4 if row[6] == 0 else 3
                    pending[(row[3], kind, row[5])].append(row[1])
                elif row[2] == 2 and row[4] in (3, 4) and row[5] & 255 in b'abcd':
                    ingress = pending[(row[3], row[4], row[5])]
                    if ingress:
                        physical_timing.append((row[1] - ingress.popleft()) * 100)
            (output / 'event-timing.json').write_text(json.dumps(snapshot, indent=2) + '\n')
        report = {
            'transport': 'private single-hart RV64 disk -> public SDK ECALL -> physical virtio input',
            'platform': 'windows-native' if windows else 'linux',
            'assertion_owner': 'mac-toolbox/tests/event_latency_cases.rs',
            'timing_scope': 'guest monotonic post-to-wait-return; excludes host QMP and presentation',
            'original_before': before,
            'original_after': {name: packaging.sha(original / name) for name in before},
            'fixture_elf_sha256': packaging.sha(binary),
            'spinner_elf_sha256': packaging.sha(spinner_elf),
            'launch': json.loads((output / 'vm/command.json').read_text()),
            'serial_sha256': packaging.sha(serial),
            'distributions': [json.loads(line.split(' ', 1)[1]) for line in current.splitlines()
                              if line.startswith('EVENT_LATENCY_DISTRIBUTION ')],
            'blocked_samples': [json.loads(line.split(' ', 1)[1]) for line in current.splitlines()
                                if line.startswith('EVENT_LATENCY_BLOCKED_SAMPLE ')],
            'input': actions,
            'physical_timing_scope': 'decoded virtio input to app event delivery; excludes time before driver observation and masked z deliberately held for timeout',
            'physical_timing_available': bool(trace),
            'physical_delivery_ns': physical_timing,
        }
    finally:
        try:
            if process.poll() is None:
                vm.qmp('quit', endpoint=endpoint)
        finally:
            process.wait(timeout=10)
            vm.DIST = original
    report['owned_vm_exited'] = process.returncode == 0
    (output / 'capture.json').write_text(json.dumps(report, indent=2) + '\n')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', required=True)
    parser.add_argument('--windows-headless', action='store_true')
    args = parser.parse_args()
    capture(args.output, args.windows_headless)
