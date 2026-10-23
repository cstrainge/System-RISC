#!/usr/bin/env python3
"""Collect Text Edit phase timings and resident present traces; Rust asserts them."""
import argparse
import json
from pathlib import Path
import re
import time

import capture_system7_conformance as packaging
from capture_system7_event_trace import decode, delta
import system7_vm as vm


def capture(output):
    output = Path(output).resolve()
    output.mkdir(parents=True, exist_ok=False)
    original = vm.DIST
    hashes = {name: packaging.sha(original / name)
              for name in ('system7.qcow2', 'bootrom.bin', 'System.rsrc')}
    process = None
    stages = []
    try:
        private, binary = packaging.private_disk(output, 'typing-probe', features=('performance',))
        vm.DIST = private
        process, endpoint = vm.launch(output / 'vm')
        serial = output / 'vm/serial.log'

        def wait(marker):
            deadline = time.monotonic() + 60
            while time.monotonic() < deadline:
                log = serial.read_text(errors='replace')
                if marker in log:
                    return log
                if process.poll() is not None or 'TYPING_PROFILE_FAIL' in log:
                    raise RuntimeError(log[-4000:])
                time.sleep(.002)
            raise TimeoutError(marker + '\n' + log[-4000:])

        log = wait('TEXTEDIT_PERF_READY name=small-typing')
        match = re.search(r'EVENT_TRACE address=(\d+) bytes=(\d+) version=2', log)
        if not match:
            raise RuntimeError('Performance capture requires SYSTEM7_EVENT_TRACE=1 image')
        address, size = map(int, match.groups())

        def snapshot(name):
            path = output / (name + '.bin')
            vm.qmp('stop', endpoint=endpoint)
            try:
                vm.qmp('pmemsave', {'val': address, 'size': size, 'filename': str(path)}, endpoint)
            finally:
                vm.qmp('cont', endpoint=endpoint)
            return decode(path)

        for name in ('small-typing', 'large-typing', 'small-blink', 'large-blink', 'visible-cursor-blink', 'large-scrolling'):
            wait('TEXTEDIT_PERF_READY name=' + name)
            before = snapshot(name + '-before')
            events = [{'type': 'key', 'data': {'key': {'type': 'qcode', 'data': 'spc'}, 'down': down}}
                      for down in (True, False)]
            vm.qmp('input-send-event', {'events': events}, endpoint)
            log = wait('TEXTEDIT_PERF_READY name=phase-complete\n' if name == 'small-typing' else 'TEXTEDIT_PERF_DONE name=' + name)
            # UART writes may be observed between bytes; require a whole result.
            deadline = time.monotonic() + 60
            while not re.search(r'TEXTEDIT_PERF_DONE name=' + name + r' [^\n]*\n', log):
                if time.monotonic() >= deadline:
                    raise TimeoutError('incomplete phase result: ' + name)
                time.sleep(.002)
                log = serial.read_text(errors='replace')
            after = snapshot(name + '-after')
            line = next(line for line in log.splitlines() if line.startswith('TEXTEDIT_PERF_DONE name=' + name))
            fields = dict(re.findall(r'(\w+)=([^ ]+)', line))
            evidence = delta(before, after)
            evidence.update({key: int(value) if value.isdigit() else value for key, value in fields.items()})
            stages.append(evidence)
            print(line, flush=True)
            vm.qmp('input-send-event', {'events': events}, endpoint)
        wait('TEXTEDIT_PERF_PASS')
        vm.qmp('screendump', {'filename': str(output / 'final.ppm')}, endpoint)
        report = {'stages': stages, 'original_before': hashes,
                  'original_after': {name: packaging.sha(original / name) for name in hashes},
                  'fixture_elf_sha256': packaging.sha(binary), 'trace_clock_hz': 10000000,
                  'limitations': ['Instrumented resident services; timings are QEMU observations.',
                                  'Trace phase includes fence-key delivery; fixture timings exclude it.']}
        (output / 'capture.json').write_text(json.dumps(report, indent=2) + '\n')
    finally:
        vm.DIST = original
        if process is not None:
            if process.poll() is None:
                vm.qmp('quit', endpoint=endpoint)
            process.wait(timeout=10)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', required=True)
    capture(parser.parse_args().output)
