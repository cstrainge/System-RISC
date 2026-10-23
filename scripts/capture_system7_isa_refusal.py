#!/usr/bin/env python3
"""Capture the early ISA refusal; Rust owns acceptance. No disk is needed."""
import argparse
import json
from pathlib import Path
import subprocess
import time
import system7_vm as vm

def capture(output, cpu):
    output = Path(output).resolve()
    output.mkdir(parents=True, exist_ok=False)
    serial = output / 'serial.log'
    args = ['qemu-system-riscv64', '-machine', 'virt', '-cpu', cpu,
            '-m', '128M', '-smp', '1', '-bios', 'none', '-display', 'none',
            '-monitor', 'none', '-nic', 'none', '-serial', 'file:' + str(serial),
            '-drive', 'if=pflash,unit=0,format=raw,readonly=on,file=' + str(vm.DIST/'bootrom.bin')]
    with (output/'qemu.log').open('wb') as log:
        process = subprocess.Popen(args, stdout=log, stderr=log)
    try:
        deadline = time.monotonic() + 10
        while process.poll() is None and time.monotonic() < deadline:
            text = serial.read_text(errors='replace') if serial.exists() else ''
            if '\n' in text:
                break
            time.sleep(.01)
        (output/'capture.json').write_text(json.dumps({'command':args, 'serial':'serial.log'})+'\n')
    finally:
        process.terminate()
        process.wait(timeout=10)

if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', required=True)
    parser.add_argument('--cpu', required=True)
    args = parser.parse_args()
    capture(args.output, args.cpu)
