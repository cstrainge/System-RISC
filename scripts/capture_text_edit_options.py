#!/usr/bin/env python3
"""Physical Options dialog, cancellation, validation and preference transport."""
import argparse
import json
from pathlib import Path
import subprocess
import capture_system7_conformance as packaging
from capture_system7_applications_ui import Driver
from hfs_b_tool import Image
import system7_vm as vm


def capture(output):
    output=Path(output).resolve()
    output.mkdir(parents=True,exist_ok=False)
    hashes={name:packaging.sha(vm.DIST/name) for name in ('system7.qcow2','System.rsrc','bootrom.bin')}
    process=None
    try:
        process,endpoint=vm.launch(output/'vm','headless',writable=True)
        driver=Driver(endpoint,output/'vm/serial.log')
        driver.click(40,95,double=True)
        driver.wait_folder_ready('Applications')
        driver.application('Text Edit')
        driver.wait('TEXT_EDIT_READY\n')
        for key in ['a','b','c','ret','d','e','f']:
            driver.key(key)
        def frame(name): vm.qmp('screendump',{'filename':str(output/(name+'.ppm'))},endpoint)
        frame('default')
        documents=int(driver.state()['windows'])
        driver.menu(190,76,popup_x=220)
        driver.wait('TEXT_EDIT_OPTIONS_READY\n')
        frame('options')
        previous=driver.log().count(f"PROCESS_ACTIVE app={driver.app_id('Finder')}\n")
        driver.menu(618,93,popup_x=580)
        driver.wait(f"PROCESS_ACTIVE app={driver.app_id('Finder')}\n",previous)
        frame('background-options')
        previous=driver.log().count(f"PROCESS_ACTIVE app={driver.app_id('Text Edit')}\n")
        driver.menu(618,111,popup_x=580)
        driver.wait(f"PROCESS_ACTIVE app={driver.app_id('Text Edit')}\n",previous)

        driver.click(110,254)  # Courier
        driver.click(420,363)  # Cancel
        driver.wait('TEXT_EDIT_OPTIONS_CANCELLED\n')
        driver.wait_state(lambda state: int(state['windows']) == documents)
        frame('cancelled')
        driver.menu(190,76,popup_x=220)
        driver.wait('TEXT_EDIT_OPTIONS_READY\n',1)
        driver.click(110,254)  # Courier
        driver.click(266,194)  # 18 point
        driver.click(360,116)  # wrap off
        driver.click(360,144)  # numbers on
        driver.click(360,212)  # column ruler
        driver.click(460,272)
        driver.key('a',('ctrl',))
        driver.key('0')
        driver.click(516,363)
        driver.wait('TEXT_EDIT_OPTIONS_INVALID_LIMIT\n')
        frame('invalid-limit')
        driver.key('ret')
        driver.click(460,272)
        driver.key('a',('ctrl',))
        driver.key('2');driver.key('0')
        driver.click(516,363)
        driver.wait('TEXT_EDIT_OPTIONS_APPLIED\n')
        driver.wait_state(lambda state: int(state['windows']) == documents)
        frame('applied')
        previous=driver.log().count('FINDER_TEXTEDIT_READY\n')
        driver.key('n',('ctrl',))
        driver.wait('FINDER_TEXTEDIT_READY\n',previous)
        frame('new-document')
        vm.qmp('quit',endpoint=endpoint);process.wait(timeout=10)
        raw=output/'volume.hfsb'
        subprocess.run(['qemu-img','convert','-f','qcow2','-O','raw','-S','4k',str(output/'vm/system7.qcow2'),str(raw)],check=True)
        image=Image(raw)
        path='/System Folder/Preferences/Text Edit Preferences'
        metadata=image.stat(path)
        image.extract(path,output/'preferences.json')
        report={'original_before':hashes,'original_after':{name:packaging.sha(vm.DIST/name) for name in hashes},'metadata':metadata}
        (output/'capture.json').write_text(json.dumps(report,indent=2)+'\n')
    finally:
        if process is not None and process.poll() is None:
            vm.qmp('screendump',{'filename':str(output/'failure.ppm')},endpoint)
            vm.qmp('quit',endpoint=endpoint);process.wait(timeout=10)
        for path in (output/'volume.hfsb',output/'vm/system7.qcow2',output/'vm/bootrom.bin'):
            path.unlink(missing_ok=True)

if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output',required=True)
    capture(parser.parse_args().output)
