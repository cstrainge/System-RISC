#!/usr/bin/env python3
"""Physical Options dialog, cancellation, validation and preference transport."""
import argparse
import json
from pathlib import Path
import subprocess
import re
import time
from capture_system7_event_trace import decode, delta
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
        def wait_frame():
            # Window removal publishes observer state before the next retained
            # composition. Wait for a frame; never force an application repaint.
            before=int(driver.state()['presentations'])
            driver.wait_state(lambda state: int(state['presentations']) > before)
        drag_steps={}
        address,size=map(int,re.search(r'EVENT_TRACE address=(\d+) bytes=(\d+) version=2',driver.log()).groups())
        def trace(name):
            path=output/(name+'.bin');vm.qmp('stop',endpoint=endpoint)
            try:vm.qmp('pmemsave',{'val':address,'size':size,'filename':str(path)},endpoint)
            finally:vm.qmp('cont',endpoint=endpoint)
            return decode(path)
        trace_before=trace('drag-before')
        def drag(name,start,end,hold=False):
            before=len(driver.log())
            count=driver.log().count('TEXTEDIT_DRAG_END\n')
            driver.move(*start);driver.button(True);driver.move(*end)
            if hold:
                driver.wait('FINDER_TEXTEDIT_STATE length=7 selection=0..3\n',driver.log()[:before].count('FINDER_TEXTEDIT_STATE length=7 selection=0..3\n'))
                drag_steps['held-before-release']=driver.log()[before:]
            driver.button(False)
            driver.wait('TEXTEDIT_DRAG_END\n',count)
            drag_steps[name]=driver.log()[before:]
        drag('forward',(129,105),(149,105),True)
        time.sleep(.15)
        drag_trace=delta(trace_before,trace('drag-after'))
        drag('reverse',(149,105),(129,105))
        drag('multiple-lines',(129,105),(150,121))
        drag('outside',(129,105),(620,440))
        driver.click(129,105)
        driver.key_state('shift',True)
        drag('shift-extend',(149,105),(149,105))
        driver.key_state('shift',False)
        driver.key('end',('ctrl',))
        frame('default')
        documents=int(driver.state()['windows'])
        driver.menu(110,76,popup_x=145)
        driver.wait('TEXT_EDIT_OPTIONS_READY\n')
        frame('options')
        previous=driver.log().count(f"PROCESS_ACTIVE app={driver.app_id('Finder')}\n")
        driver.menu(618,93,popup_x=580)
        driver.wait(f"PROCESS_ACTIVE app={driver.app_id('Finder')}\n",previous)
        # Process activation and the old app's selection deactivation are
        # separate queued work. Capture after the retained scene stops changing.
        stable=time.monotonic();stamp=None;deadline=stable+5
        while time.monotonic() < deadline:
            state=driver.state()
            current=(state['scene_rev'],state['compositions'],state['presentations'])
            if current != stamp: stamp=current;stable=time.monotonic()
            if time.monotonic()-stable >= .08: break
            time.sleep(.005)
        frame('background-options')
        previous=driver.log().count(f"PROCESS_ACTIVE app={driver.app_id('Text Edit')}\n")
        driver.menu(618,111,popup_x=580)
        driver.wait(f"PROCESS_ACTIVE app={driver.app_id('Text Edit')}\n",previous)

        driver.click(110,254)  # Courier
        driver.click(420,363)  # Cancel
        driver.wait('TEXT_EDIT_OPTIONS_CANCELLED\n')
        driver.wait_state(lambda state: int(state['windows']) == documents)
        wait_frame()
        frame('cancelled')
        driver.menu(110,76,popup_x=145)
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
        driver.key('ret')  # Default OK button
        driver.wait('TEXT_EDIT_OPTIONS_APPLIED\n')
        driver.wait_state(lambda state: int(state['windows']) == documents)
        wait_frame()
        frame('applied')
        first_window=driver.state()['front']
        previous=driver.log().count('FINDER_TEXTEDIT_READY\n')
        driver.key('n',('ctrl',))
        driver.wait('FINDER_TEXTEDIT_READY\n',previous)
        driver.wait_state(lambda state: state['front'] != first_window)
        frame('new-document')
        second_window=driver.state()['front']
        driver.menu(175,28,popup_x=200)
        first_selected=driver.wait_state(lambda state: state['front'] == first_window)
        frame('windows-first')
        driver.menu(175,44,popup_x=200)
        second_selected=driver.wait_state(lambda state: state['front'] == second_window)
        frame('windows-second')
        driver.menu(175,28,popup_x=200)
        driver.wait_state(lambda state: state['front'] == first_window)
        driver.discard_text_edit(quit=False)
        after_close=driver.wait_state(lambda state: int(state['windows']) == documents)
        driver.menu(175,28,popup_x=200)
        remaining=driver.wait_state(lambda state: state['front'] == second_window)
        driver.key('w',('ctrl',))  # Closing the last clean document clears Windows too.
        last_closed=driver.wait_state(lambda state: int(state['windows']) == documents - 1)
        driver.menu(175,28,popup_x=200)  # Empty Windows must not keep a stale command.
        empty_menu=driver.state()
        previous=driver.log().count('FINDER_TEXTEDIT_READY\n')
        driver.key('n',('ctrl',))
        driver.wait('FINDER_TEXTEDIT_READY\n',previous)
        reopened=driver.wait_state(lambda state: int(state['windows']) == documents)
        vm.qmp('quit',endpoint=endpoint);process.wait(timeout=10)
        raw=output/'volume.hfsb'
        subprocess.run(['qemu-img','convert','-f','qcow2','-O','raw','-S','4k',str(output/'vm/system7.qcow2'),str(raw)],check=True)
        image=Image(raw)
        path='/System Folder/Preferences/Text Edit Preferences'
        metadata=image.stat(path)
        image.extract(path,output/'preferences.json')
        report={'original_before':hashes,'original_after':{name:packaging.sha(vm.DIST/name) for name in hashes},'metadata':metadata,'drag_steps':drag_steps,'drag_trace':drag_trace,'editor_app':str(driver.app_id('Text Edit')),'window_menu':{'first':first_window,'second':second_window,'first_selected':first_selected['front'],'second_selected':second_selected['front'],'after_close':after_close['windows'],'remaining_selected':remaining['front'],'last_closed_windows':last_closed['windows'],'empty_menu_windows':empty_menu['windows'],'reopened_windows':reopened['windows']}}
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
