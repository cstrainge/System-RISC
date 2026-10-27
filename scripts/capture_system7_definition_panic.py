#!/usr/bin/env python3
"""Private terminal-failure evidence. Rust owns pixel/restart/diagnostic acceptance."""
import argparse
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import time

from hfs_b_tool import Image
from system7_capture import OwnedProcesses
from system7_fixture_build import guest_env as fixture_guest_env, host_env as fixture_host_env
from system7_platform import TARGET, display_size, tablet_axis
import capture_system7_conformance as packaging
from capture_system7_applications_ui import Driver
import system7_vm as vm


def capture(output, scenario):
    output.mkdir(parents=True, exist_ok=False)
    (output / 'pid').write_text(str(os.getpid()))
    original = vm.DIST
    before = {name: packaging.sha(original / name) for name in
              ['System.bin', 'System.rsrc', 'system7.qcow2', 'bootrom.bin']}
    owners = OwnedProcesses()
    private = output / 'dist'
    process = endpoint = None
    stages = []
    actions = []
    report = {'scenario': scenario, 'original_before': before, 'stages': stages,
              'input': actions, 'result': 'transport-incomplete'}
    try:
        fixture = vm.ROOT / 'System7/crates/mac-toolbox/tests/fixtures/definition-panic'
        target = vm.build_dir() / 'fixtures/definition-panic'
        manifest = fixture / 'Cargo.toml'
        subprocess.run(['cargo', 'build', '--manifest-path', str(manifest), '--locked', '--release',
                        '--features', 'guest', '--target', TARGET, '--bin', 'CDEF_PANIC',
                        '--target-dir', str(target)], env=fixture_guest_env(target), check=True)
        elf = target / TARGET / 'release/CDEF_PANIC'
        extra = []
        if scenario != 'definition':
            system_target = vm.build_dir() / 'fixtures' / ('terminal-' + scenario)
            features = 'guest,emergency-qualification'
            if scenario == 'oom':
                features += ',emergency-qualification-oom'
            subprocess.run(['cargo', 'build', '--manifest-path', str(vm.SOURCE / 'Cargo.toml'),
                            '--locked', '--release', '--features', features, '--target', TARGET,
                            '-p', 'mac-system', '--bin', 'System', '--target-dir', str(system_target)],
                           env=fixture_guest_env(system_target), check=True)
            extra = [str(system_target / TARGET / 'release/System'),
                     str(vm.build_dir() / 'target' / TARGET / 'release')]
        packaged = output / 'System.bin'
        subprocess.run(['cargo', 'run', '--manifest-path', str(manifest), '--locked', '--bin',
                        'package', '--target-dir', str(target), '--', str(elf),
                        str(original / 'System.bin'), str(packaged), *extra],
                       env=fixture_host_env(target), check=True)
        report['mutation'] = json.loads(packaged.with_suffix('.mutation.json').read_text())
        report['probe_elf_sha256'] = packaging.sha(elf)
        private.mkdir()
        raw = private / 'definition-panic.raw'
        subprocess.run(['qemu-img', 'convert', '-f', 'qcow2', '-O', 'raw', '-S', '4k',
                        str(original / 'system7.qcow2'), str(raw)], check=True)
        image = Image(raw)
        image.remove('/System Folder/System')
        image.import_macbinary(packaged, '/System Folder')
        subprocess.run(['qemu-img', 'convert', '-f', 'raw', '-O', 'qcow2', str(raw),
                        str(private / 'system7.qcow2')], check=True)
        shutil.copyfile(original / 'bootrom.bin', private / 'bootrom.bin')
        vm.DIST = private
        process, endpoint = owners.launch(output / 'vm', frontend='headless')
        serial = output / 'vm/serial.log'

        def log():
            return serial.read_text(errors='replace')

        def wait(predicate, description, seconds=30):
            deadline = time.monotonic() + seconds
            while time.monotonic() < deadline:
                text = log()
                if predicate(text):
                    return text
                if process.poll() is not None:
                    raise RuntimeError('Owned terminal guest exited before ' + description + ':\n' + text[-10000:])
                time.sleep(.01)
            raise TimeoutError('Terminal checkpoint missing: ' + description + '\n' + log()[-10000:])

        def send(events, label):
            vm.qmp('input-send-event', {'events': events}, endpoint)
            actions.append({'label': label, 'events': events})
            time.sleep(.08)

        def move(x, y):
            send([{'type': 'abs', 'data': {'axis': 'x', 'value': tablet_axis(x, 'x')}},
                  {'type': 'abs', 'data': {'axis': 'y', 'value': tablet_axis(y, 'y')}}], 'terminal-pointer')

        def button(down):
            send([{'type': 'btn', 'data': {'button': 'left', 'down': down}}], 'terminal-button')

        def key(name, down):
            send([{'type': 'key', 'data': {'key': {'type': 'qcode', 'data': name}, 'down': down}}], 'terminal-key')

        def frame(name):
            filename = name + '.ppm'
            vm.qmp('screendump', {'filename': str(output / filename)}, endpoint)
            stages.append({'stage': name, 'frame': filename})
            (output / 'stages.json').write_text(json.dumps(stages, indent=2) + '\n')

        driver = Driver(endpoint, serial)
        driver.click(40, 95, double=True)
        driver.wait_folder_ready('Applications')
        try:
            driver.application('Reactor Room')
        except (OSError, RuntimeError):
            if 'SYSTEM_EMERGENCY_READY ' not in log():
                raise
        text = wait(lambda text: 'SYSTEM_EMERGENCY_READY ' in text, 'terminal report')
        match = re.search(r'SYSTEM_EMERGENCY_READY kind=(\w+) flags=(\d+) restart=([\d,]+) panel=([\d,]+) ordinary_execution=abandoned', text)
        if not match:
            raise RuntimeError('Terminal readiness record malformed:\n' + text[-10000:])
        report['terminal'] = {'kind': match[1], 'flags': int(match[2]),
                              'restart': [int(n) for n in match[3].split(',')],
                              'panel': [int(n) for n in match[4].split(',')]}
        left, top, right, bottom = report['terminal']['restart']
        panel = report['terminal']['panel']
        # All captures hold the pointer away from diagnostic/bomb/button pixels.
        width, _height = display_size()
        first = (8, 8)
        second = (width - 24, 8)
        move(*first)
        report['cursor_positions'] = [first, second]
        frame('baseline')
        move(*second)
        frame('cursor-moved')
        move(*first)
        frame('cursor-restored')
        key('ret', True)
        frame('pressed')
        key('esc', True)
        key('esc', False)
        key('ret', False)
        frame('cancelled')
        if scenario == 'system-panic':
            # Mouse tracking cancellation and release-outside are meaningful
            # unhappy paths before the actual pointer-triggered Restart.
            move((left + right) // 2, (top + bottom) // 2)
            button(True)
            frame('pointer-pressed')
            move(*first)
            button(False)
            frame('pointer-cancelled')
            move((left + right) // 2, (top + bottom) // 2)
            button(True)
            button(False)
        else:
            key('ret', True)
            key('ret', False)
        wait(lambda text: text.count('System RISC Boot ROM v1') >= 2
             and text.count('DESKTOP_READY') >= 2, 'second ROM and desktop', seconds=60)
        frame('rebooted')
        report['result'] = 'transport-complete'
        report['restart_input'] = 'pointer' if scenario == 'system-panic' else 'keyboard'
        report['original_after'] = {name: packaging.sha(original / name) for name in before}
    except BaseException as error:
        report['transport_error'] = str(error)
        if endpoint is not None and process is not None and process.poll() is None:
            try:
                vm.qmp('screendump', {'filename': str(output / 'failure.ppm')}, endpoint)
            except (OSError, RuntimeError, ValueError) as evidence_error:
                report['failure_capture_error'] = str(evidence_error)
        raise
    finally:
        vm.DIST = original
        try:
            cleanup = owners.cleanup(output, [private, output / 'vm'])
            report['cleanup'] = cleanup
        finally:
            (output / 'capture.json').write_text(json.dumps(report, indent=2) + '\n')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--scenario', choices=['definition', 'system-panic', 'oom'], default='definition')
    args = parser.parse_args()
    capture(args.output.resolve(), args.scenario)
