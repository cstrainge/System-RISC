#!/usr/bin/env python3
"""Private boot/display transport. Rust owns pixel and SDK acceptance."""
from system7_platform import display_size
from system7_platform import CPU, RAM
import argparse
import json
from pathlib import Path
import shutil
import subprocess
import time
import system7_vm as vm
from capture_system7_applications_ui import Driver


def capture(output):
    output = Path(output).resolve()
    output.mkdir(parents=True, exist_ok=False)
    for mode in ['missing', 'boot']:
        state = output / mode
        state.mkdir()
        shutil.copyfile(vm.DIST / 'bootrom.bin', state / 'bootrom.bin')
        if mode == 'boot':
            shutil.copyfile(vm.DIST / 'system7.qcow2', state / 'system7.qcow2')
        endpoint = {'transport': 'unix', 'path': str(state / 'qmp.sock')}
        serial = state / 'serial.log'
        args = ['qemu-system-riscv64', '-machine', 'virt', '-cpu', CPU, '-m', RAM,
                '-smp', '1', '-bios', 'none', '-drive',
                f'if=pflash,unit=0,format=raw,readonly=on,file={state}/bootrom.bin',
                '-global', 'virtio-mmio.force-legacy=false', '-device', 'ramfb',
                '-device', 'virtio-keyboard-device', '-device', 'virtio-tablet-device',
                '-display', 'none', '-nic', 'none', '-monitor', 'none', '-parallel', 'none',
                '-serial', f'file:{serial}', '-qmp',
                f'unix:{state}/qmp.sock,server=on,wait=off', '-S']
        if mode == 'boot':
            args += ['-drive', f'if=none,id=startup,format=qcow2,readonly=on,file={state}/system7.qcow2',
                     '-device', 'virtio-blk-device,drive=startup,bus=virtio-mmio-bus.0']
        (state / 'command.json').write_text(json.dumps(args, indent=2))
        with (state / 'qemu.log').open('wb') as log:
            process = subprocess.Popen(args, stdout=log, stderr=log)
        (state / 'pid').write_text(str(process.pid))

        def wait(marker):
            deadline = time.monotonic() + 25
            while time.monotonic() < deadline:
                text = serial.read_text(errors='replace') if serial.exists() else ''
                if marker in text:
                    return
                if process.poll() is not None:
                    raise RuntimeError((state / 'qemu.log').read_text() + text)
                time.sleep(.001)
            raise TimeoutError(marker + '\n' + text)

        def frame(name):
            vm.qmp('stop', endpoint=endpoint)
            vm.qmp('screendump', {'filename': str(output / (name + '.ppm'))}, endpoint)
            (output / (name + '.serial.log')).write_text(serial.read_text(errors='replace'))
            vm.qmp('cont', endpoint=endpoint)

        try:
            deadline = time.monotonic() + 10
            while time.monotonic() < deadline:
                try:
                    vm.qmp('query-status', endpoint=endpoint)
                    break
                except (OSError, RuntimeError):
                    time.sleep(.02)
            vm.qmp('cont', endpoint=endpoint)
            if mode == 'missing':
                wait('BOOT_VISUAL_MISSING')
                frame('missing')
            else:
                wait('BOOT_VISUAL_HAPPY')
                frame('happy')
                wait('BOOT_PANEL_READY')
                frame('panel')
                wait('DESKTOP_READY')
                driver = Driver(endpoint, serial)
                driver.click(display_size()[0] - 40, 44)
                driver.key('i', ['ctrl'])
                driver.wait('FINDER_INFO_OPEN')
                driver.move(630, 460)
                time.sleep(.05)
                frame('volume-info')
        finally:
            if process.poll() is None:
                try:
                    vm.qmp('quit', endpoint=endpoint)
                except (OSError, RuntimeError):
                    process.kill()
            process.wait(timeout=10)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', required=True)
    capture(parser.parse_args().output)
