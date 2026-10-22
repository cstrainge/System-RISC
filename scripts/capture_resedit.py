#!/usr/bin/env python3
"""Private real-app ResEdit transport; Rust owns round-trip and pixel acceptance."""
from hfs_b_tool import Image as NativeImage
import argparse
import hashlib
import json
import os
import re
import subprocess
from pathlib import Path
import time
from capture_system7_applications_ui import Driver
from extract_standard_wdef_assets import resources
from export_resedit_json import exports
import system7_vm as vm
from system7_diagnostics import capture_failure, heap_usage
from capture_system7_event_trace import decode, delta


def capture(output):
    output=Path(output).resolve();output.mkdir(parents=True,exist_ok=False)
    (output/'pid').write_text(str(os.getpid()))
    baseline=hashlib.sha256((vm.DIST/'system7.qcow2').read_bytes()).hexdigest()
    process,endpoint=vm.launch(output/'vm',writable=True)
    driver=Driver(endpoint,output/'vm/serial.log');frames=[];damage_samples=[]
    trace=re.search(r'EVENT_TRACE address=(\d+) bytes=(\d+) version=2',driver.log())
    def snapshot(label):
        if not trace:return None
        address,size=map(int,trace.groups());path=output/(label+'.bin')
        vm.qmp('stop',endpoint=endpoint)
        try:vm.qmp('pmemsave',{'val':address,'size':size,'filename':str(path)},endpoint)
        finally:vm.qmp('cont',endpoint=endpoint)
        return decode(path)
    def click(x,y):
        before=driver.log().count('RESEDIT_DRAW_READY\n');driver.click(x,y)
        driver.wait('RESEDIT_DRAW_READY\n',before)
    def frame(name,move_cursor=True):
        if move_cursor:driver.move(16,444)
        time.sleep(.05)
        path=output/(name+'.ppm');vm.qmp('screendump',{'filename':str(path)},endpoint)
        frames.append({'name':name,'frame':path.name,'log':driver.log(),
                       'heap':heap_usage(driver.endpoint,vm.DIST)})
        (output/'frames.json').write_text(json.dumps(frames,indent=2)+'\n')
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
    def view():
        line=[line for line in driver.log().splitlines() if line.startswith('RESEDIT_VIEW ')][-1]
        return {k:int(v) for k,v in re.findall(r'(\w+)=(\d+)',line)}
    def draw_key(name,modifiers=()):
        before=driver.log().count('RESEDIT_DRAW_READY\n');driver.key(name,modifiers)
        driver.wait('RESEDIT_DRAW_READY\n',before)
    def choose_type(package,kind):
        kinds=sorted(set(item[0] for item in resources((vm.DIST/(package+'.rsrc')).read_bytes())))
        index=kinds.index(kind)
        deadline=time.monotonic()+20
        while index < view()['types_top']:
            if time.monotonic()>deadline:raise TimeoutError('types scrollbar did not reach '+kind.hex())
            click(113,48+32)
        while index >= view()['types_top']+view()['types_rows']:
            if time.monotonic()>deadline:raise TimeoutError('types scrollbar did not reach '+kind.hex())
            click(113,48+412-42)
        marker=f'RESEDIT_RESOURCE kind={int.from_bytes(kind,"big")} '
        before=driver.log().count(marker)
        click(50,48+37+(index-view()['types_top'])*16)
        driver.wait(marker,before)
        frame(package+'-'+kind.hex())
        if kind==b'RV64':
            draw_key('end');frame(package+'-disassembly-end')
            draw_key('home')
            # Exercise actual CDEF0 radio press/leave/release cancellation.
            # Capture the settled frame; Rust checks CDEF press/leave pixels.
            driver.move(398,70);scene=int(driver.state()['scene_rev']);driver.button(True)
            driver.wait_state(lambda state:int(state['scene_rev'])>scene)
            frame(package+'-hex-radio-held',False)
            scene=int(driver.state()['scene_rev'])
            driver.move(16,444)
            driver.wait_state(lambda state:int(state['scene_rev'])>scene)
            frame(package+'-hex-radio-left',False)
            before=driver.log().count('RESEDIT_DRAW_READY\n');driver.button(False)
            driver.wait('RESEDIT_DRAW_READY\n',before);frame(package+'-hex-radio-cancelled')
            click(330,60);frame(package+'-hex')
            # Drag the actual CDEF thumb across its full track. No virtual row
            # positions are injected; Rust checks the resulting viewport.
            driver.move(604,251);driver.button(True);frame(package+'-hex-thumb-held',False)
            driver.move(604,402);before=driver.log().count('RESEDIT_DRAW_READY\n');driver.button(False)
            driver.wait('RESEDIT_DRAW_READY\n',before)
            frame(package+'-hex-end')
            draw_key('home');click(604,418);frame(package+'-hex-down')
            # Timed one-row key repaints include row fill and resident commit.
            before_trace=snapshot(package+'-scroll-before')
            for i in range(8):draw_key('down')
            after_trace=snapshot(package+'-scroll-after')
            if before_trace is not None:damage_samples.append({'package':package,**delta(before_trace,after_trace)})
            frame(package+'-hex-key-scroll')
            click(460,60);frame(package+'-disassembly-restored')
    try:
        driver.click(40,95,double=True);driver.wait(' name=Applications\n')
        driver.application('QuickDraw Test');driver.wait('QUICKDRAW_TEST_READY\n')
        driver.menu(618,93,popup_x=580);driver.wait(f"PROCESS_ACTIVE app={driver.app_id('Finder')}\n")
        frame('finder-icons')
        driver.application('ResEdit');driver.wait('RESEDIT_READY\n');frame('picker')
        open_dialog();frame('system-open')
        pick('QuickDraw Test');opened('QuickDraw Test');frame('running-app-open')
        for kind in [b'ICN#',b'MENU',b'STR ',b'STR#',b'vers',b'RV64',b'NABI',b'RLOC']:
            choose_type('QuickDraw Test',kind)
        n=driver.log().count('STANDARD_FILE_BEGIN ');driver.key('s',('ctrl',));driver.wait('STANDARD_FILE_BEGIN ',n);frame('system-save')
        driver.key('ret');driver.wait('RESEDIT_SAVED ');frame('exported')
        n=driver.log().count('RESEDIT_SAVED ');driver.key('s',('ctrl',));driver.wait('RESEDIT_SAVED ',n);frame('saved-again')
        # Save As shares StandardPutFile; cancel must leave the existing JSON.
        n=driver.log().count('STANDARD_FILE_BEGIN ');driver.menu(42,75);driver.wait('STANDARD_FILE_BEGIN ',n);frame('system-save-as')
        done=driver.log().count('STANDARD_FILE_DONE ');driver.key('esc');driver.wait('STANDARD_FILE_DONE ',done);frame('save-as-cancelled')
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
        for kind in [b'ICON',b'cicn',b'RV64',b'NABI',b'CDEF',b'MDEF',b'WDEF',b'NFNT']:
            choose_type('System',kind)
        click(276,418);frame('System-resources-down')
        def resize(anchor,target,name):
            driver.move(*anchor);driver.button(True);driver.move(*target)
            before=driver.log().count('RESEDIT_DRAW_READY\n');driver.button(False)
            driver.wait('RESEDIT_DRAW_READY\n',before);frame(name)
        resize((618,450),(602,438),'resized')
        resize((602,438),(618,450),'resize-restored')
        # The running app is still usable after its file was inspected/exported.
        active=f"PROCESS_ACTIVE app={driver.app_id('QuickDraw Test')}\n";previous=driver.log().count(active)
        driver.menu(618,111,popup_x=580);driver.wait(active,previous)
        driver.key('2');driver.wait('QD_DEMO_READY page=2 transfer\n');frame('running-app-still-usable')
        active=f"PROCESS_ACTIVE app={driver.app_id('ResEdit')}\n";previous=driver.log().count(active)
        driver.menu(618,129,popup_x=580);driver.wait(active,previous)
        driver.key('q',('ctrl',));driver.wait(f"PROCESS_EXIT app={driver.app_id('ResEdit')}\n");frame('resedit-closed')
    except Exception:
        if process.poll() is None:
            capture_failure(output,driver,vm.DIST)
        raise
    finally:
        if process.poll() is None:vm.qmp('quit',endpoint=endpoint)
        process.wait(timeout=10)
        # Extract only after QEMU has released and flushed this private disk.
    raw=output/'saved.raw'
    subprocess.run(['qemu-img','convert','-f','qcow2','-O','raw','-S','4k',str(output/'vm/system7.qcow2'),str(raw)],check=True)
    fs = NativeImage(raw)
    fs.extract('/Applications/QuickDraw Test.json', output / 'native-resources.json')
    fs.extract('/Applications/exports/QuickDraw Test.json', output / 'native-subfolder-resources.json')
    # Rust reads the captured source catalog through the public SDK and owns
    # disposal of this offline evidence disk, including assertion failures.
    for path in [output/'vm/system7.qcow2',output/'vm/bootrom.bin']:path.unlink(missing_ok=True)
    report={'frames':frames,'damage_samples':damage_samples,'token':token,'base_before':baseline,'base_after':hashlib.sha256((vm.DIST/'system7.qcow2').read_bytes()).hexdigest(),'assertion_owner':'mac-toolbox/tests/resedit.rs'}
    (output/'capture.json').write_text(json.dumps(report,indent=2)+'\n')

if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__);parser.add_argument('--output',required=True)
    capture(parser.parse_args().output)
