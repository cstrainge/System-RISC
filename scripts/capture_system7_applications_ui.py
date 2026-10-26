#!/usr/bin/env python3
"""Transport for real Finder launches and system application-menu interaction.

This file sends physical QMP input and retains serial/frames. Canonical Rust
tests own acceptance decisions. It never changes the visible desktop VM.
"""
from system7_platform import tablet_axis, display_size
from hfs_b_tool import Image as NativeImage
import argparse
import json
import os
import shutil
import struct
import subprocess
from pathlib import Path
import time
import re

import capture_system7_conformance as packaging
import system7_vm as vm
import system7_observer as observer
from system7_resource_map import resource_map


# Fresh 404px Finder folder, resource-backed Geneva names and 64px icon cells.
# Wide captions reserve cells; installed apps continue on the second row.
APPLICATION_POINTS = {
    'Cursor Test': (56, 106),
    'Panic Test': (120, 106),
    'QuickDraw Test': (184, 106),
    'Reactor Room': (312, 106),
    'Res Edit': (376, 106),
    'Text Edit': (56, 158),
    'Z QuickDraw Copy': (184, 158),
}


class Driver:
    def __init__(self, endpoint, serial):
        self.endpoint = endpoint
        self.serial = Path(serial)

    def log(self):
        return self.serial.read_text(errors='replace')

    def app_id(self, name):
        matches = re.findall(r'PROCESS_(?:REGISTERED|LAUNCHED) app=(\d+)(?: thread=\d+)? name=' + re.escape(name) + r'\n', self.log())
        if not matches:
            raise RuntimeError('No registered application: ' + name)
        return int(matches[-1])

    def state(self):
        return observer.read(self.endpoint, self.serial)

    def wait_state(self, predicate):
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            current = self.state()
            if predicate(current):return current
            log = self.log()
            if any(marker in log for marker in ['SYSTEM_PANIC:', 'SYSTEM_TRAP:',
                                                'SYSTEM_ALLOCATION_FAILED:']):
                raise RuntimeError('Guest stopped while awaiting input:\n' + log[-5000:])
            time.sleep(.01)
        raise TimeoutError('Guest did not acknowledge input: ' + str(self.state()))

    def wait(self, marker, previous=0):
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            text = self.log()
            if text.count(marker) > previous:
                return text
            if any(failure in text for failure in ['SYSTEM_PANIC:', 'SYSTEM_TRAP:',
                                                   'SYSTEM_ALLOCATION_FAILED:',
                                                   'TEXT_EDIT_ERROR',
                                                   'QUICKDRAW_TEST_ERROR']):
                raise RuntimeError(text[-5000:])
            time.sleep(.01)
        raise TimeoutError('Guest did not acknowledge ' + marker + '\n' + self.log()[-5000:])

    def wait_folder_ready(self, name):
        # Volume discovery also logs "name=...". Wait for an actual folder-open
        # record before selecting its window identity and completed snapshot.
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            rows = [line for line in self.log().splitlines()
                    if line.startswith('FINDER_FOLDER_OPEN ') and line.endswith(' name='+name)]
            if rows:
                identity = rows[-1].split(' window=',1)[1].split(' ',1)[0]
                return self.wait('FINDER_FOLDER_READY window='+identity+'\n')
            time.sleep(.01)
        raise TimeoutError('Guest did not open folder '+name+'\n'+self.log()[-5000:])

    def send(self, events):
        vm.qmp('input-send-event', {'events': events}, self.endpoint)

    def move(self, x, y):
        self.send([{'type': 'abs', 'data': {'axis': 'x', 'value': tablet_axis(x, 'x')}},
                   {'type': 'abs', 'data': {'axis': 'y', 'value': tablet_axis(y, 'y')}}])
        self.wait_state(lambda state: state['mouse'] == f'{x},{y}')

    def button(self, down):
        self.send([{'type': 'btn', 'data': {'button': 'left', 'down': down}}])
        time.sleep(.06)

    def click(self, x, y, double=False):
        self.move(x, y)
        for _ in range(2 if double else 1):
            self.button(True)
            self.button(False)

    def application(self, name, double=True):
        self.click(*APPLICATION_POINTS[name], double=double)

    def key(self, name, modifiers=()):
        def event(code, down):
            return {'type': 'key', 'data': {'key': {'type': 'qcode', 'data': code}, 'down': down}}
        events = [event(code, True) for code in modifiers]
        events += [event(name, True), event(name, False)]
        events += [event(code, False) for code in reversed(modifiers)]
        self.send(events)
        time.sleep(.06)

    def key_state(self, name, down):
        self.send([{'type': 'key', 'data': {'key': {'type': 'qcode', 'data': name},
                                          'down': down}}])
        time.sleep(.06)

    def discard_text_edit(self, quit=True):
        """Physical File command followed by the resource-based Don't Save button."""
        before = int(self.state()['windows'])
        self.key('q' if quit else 'w', ('ctrl',))
        self.wait_state(lambda state: int(state['windows']) > before)
        # System positions the 366x136 Save Changes ALRT in the upper fifth.
        # The reference 640x480 content origin is137,89; Don't Save is
        # local20,96..124,118. Follow the structure's centered/upper-fifth
        # placement as the checked display grows.
        width, height = display_size()
        self.click(193 + (width - 640) // 2, 196 + (height - 480) // 5)

    def menu(self, x, row_y, popup_x=None):
        self.move(x, 10)
        self.button(True)
        self.move(x if popup_x is None else popup_x, row_y)
        self.button(False)


def open_textedit(endpoint, serial):
    """Fresh actual Finder: open Applications, then double-click TextEdit.

    Shared by the existing typing/caret captures after the exerciser became a
    separate application. Returns after the real editor has drawn its window.
    """
    driver = Driver(endpoint, serial)
    driver.click(40, 95, double=True)
    driver.wait_folder_ready('Applications')
    driver.application('Text Edit')
    driver.wait('TEXT_EDIT_READY\n')
    return driver


def icon_disk(output):
    """Same executable, but non-128 icon IDs prove bundle-based lookup."""
    private = output / 'dist'
    private.mkdir()
    raw = private / 'icons.raw'
    subprocess.run(['qemu-img', 'convert', '-f', 'qcow2', '-O', 'raw', '-S', '4k',
                    str(vm.DIST / 'system7.qcow2'), str(raw)], check=True)
    binary = bytearray((vm.DIST / 'QuickDraw Test.bin').read_bytes())
    data_length, fork_length = struct.unpack_from('>II', binary, 83)
    fork = 128 + (data_length + 127) // 128 * 128
    for entry in resource_map(bytes(binary[fork:fork + fork_length]))['resources']:
        kind, resource_id = bytes(entry['kind']), entry['id']
        if kind == b'ICN#' and resource_id == 128:
            struct.pack_into('>h', binary, fork + entry['reference_offset'], 9433)
        if kind == b'BNDL' and resource_id == 128:
            payload = fork + entry['payload_offset']
            # Fixture metadata is pinned by the canonical Rust assertions.
            binary[payload + 16:payload + 18] = struct.pack('>h', 9433)
    package = private / 'QuickDraw Test.bin'
    package.write_bytes(binary)
    # Retain the small map/metadata fork identity, not another executable copy.
    (output / 'icon-fixture.json').write_text(json.dumps({
        'application': 'QuickDraw Test', 'icon_id': 9433,
        'package_sha256': packaging.sha(package), 'resource_length': fork_length}, indent=2) + '\n')
    fs = NativeImage(raw)
    fs.remove('/Applications/QuickDraw Test')
    fs.import_macbinary(package, '/Applications')
    fs.import_macbinary(package, '/Applications', name='Z QuickDraw Copy')
    shutil.copyfile(vm.DIST / 'bootrom.bin', private / 'bootrom.bin')
    subprocess.run(['qemu-img', 'convert', '-f', 'raw', '-O', 'qcow2', str(raw),
                    str(private / 'system7.qcow2')], check=True)
    return private


def capture(output, remap_icon=False, windows=False):
    output = Path(output).resolve()
    output.mkdir(parents=True, exist_ok=False)
    (output / 'pid').write_text(str(os.getpid()))
    original = vm.DIST
    before = {name: packaging.sha(original / name)
              for name in ['system7.qcow2', 'bootrom.bin', 'System.rsrc']}
    private = output / 'dist'
    try:
        if remap_icon:
            vm.DIST = icon_disk(output)
        options = {'frontend': 'native-headless', 'qmp_port': 4457} if windows else {}
        process, endpoint = vm.launch(output / 'vm', writable=True, **options)
    except BaseException:
        if private.exists():
            shutil.rmtree(private)
        raise
    finally:
        vm.DIST = original
    driver = Driver(endpoint, output / 'vm/serial.log')
    snapshots = []

    def frame(name, park=True):
        if park:
            driver.move(16, 444)
        time.sleep(.1)
        path = output / (name + '.ppm')
        vm.qmp('screendump', {'filename': str(path)}, endpoint)
        snapshots.append({'name': name, 'frame': path.name,
                          'state': driver.state(),
                          'serial_length': len(driver.log().encode())})

    def activate(row, app):
        previous = driver.log().count(f'PROCESS_ACTIVE app={app}\n')
        driver.menu(display_size()[0]-22, 93 + row * 18, popup_x=display_size()[0]-60)
        driver.wait(f'PROCESS_ACTIVE app={app}\n', previous)

    def editor_key(name, modifiers=()):
        previous = driver.log().count('FINDER_TEXTEDIT_STATE ')
        driver.key(name, modifiers)
        driver.wait('FINDER_TEXTEDIT_STATE ', previous)

    try:
        frame('finder-initial')
        driver.move(display_size()[0]-22, 10)
        driver.button(True)
        frame('finder-only-menu')
        driver.button(False)
        clicks = driver.log().count('FINDER_CLICK_HANDLED\n')
        driver.click(40, 95, double=True)
        driver.wait_folder_ready('Applications')
        # Folder-open is logged before the second click's redraw completes.
        driver.wait('FINDER_CLICK_HANDLED\n', clicks + 1)
        frame('applications-folder')
        clicks = driver.log().count('FINDER_CLICK_HANDLED\n')
        driver.application('QuickDraw Test', double=False)
        driver.wait('FINDER_CLICK_HANDLED\n', clicks)
        driver.menu(52, 44, popup_x=80)  # Actual Finder File > Open resource item.
        driver.wait('QUICKDRAW_TEST_READY\n')
        frame('quickdraw-launched')
        driver.key('2')
        driver.wait('QD_DEMO_READY page=2 transfer\n')
        frame('quickdraw-page-two')
        activate(0, driver.app_id('Finder'))
        frame('finder-restored')
        clicks = driver.log().count('FINDER_CLICK_HANDLED\n')
        driver.click(320, 200)
        driver.wait('FINDER_CLICK_HANDLED\n', clicks)
        frame('running-file-icons')
        clicks = driver.log().count('FINDER_CLICK_HANDLED\n')
        driver.application('QuickDraw Test', double=False)
        driver.wait('FINDER_CLICK_HANDLED\n', clicks)
        info = driver.log().count('FINDER_INFO_OPEN ')
        driver.key('i', ('ctrl',));driver.wait('FINDER_INFO_OPEN ', info)
        frame('running-app-info')
        info = driver.log().count('FINDER_INFO_CLOSED ')
        driver.key('w', ('ctrl',));driver.wait('FINDER_INFO_CLOSED ', info)
        driver.application('Text Edit')
        driver.wait('TEXT_EDIT_READY\n')
        driver.key('a', ('ctrl',))
        driver.key('a')
        driver.key('b')
        driver.key('c')
        driver.wait('FINDER_TEXTEDIT_STATE length=3 selection=3..3\n')
        frame('textedit-abc')
        driver.move(display_size()[0]-22, 10)
        driver.button(True)
        frame('three-app-menu')
        driver.button(False)
        driver.menu(display_size()[0]-22, 44, popup_x=display_size()[0]-60)  # Hide Others.
        frame('hide-others')
        folder_id = re.search(r'FINDER_FOLDER_OPEN .* window=(\d+) name=Applications',
                              driver.log())[1]
        restored_ready = f'FINDER_FOLDER_READY window={folder_id}\n'
        restored_count = driver.log().count(restored_ready)
        driver.menu(display_size()[0]-22, 60, popup_x=display_size()[0]-60)  # Show All, foreground unchanged.
        # Restoring hidden windows posts real application updates. Capture
        # their completed drawing, not the still-visible tracking popup while
        # those updates are waiting for their application threads to run.
        driver.wait(restored_ready, restored_count)
        frame('show-all')
        # Expose a narrow editor-content strip to the right of QuickDraw. This
        # permits a real background-content click rather than a menu activation.
        driver.move(350, 80)
        driver.button(True)
        driver.move(380, 100)
        driver.button(False)
        editor_key('home', ('ctrl',))
        activate(1, driver.app_id('QuickDraw Test'))
        frame('background-editor-before-click')
        activate_count = driver.log().count(f"PROCESS_ACTIVE app={driver.app_id('Text Edit')}\n")
        driver.click(610, 120)
        driver.wait(f"PROCESS_ACTIVE app={driver.app_id('Text Edit')}\n", activate_count)
        editor_key('right')
        # Arrow probes expose the actual click result through the editor's
        # ordinary state report. The app emits no special testing-only service.
        frame('first-background-content-click')
        editor_key('home', ('ctrl',))
        frame('first-click-caret-reset')
        driver.click(610, 120)
        editor_key('left')
        frame('second-foreground-content-click')
        editor_key('end', ('ctrl',))
        frame('editor-caret-restored')
        driver.move(300, 66)
        driver.key_state('ctrl', True)
        driver.button(True)
        driver.move(310, 76)
        frame('background-command-drag-held', park=False)
        driver.button(False)
        driver.key_state('ctrl', False)
        frame('background-command-drag-released')
        driver.move(300, 76)
        driver.button(True)
        driver.move(310, 86)
        frame('background-ordinary-drag-held', park=False)
        activate_count = driver.log().count(f"PROCESS_ACTIVE app={driver.app_id('QuickDraw Test')}\n")
        driver.button(False)
        driver.wait(f"PROCESS_ACTIVE app={driver.app_id('QuickDraw Test')}\n", activate_count)
        frame('background-ordinary-drag-released')
        driver.key('3')
        driver.wait('QD_DEMO_READY page=3 regions\n')
        frame('quickdraw-resumed')
        activate(2, driver.app_id('Text Edit'))
        driver.key('ret')
        driver.key('d')
        driver.wait('FINDER_TEXTEDIT_STATE length=5 selection=5..5\n')
        frame('textedit-resumed')
        driver.discard_text_edit()  # Actual Quit and the unsaved-document dialog.
        driver.wait(f"PROCESS_EXIT app={driver.app_id('Text Edit')}\n")
        frame('textedit-quit')
        driver.menu(28, 60, popup_x=65)  # QuickDraw's own File > Quit.
        exit_marker = f"PROCESS_EXIT app={driver.app_id('QuickDraw Test')}\n"
        driver.wait(exit_marker)
        # Exit publication precedes Finder's queued icon/update work. Wait for
        # the exposed folder's committed drawing without injecting more input;
        # the canonical Rust assertion still checks the actual normal icon.
        folder_id = re.findall(r'FINDER_FOLDER_OPEN .* window=(\d+) name=Applications\n',driver.log())[-1]
        ready = f'FINDER_FOLDER_READY window={folder_id}\n'
        before_exit = driver.log().split(exit_marker,1)[0]
        driver.wait(ready,before_exit.count(ready))
        frame('quickdraw-quit')
        clicks = driver.log().count('FINDER_CLICK_HANDLED\n')
        driver.click(320, 200)
        driver.wait('FINDER_CLICK_HANDLED\n', clicks)
        frame('closed-file-icons')
        previous = driver.log().count('TEXT_EDIT_READY\n')
        driver.application('Text Edit')
        driver.wait('TEXT_EDIT_READY\n', previous)
        frame('textedit-relaunched')
        previous = driver.log().count('FINDER_TEXTEDIT_STATE length=1 selection=1..1\n')
        driver.key('a', ('ctrl',))
        driver.key('z')
        driver.wait('FINDER_TEXTEDIT_STATE length=1 selection=1..1\n', previous)
        frame('textedit-relaunched-input')
        driver.discard_text_edit()
        driver.wait(f"PROCESS_EXIT app={driver.app_id('Text Edit')}\n")
        driver.click(200, 36)  # Bring Finder's startup-volume window forward.
        driver.click(104, 95, double=True)
        driver.wait('name=Documents\n')
        frame('finder-responsive-after-exit')
    finally:
        try:
            if process.poll() is None:
                vm.qmp('quit', endpoint=endpoint)
        finally:
            process.wait(timeout=10)
            for path in [output / 'vm/system7.qcow2', output / 'vm/bootrom.bin']:
                path.unlink(missing_ok=True)
            if private.exists():
                shutil.rmtree(private)
    report = {'transport': 'actual Finder and separately packaged applications; physical QMP input',
              'assertion_owner': 'mac-toolbox/tests/application_ui.rs',
              'snapshots': snapshots, 'owned_vm_exited': process.poll() is not None,
              'original_before': before,
              'original_after': {name: packaging.sha(original / name) for name in before},
              'launch': json.loads((output / 'vm/command.json').read_text()),
              'serial_sha256': packaging.sha(output / 'vm/serial.log')}
    (output / 'capture.json').write_text(json.dumps(report, indent=2) + '\n')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', required=True)
    parser.add_argument('--remap-icon', action='store_true')
    parser.add_argument('--windows-headless', action='store_true')
    args = parser.parse_args()
    capture(args.output, args.remap_icon, args.windows_headless)
