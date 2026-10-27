#!/usr/bin/env python3
"""Physical Finder/Text Edit document transport; Rust owns acceptance assertions."""
import argparse
import json
import re
import subprocess
from pathlib import Path

import capture_system7_conformance as packaging
import system7_vm as vm
from capture_system7_applications_ui import Driver
from hfs_b_tool import Image


def capture(output):
    output = Path(output).resolve()
    output.mkdir(parents=True, exist_ok=False)
    base = vm.DIST / 'system7.qcow2'
    before = packaging.sha(base)
    process = None
    endpoint = None
    raw = output / 'volume.hfsb'
    try:
        process, endpoint = vm.launch(output / 'vm', 'headless', writable=True)
        driver = Driver(endpoint, output / 'vm/serial.log')

        def frame(name):
            vm.qmp('screendump', {'filename': str(output / (name + '.ppm'))}, endpoint)

        def quit_editor():
            previous = driver.log().count('PROCESS_EXIT app=')
            driver.key('q', ('ctrl',))
            driver.wait('PROCESS_EXIT app=', previous)

        driver.click(40, 95, double=True)
        driver.wait_folder_ready('Applications')
        driver.application('Text Edit')
        driver.wait('TEXT_EDIT_READY\n')
        driver.key('h')
        driver.key('i')
        driver.wait('FINDER_TEXTEDIT_STATE length=2 selection=2..2\n')
        driver.menu(110, 76, popup_x=145)  # View / Options
        driver.wait('TEXT_EDIT_OPTIONS_READY\n')
        driver.click(110, 156)  # Geneva in the font list
        driver.click(266, 194)  # 18 points
        driver.key('ret')  # Default OK
        driver.wait('TEXT_EDIT_OPTIONS_APPLIED\n')
        frame('chosen-font')
        driver.key('s', ('ctrl',))
        driver.wait('STANDARD_FILE_BEGIN ')
        frame('save-dialog')
        for key in ['p', 'r', 'e', 'v', 'i', 'e', 'w']:
            driver.key(key)
        driver.key('ret')
        driver.wait('TEXT_EDIT_DOCUMENT_SAVED\n')
        quit_editor()

        # Fresh Finder windows, using the ordinary resource-backed icon layout.
        driver.click(200, 36)
        # Fresh Finder layout reserves extra cells for the wide folder label.
        driver.click(232, 95, double=True)
        driver.wait_folder_ready('System Folder')
        driver.click(390, 126, double=True)
        driver.wait_folder_ready('Preferences')
        frame('preferences-folder')
        driver.click(88, 138, double=True)  # Extensionless File Types
        driver.wait('TEXT_EDIT_DOCUMENT_OPEN\n')
        frame('json-document')
        # Save a harmless whitespace edit through the normal document path:
        # this must preserve JSON type rather than changing it to TEXT.
        driver.key('spc')
        previous = driver.log().count('TEXT_EDIT_DOCUMENT_SAVED\n')
        driver.key('s', ('ctrl',))
        driver.wait('TEXT_EDIT_DOCUMENT_SAVED\n', previous)
        quit_editor()
        previous = driver.log().count('TEXT_EDIT_DOCUMENT_OPEN\n')
        driver.click(152, 138, double=True)  # Extensionless Text Edit Preferences
        driver.wait('TEXT_EDIT_DOCUMENT_OPEN\n', previous)
        frame('font-preferences-document')
        quit_editor()
        vm.qmp('quit', endpoint=endpoint)
        process.wait(timeout=10)
        subprocess.run(['qemu-img', 'convert', '-f', 'qcow2', '-O', 'raw', '-S', '4k',
                        str(output / 'vm/system7.qcow2'), str(raw)], check=True)
        image = Image(raw)
        report = {}
        for name, path in [('document', '/Documents/preview'),
                           ('preferences', '/System Folder/Preferences/Text Edit Preferences'),
                           ('registry', '/System Folder/Preferences/File Types')]:
            report[name] = image.stat(path)
            image.extract(path, output / (name + '.bytes'))
        report['original_before'] = before
        report['original_after'] = packaging.sha(base)
        report['assertion_owner'] = 'mac-toolbox/tests/text_edit_documents.rs'
        (output / 'capture.json').write_text(json.dumps(report, indent=2) + '\n')
    except BaseException:
        if process is not None and process.poll() is None:
            registers = vm.qmp('human-monitor-command', {'command-line': 'info registers'}, endpoint)
            (output / 'registers.txt').write_text(registers)
            match = re.search(r'x2/sp\s+([0-9a-f]+)', registers)
            if match:
                address, length = int(match[1], 16), 32768
                # A failure on the guarded System stack must not make evidence
                # collection read through its upper guard and hide the cause.
                stack = re.search(r'SYSTEM_STACK_READY base=(0x[0-9a-f]+) bytes=(\d+)', driver.log())
                if stack:
                    base, size = int(stack[1], 16), int(stack[2])
                    if base <= address < base + size:
                        length = min(length, base + size - address)
                try:
                    vm.qmp('memsave', {'cpu-index': 0, 'val': address, 'size': length,
                                      'filename': str(output / 'stack.bin')}, endpoint)
                except RuntimeError as evidence_error:
                    (output / 'stack-capture-error.txt').write_text(str(evidence_error) + '\n')
            vm.qmp('screendump', {'filename': str(output / 'failure.ppm')}, endpoint)
        raise
    finally:
        try:
            if process is not None and process.poll() is None:
                vm.qmp('quit', endpoint=endpoint)
                process.wait(timeout=10)
        finally:
            for path in [output / 'vm/system7.qcow2', output / 'vm/bootrom.bin', raw]:
                path.unlink(missing_ok=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', required=True)
    args = parser.parse_args()
    capture(args.output)
