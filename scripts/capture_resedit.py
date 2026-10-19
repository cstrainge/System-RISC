#!/usr/bin/env python3
"""Private real-app ResEdit transport; Rust owns round-trip and pixel acceptance."""
import argparse
import hashlib
import json
import os
import subprocess
from pathlib import Path
import time
from capture_system7_applications_ui import Driver
from extract_standard_wdef_assets import resources
from export_resedit_json import exports
import system7_vm as vm


def capture(output):
    output=Path(output).resolve();output.mkdir(parents=True,exist_ok=False)
    baseline=hashlib.sha256((vm.DIST/'system7.qcow2').read_bytes()).hexdigest()
    process,endpoint=vm.launch(output/'vm',writable=True)
    driver=Driver(endpoint,output/'vm/serial.log');frames=[]
    def click(x,y):
        before=driver.log().count('RESEDIT_DRAW_READY\n');driver.click(x,y)
        driver.wait('RESEDIT_DRAW_READY\n',before)
    def frame(name,move_cursor=True):
        if move_cursor:driver.move(16,444)
        time.sleep(.05)
        path=output/(name+'.ppm');vm.qmp('screendump',{'filename':str(path)},endpoint)
        frames.append({'name':name,'frame':path.name,'log':driver.log()})
    def pick(name):
        for char in name.lower():driver.key('spc' if char==' ' else char)
        driver.key('ret')
    def open_dialog():
        n=driver.log().count('STANDARD_FILE_BEGIN ');driver.key('o',('ctrl',));driver.wait('STANDARD_FILE_BEGIN ',n)
    def opened(name):
        deadline=time.monotonic()+20
        while time.monotonic()<deadline:
            text=driver.log()
            matching=[line for line in text.splitlines() if line.startswith('RESEDIT_OPEN ') and line.endswith(' name='+name)]
            if matching and text.rfind('RESEDIT_DRAW_READY\n')>text.rfind(matching[-1]):return
            time.sleep(.01)
        raise TimeoutError('ResEdit did not finish opening '+name)
    def choose_type(package,kind):
        kinds=sorted(set(item[0] for item in resources((vm.DIST/(package+'.rsrc')).read_bytes())))
        index=kinds.index(kind)
        for _ in range(index//16):click(92,48+412-47)
        marker=f'RESEDIT_RESOURCE kind={int.from_bytes(kind,"big")} '
        before=driver.log().count(marker)
        click(50,48+70+(index%16)*16)
        driver.wait(marker,before)
        deadline=time.monotonic()+20
        while driver.log().rfind('RESEDIT_DRAW_READY\n')<driver.log().rfind(marker):
            if time.monotonic()>deadline:raise TimeoutError('resource drawing did not finish')
            time.sleep(.01)
        frame(package+'-'+kind.hex())
        # Return the types list to its first page for the next selection.
        for _ in range(index//16):click(38,48+412-47)
    try:
        driver.click(40,95,double=True);driver.wait(' name=Applications\n')
        driver.application('QuickDraw Test');driver.wait('QUICKDRAW_TEST_READY\n')
        driver.menu(618,93,popup_x=580);driver.wait('PROCESS_ACTIVE app=1\n')
        frame('finder-icons')
        driver.application('ResEdit');driver.wait('RESEDIT_READY\n');frame('picker')
        # Standard CDEF button tracking: release outside must cancel Open.
        driver.move(40,68);driver.button(True);frame('open-held',False)
        driver.move(16,444);frame('open-left',False)
        previous=driver.log().count('RESEDIT_DRAW_READY\n');driver.button(False)
        driver.wait('RESEDIT_DRAW_READY\n',previous);frame('open-cancelled')
        open_dialog();frame('system-open')
        pick('QuickDraw Test');opened('QuickDraw Test');frame('running-app-open')
        for kind in [b'ICN#',b'MENU',b'STR ',b'STR#',b'vers',b'RV64']:
            choose_type('QuickDraw Test',kind)
        n=driver.log().count('STANDARD_FILE_BEGIN ');driver.key('s',('ctrl',));driver.wait('STANDARD_FILE_BEGIN ',n);frame('system-save')
        driver.key('ret');driver.wait('RESEDIT_SAVED ');frame('exported')
        n=driver.log().count('RESEDIT_SAVED ');driver.key('s',('ctrl',));driver.wait('RESEDIT_SAVED ',n);frame('saved-again')
        # Save As shares StandardPutFile; cancel must leave the existing JSON.
        n=driver.log().count('STANDARD_FILE_BEGIN ');driver.menu(42,75);driver.wait('STANDARD_FILE_BEGIN ',n);frame('system-save-as');driver.key('esc')
        # Save As to the same name requires a System-owned replacement confirmation.
        n=driver.log().count('STANDARD_FILE_BEGIN ');driver.menu(42,75);driver.wait('STANDARD_FILE_BEGIN ',n);driver.key('ret');frame('system-replace');driver.key('ret')
        driver.wait('RESEDIT_SAVED ',driver.log().count('RESEDIT_SAVED ')-1)
        # New Folder is another System-owned, original resource-based modal.
        n=driver.log().count('STANDARD_FILE_BEGIN ');driver.menu(42,75);driver.wait('STANDARD_FILE_BEGIN ',n)
        driver.click(440,178);driver.key('a',('ctrl',))
        for key in 'exports':driver.key(key)
        frame('system-new-folder');driver.key('ret');frame('save-in-new-folder')
        n=driver.log().count('RESEDIT_SAVED ');driver.key('ret');driver.wait('RESEDIT_SAVED ',n)
        # Optional serial export must encode the very same JSON as native Save.
        driver.menu(42,107);driver.wait('RESEDIT_JSON_END ')
        candidates=exports(driver.log())
        if not candidates:raise RuntimeError('No complete ResEdit export')
        token,data,_=candidates[-1];(output/'resources.json').write_bytes(data)
        open_dialog();driver.key('up',('ctrl',));pick('System Folder');pick('System');opened('System')
        for kind in [b'ICON',b'cicn']:
            choose_type('System',kind)
        # The running app is still usable after its file was inspected/exported.
        driver.menu(618,111,popup_x=580);driver.wait('PROCESS_ACTIVE app=2\n')
        driver.key('2');driver.wait('QD_DEMO_READY page=2 transfer\n');frame('running-app-still-usable')
        driver.menu(618,129,popup_x=580);driver.wait('PROCESS_ACTIVE app=3\n')
        driver.key('q',('ctrl',));driver.wait('PROCESS_EXIT app=3\n');frame('resedit-closed')
    finally:
        if process.poll() is None:vm.qmp('quit',endpoint=endpoint)
        process.wait(timeout=10)
        # Extract only after QEMU has released and flushed this private disk.
    raw=output/'saved.raw'
    subprocess.run(['qemu-img','convert','-f','qcow2','-O','raw','-S','4k',str(output/'vm/system7.qcow2'),str(raw)],check=True)
    state=output/'hfs-state';state.mkdir()
    env=dict(os.environ,HOME=str(state),TZ='UTC');tools=vm.ROOT/'.toolchains/hfsutils/rootfs/usr/bin'
    def hfs(tool,*args):subprocess.run([str(tools/tool),*map(str,args)],env=env,check=True,capture_output=True)
    hfs('hmount',raw)
    try:
        hfs('hcopy','-r',':Applications:QuickDraw Test.json',output/'native-resources.json')
        hfs('hcopy','-r',':Applications:exports:QuickDraw Test.json',output/'native-subfolder-resources.json')
    finally:hfs('humount')
    for path in [raw,output/'vm/system7.qcow2',output/'vm/bootrom.bin']:path.unlink(missing_ok=True)
    report={'frames':frames,'token':token,'base_before':baseline,'base_after':hashlib.sha256((vm.DIST/'system7.qcow2').read_bytes()).hexdigest(),'assertion_owner':'mac-toolbox/tests/resedit.rs'}
    (output/'capture.json').write_text(json.dumps(report,indent=2)+'\n')

if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__);parser.add_argument('--output',required=True)
    capture(parser.parse_args().output)
