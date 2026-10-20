#!/usr/bin/env python3
"""Capture native userspace interactions. Rust owns the conformance assertions.

This launches a fresh read-only headless guest, never the user's visible VM.
Waiting for input acknowledgements is transport synchronization, not an oracle.
"""
from hfs_b_tool import Image as NativeImage
import argparse
import hashlib
import json
from pathlib import Path
import time
import os
import shutil
import subprocess
import system7_vm as vm
import system7_observer as observer


def private_probe_disk(output, menus=False, deferred=False):
    fixture = vm.ROOT / 'System7/crates/mac-toolbox/tests/fixtures/window-probe'
    target = vm.ROOT / 'build/system7/window-probe-target'
    subprocess.run(['cargo','build','--manifest-path',str(fixture/'Cargo.toml'),'--locked',
                    '--release','--target','riscv64imac-unknown-none-elf','--bin','Finder',
                    '--target-dir',str(target),*(['--features','deferred-close'] if deferred else ['--features','menus'] if menus else [])],cwd=vm.ROOT,check=True)
    packaged = output / 'package'
    subprocess.run(['cargo','run','--manifest-path',str(fixture/'Cargo.toml'),'--locked',
                    '--bin','package','--target-dir',str(target),'--',
                    str(target/'riscv64imac-unknown-none-elf/release/Finder'),str(packaged)],
                   cwd=vm.ROOT,check=True)
    private = output / 'dist'
    private.mkdir()
    raw = private / 'probe.raw'
    subprocess.run(['qemu-img','convert','-f','qcow2','-O','raw','-S','4k',
                    str(vm.DIST/'system7.qcow2'),str(raw)],check=True)
    fs = NativeImage(raw)
    fs.remove('/System Folder/Finder')
    fs.import_macbinary(packaged / 'Finder.bin', '/System Folder')
    shutil.copyfile(vm.DIST/'bootrom.bin',private/'bootrom.bin')
    subprocess.run(['qemu-img','convert','-f','raw','-O','qcow2',str(raw),
                    str(private/'system7.qcow2')],check=True)
    raw.unlink()
    return private

def capture(output, probe=False, menus=False):
    output = Path(output).resolve()
    output.mkdir(parents=True, exist_ok=True)
    disk = vm.DIST / 'system7.qcow2'
    disk_before = hashlib.sha256(disk.read_bytes()).hexdigest()
    if probe:
        vm.DIST = private_probe_disk(output,menus)
    process, endpoint = vm.launch(output / 'vm')
    serial = output / 'vm/serial.log'
    observations = []

    def state():
        return observer.read(endpoint, serial)

    def wait(predicate):
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            current = state()
            if current and predicate(current):
                return
            if process.poll() is not None:
                raise RuntimeError('Guest stopped during capture:\n' + serial.read_text()[-4000:])
            time.sleep(.02)
        raise TimeoutError('Input capture synchronization timed out: ' + str(state()))

    def send(events):
        vm.qmp('input-send-event', {'events': events}, endpoint)

    def move(x, y):
        send([{'type': 'abs', 'data': {'axis': 'x', 'value': (x * 32767 + 638) // 639}},
              {'type': 'abs', 'data': {'axis': 'y', 'value': (y * 32767 + 478) // 479}}])
        wait(lambda current: current.get('mouse') == f'{x},{y}')

    def button(down):
        previous = int(state()['input_seq'])
        send([{'type': 'btn', 'data': {'button': 'left', 'down': down}}])
        wait(lambda current: int(current['input_seq']) > previous)

    def key_transition(code, down):
        previous = int(state()['input_seq'])
        send([{'type':'key','data':{'key':{'type':'qcode','data':code},'down':down}}])
        wait(lambda current: int(current['input_seq']) > previous)

    def snapshot(name):
        # Let SDK scene commits following a release reach the compositor.
        time.sleep(.12)
        path = output / f'{name}.ppm'
        vm.qmp('screendump', {'filename': str(path)}, endpoint)
        observations.append({'name': name, 'state': state(), 'frame': path.name})

    try:
        if menus:
            wait(lambda _: 'WINDOW_PROBE_READY' in serial.read_text(errors='replace'))
            move(16,444);snapshot('menu-baseline')
            move(50,10);button(True);snapshot('menu-file')
            move(50,28);snapshot('menu-file-highlight')
            button(False);move(16,444);snapshot('menu-selected')
            move(50,10);button(True);move(50,44);snapshot('menu-disabled')
            button(False);move(16,444);snapshot('menu-disabled-release')
            move(50,10);button(True);move(500,400);snapshot('menu-outside')
            button(False);move(16,444);snapshot('menu-cancelled')
            move(50,10);button(True);move(90,10);snapshot('menu-edit')
            move(500,400);button(False)
            key_transition('ctrl',True);key_transition('n',True);key_transition('n',False);key_transition('ctrl',False)
            move(16,444);snapshot('menu-key')
            key_transition('ctrl',True);key_transition('o',True);key_transition('o',False);key_transition('ctrl',False)
            key_transition('ctrl',True);key_transition('w',True);key_transition('w',False);key_transition('ctrl',False)
            move(16,444);snapshot('menu-disabled-key')
            key_transition('g',True);key_transition('g',False)
            wait(lambda _: 'MENU_LABEL width=' in serial.read_text(errors='replace'))
            move(20,10);button(True);snapshot('menu-keyless')
            move(16,444);button(False)
            key_transition('h',True);key_transition('h',False)
            wait(lambda _: 'MENU_KEYED_READY' in serial.read_text(errors='replace'))
            move(20,10);button(True);snapshot('menu-keyed')
            move(16,444);button(False)
        elif probe:
            wait(lambda _: 'WINDOW_PROBE_READY' in serial.read_text(errors='replace'))
            move(16,444); snapshot('baseline')
            move(230,88); button(True); move(289,127); snapshot('drag-held')
            button(False); move(16,444); snapshot('drag-committed')
            move(289,127); button(True); move(230,88); button(False)
            move(16,444); snapshot('drag-restored')
            move(537,397); button(True); move(582,426); snapshot('grow-held')
            button(False); move(16,444); snapshot('grow-committed')
            move(582,426); button(True); move(537,397); button(False)
            move(16,444); snapshot('grow-restored')
            move(537,397); button(True); button(False); move(16,444); snapshot('grow-no-motion')
        else:
            wait(lambda _: 'FINDER_FOLDER_READY window=' in serial.read_text(errors='replace'))
            move(16,444); snapshot('baseline')
            move(150,33); button(True); move(210,66); snapshot('drag-held')
            move(150,10); snapshot('drag-outside-held')
            button(False); move(16,444); snapshot('drag-cancelled')
            move(150,33); button(True); move(150,10); move(210,66); snapshot('drag-reentered')
            button(False); move(16,444); snapshot('drag-committed')
            move(465,260); button(True); move(405,230); snapshot('grow-held')
            button(False); move(16,444); snapshot('grow-committed')
            move(405,230); button(True); move(465,260); button(False)
            move(210,66); button(True); move(150,33); button(False)
            move(16,444); snapshot('roundtrip')
            move(21,36); button(True); snapshot('close-held')
            move(20,440); snapshot('close-left')
            move(21,36); snapshot('close-reentered')
            move(20,440); button(False); move(16,444); snapshot('close-cancelled')
            move(398,35); button(True); snapshot('zoom-held')
            move(20,440); snapshot('zoom-left')
            move(398,35); snapshot('zoom-reentered')
            move(20,440); button(False); move(16,444); snapshot('zoom-cancelled')
            move(398,35); button(True); button(False); move(16,444); snapshot('zoom-committed')
            move(624,35); button(True); button(False); move(16,444); snapshot('zoom-restored')
            key_transition('a',True); key_transition('a',False); snapshot('background-baseline')
            key_transition('ctrl',True)
            move(150,33); button(True); move(210,66); snapshot('command-drag-held')
            button(False); key_transition('ctrl',False); move(16,444); snapshot('command-drag-committed')
            move(210,66); button(True); move(150,10); button(False)
            move(16,444); snapshot('background-drag-cancelled')
            move(210,66); button(True); move(150,33); snapshot('background-drag-held')
            button(False); move(16,444); snapshot('background-drag-activated')
            move(21,36); button(True); button(False); move(16,444); snapshot('close-accepted')
            move(263,140); button(True); button(False); move(16,444); snapshot('last-close-accepted')

        report = {'profile': 'native-rv64-640x480', 'observations': observations,
                  'disk_sha256_before': disk_before,
                  'disk_sha256_after': hashlib.sha256(disk.read_bytes()).hexdigest(),
                  'launch': json.loads((output / 'vm/command.json').read_text()),
                  'menu_sizing_pass': 'MENU_SIZING_PASS cases=8' in serial.read_text(errors='replace'),
                  'menu_events':[line for line in serial.read_text(errors='replace').splitlines() if line.startswith('MENU_PROBE ')],
                  'native_menu_keys':[line for line in serial.read_text(errors='replace').splitlines() if line.startswith('MENU_NATIVE_KEY ')],
                  'menu_label_width': next((int(line.split('=')[1]) for line in serial.read_text(errors='replace').splitlines() if line.startswith('MENU_LABEL width=')),None)}
        (output / 'capture.json').write_text(json.dumps(report, indent=2) + '\n')
    finally:
        try:
            vm.qmp('quit', endpoint=endpoint)
        finally:
            process.wait(timeout=10)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', required=True)
    parser.add_argument('--probe', action='store_true', help='private application using only publicSDK')
    parser.add_argument('--menus', action='store_true')
    args=parser.parse_args()
    capture(args.output,args.probe or args.menus,args.menus)
