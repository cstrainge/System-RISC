#!/usr/bin/env python3
"""Exercise the normal export launcher and actual Finder in a private VM.

Rust owns acceptance assertions. This transport creates private exports, sends
physical input, retains frames/serial, and records companion/process lifetimes.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import time

from capture_system7_applications_ui import Driver
import system7_vm as vm
from system7_paths import build_dir, host_dir
from system7_platform import display_size


def running(pid):
    try:
        # A reaped or zombie child has released its sockets and directory leases.
        return Path(f'/proc/{pid}/stat').read_text().rsplit(')', 1)[1].split()[0] != 'Z'
    except FileNotFoundError:
        return False


def capture(output, windows=False, default_transfer=False, application=False):
    output.mkdir(parents=True, exist_ok=False)
    (output / 'pid').write_text(str(os.getpid()))
    before = hashlib.sha256((vm.DIST / 'system7.qcow2').read_bytes()).hexdigest()
    subprocess.run(['cargo', 'build', '--locked', '-p', 'risc-forks',
                    '--target-dir', str(host_dir())], cwd=vm.ROOT / 'System7', check=True)
    tool = host_dir() / 'debug/risc-forks'
    exports = []
    for directory, name in ([] if default_transfer else [('writable', 'Desktop Host'), ('readonly', 'Desktop Read Only')]):
        folder = output / directory
        subprocess.run([str(tool), 'init', str(folder), name], check=True)
        if application:
            subprocess.run([str(tool), 'import-macbinary', str(folder),
                            str(vm.DIST / 'QuickDraw Test.bin')], check=True)
        else:
            source = output / 'Example.txt'
            source.write_text('Shared through the normal launcher.\n')
            subprocess.run([str(tool), 'import', str(folder), str(source)], check=True)
        exports.append(str(folder))
    state = output / 'vm'
    endpoint = None
    owner = None
    try:
        command = ['python3', str(vm.ROOT / 'System7/scripts/system7_vm.py'), 'start',
                   '--no-build', '--frontend', 'native' if windows else 'headless',
                   '--qmp-port', '4449', '--state-dir', str(state),
                   '--build-dir', str(build_dir())]
        if not default_transfer:
            command.extend(['--no-transfer', '--export', f'Writable={exports[0]}',
                            '--read-only-export', f'ReadOnly={exports[1]}'])
        result = subprocess.run(command, capture_output=True, text=True, timeout=180)
        (output / 'launcher.stdout').write_text(result.stdout)
        (output / 'launcher.stderr').write_text(result.stderr)
        if result.returncode:
            raise RuntimeError('normal launcher failed:\n' + result.stdout + result.stderr)
        endpoint = json.loads((state / 'qmp-endpoint.json').read_text())
        owner = json.loads((state / 'host-exports.json').read_text())
        guest_pid = int((state / 'pid').read_text())
        serial = state / 'serial.log'
        driver = Driver(endpoint, serial)
        vm.qmp('screendump', {'filename': str(output / 'mounted-desktop.ppm')}, endpoint)
        # Selection can repaint the startup folder. A generic READY line is not
        # evidence that the host folder opened, nor is its initial VOLUME line.
        name = 'Transfer' if default_transfer else 'Desktop Host'
        marker = f' name={name}\n'
        previous = driver.log().count(marker)
        clicks = driver.log().count('FINDER_CLICK_HANDLED\n')
        width, _ = display_size()
        driver.click(width - 40, 96)
        driver.wait('FINDER_CLICK_HANDLED\n', clicks)
        driver.key('o', ('ctrl',))
        driver.wait(marker, previous)
        driver.wait_folder_ready(name)
        driver.wait_state(lambda state: state['windows'] == '2')
        time.sleep(.1)
        vm.qmp('screendump', {'filename': str(output / 'host-folder.ppm')}, endpoint)
        if application:
            driver.click(56, 106, double=True)
            driver.wait('QUICKDRAW_TEST_READY\n')
            driver.wait_state(lambda state: state['windows'] == '3')
            vm.qmp('screendump', {'filename': str(output / 'external-application.ppm')}, endpoint)
            # Quit the actual application, then immediately use its source folder.
            driver.key('q', ('ctrl',))
            driver.wait('PROCESS_EXIT app=' + str(driver.app_id('QuickDraw Test')) + '\n')
            driver.wait_state(lambda state: state['windows'] == '2')
            driver.click(56, 106, double=True)
            driver.wait('QUICKDRAW_TEST_READY\n', 1)
            vm.qmp('screendump', {'filename': str(output / 'external-relaunched.ppm')}, endpoint)
        vm.qmp('quit', endpoint=endpoint)
        endpoint = None
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline and (running(guest_pid) or running(owner['companion_pid'])):
            time.sleep(.02)
        after = hashlib.sha256((vm.DIST / 'system7.qcow2').read_bytes()).hexdigest()
        (output / 'capture.json').write_text(json.dumps({
            'serial': str(serial), 'frontend': 'native' if windows else 'headless',
            'original_before': before, 'original_after': after,
            'guest_running_after_quit': running(guest_pid),
            'companion_running_after_quit': running(owner['companion_pid']),
            'exports': owner, 'command': json.loads((state / 'command.json').read_text())
        }, indent=2) + '\n')
    finally:
        try:
            if endpoint is not None:
                vm.qmp('quit', endpoint=endpoint)
        finally:
            try:
                if owner is not None:
                    deadline = time.monotonic() + 5
                    while running(owner['companion_pid']) and time.monotonic() < deadline:
                        time.sleep(.02)
                    if running(owner['companion_pid']):
                        os.kill(owner['companion_pid'], 15)
            finally:
                for name in ['system7.qcow2', 'bootrom.bin']:
                    (state / name).unlink(missing_ok=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', required=True, type=Path)
    parser.add_argument('--windows', action='store_true')
    parser.add_argument('--default-transfer', action='store_true')
    parser.add_argument('--application', action='store_true')
    args = parser.parse_args()
    capture(args.output.resolve(), args.windows, args.default_transfer, args.application)
