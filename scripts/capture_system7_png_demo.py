#!/usr/bin/env python3
"""Capture the standalone QuickDraw Test PNG page launched from Finder; Rust owns pixel and completion assertions."""
import argparse
import hashlib
import json
from pathlib import Path
import time
import system7_vm as vm


def capture(output):
    output = Path(output).resolve()
    output.mkdir(parents=True, exist_ok=True)
    disk = vm.DIST / 'system7.qcow2'
    before = hashlib.sha256(disk.read_bytes()).hexdigest()
    process, endpoint = vm.launch(output / 'vm')
    serial = output / 'vm/serial.log'
    def wait(marker):
        deadline = time.monotonic() + 10
        while marker not in serial.read_text(errors='replace'):
            if process.poll() is not None or time.monotonic() > deadline:
                raise RuntimeError('PNG capture synchronization: ' + marker + '\n' + serial.read_text()[-3000:])
            time.sleep(.02)
    def key(code):
        vm.qmp('input-send-event', {'events': [
            {'type':'key','data':{'key':{'type':'qcode','data':code},'down':True}},
            {'type':'key','data':{'key':{'type':'qcode','data':code},'down':False}}]}, endpoint)
    try:
        key('q'); wait('QD_DEMO_READY page=1')
        key('0'); wait('QD_DEMO_READY page=10 png')
        # Ensure the retained scene has reached the framebuffer before transport.
        time.sleep(.15)
        vm.qmp('screendump', {'filename': str(output / 'finder-png.ppm')}, endpoint)
        (output / 'capture.json').write_text(json.dumps({
            'base_before':before,'base_after':hashlib.sha256(disk.read_bytes()).hexdigest(),
            'serial':'vm/serial.log','frame':'finder-png.ppm'},indent=2)+'\n')
    finally:
        vm.qmp('quit', endpoint=endpoint)
        process.wait(timeout=10)

if __name__ == '__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output',required=True)
    capture(parser.parse_args().output)
