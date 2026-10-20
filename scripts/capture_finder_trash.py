#!/usr/bin/env python3
"""Capture Finder menus, durable Trash and modal input in a private writable VM.

Only transport synchronization lives here. Aggregate Rust tests judge the evidence.
"""
import argparse
import hashlib
import json
from pathlib import Path
import time
import system7_vm as vm
import system7_observer as observer


def capture(output):
    output=Path(output).resolve();output.mkdir(parents=True,exist_ok=True)
    baseline=hashlib.sha256((vm.DIST/'system7.qcow2').read_bytes()).hexdigest()
    state_dir=output/'vm'
    process,endpoint=vm.launch(state_dir,writable=True,preserve=True)
    serial=state_dir/'serial.log'
    observations=[]
    def state():
        return observer.read(endpoint, serial)
    def wait(predicate):
        deadline=time.monotonic()+10
        while time.monotonic()<deadline:
            if predicate(state()):return
            if process.poll() is not None:raise RuntimeError('Guest exited during transport')
            time.sleep(.02)
        raise TimeoutError('Input acknowledgement timed out: '+str(state()))
    def send(events):vm.qmp('input-send-event',{'events':events},endpoint)
    def move(x,y):
        send([{'type':'abs','data':{'axis':'x','value':(x*32767+638)//639}},
              {'type':'abs','data':{'axis':'y','value':(y*32767+478)//479}}])
        wait(lambda s:s.get('mouse')==f'{x},{y}')
    def button(down):
        before=int(state()['input_seq']);send([{'type':'btn','data':{'button':'left','down':down}}])
        wait(lambda s:int(s.get('input_seq','0'))>before)
    def click(x,y):move(x,y);button(True);button(False);time.sleep(.12)
    def key(code,ctrl=False):
        def event(k,d):return {'type':'key','data':{'key':{'type':'qcode','data':k},'down':d}}
        sequence=([event('ctrl',True)] if ctrl else [])+[event(code,True),event(code,False)]+([event('ctrl',False)] if ctrl else [])
        before=int(state()['input_seq']);send(sequence)
        wait(lambda s:int(s.get('input_seq','0'))>=before+len(sequence));time.sleep(.25)
    def snapshot(name):
        time.sleep(.2);path=output/(name+'.ppm');vm.qmp('screendump',{'filename':str(path)},endpoint)
        observations.append({'name':name,'frame':path.name,'state':state(),'log':serial.read_text(errors='replace')})
    def empty_menu():
        # Native bar follows original positions: Apple10,File34,Edit70,View108,Label153,Special201.
        move(226,10);button(True);move(250,44);button(False);time.sleep(.2)
    try:
        move(16,444);snapshot('baseline')
        key('w',True);snapshot('desktop-no-windows')
        key('n',True);wait(lambda _: 'FINDER_NEW_FOLDER ' in serial.read_text(errors='replace'));move(16,444);snapshot('folder-created-menu')
        move(45,10);button(True);move(80,28);snapshot('file-menu')
        move(16,444);button(False)
        # New folders are selected by Finder; drag the fourth root item to Trash.
        move(296,90);button(True);move(592,436);snapshot('trash-drop-held');button(False)
        wait(lambda _: 'FINDER_TRASHED ' in serial.read_text(errors='replace'))
        move(16,444);snapshot('trashed')
        click(592,436);click(592,436);snapshot('trash-open')
        # The single Trash item is the first icon in its second cascaded window.
        click(56,106);key('y',True)
        wait(lambda _: 'FINDER_PUT_AWAY ' in serial.read_text(errors='replace'))
        snapshot('put-away')
        key('w',True)
        # Root selection was cleared by Put Away; select and exercise Ctrl-Backspace.
        click(296,90);key('backspace',True)
        wait(lambda _: serial.read_text(errors='replace').count('FINDER_TRASHED ') >= 2)
        snapshot('trashed-again')
        empty_menu();move(16,444);snapshot('empty-confirm')
        # Modal background/menu clicks must not activate anything or create a folder.
        click(40,90);key('n',True);snapshot('modal-blocked')
        key('esc');snapshot('empty-cancelled')
        click(600,44);move(16,444);snapshot('desktop-focused-before')
        empty_menu();move(16,444);snapshot('desktop-focused-confirm')
        key('esc');snapshot('desktop-focus-restored')
        snapshot('before-reboot')
        first_log=serial.read_text(errors='replace');(output/'first-boot.log').write_text(first_log)
        vm.qmp('quit',endpoint=endpoint);process.wait(timeout=10)
        process,endpoint=vm.launch(state_dir,writable=True,preserve=True)
        serial=state_dir/'serial.log';move(16,444);snapshot('rebooted-full-trash')
        empty_menu();move(16,444);snapshot('reboot-confirm')
        # Exercise real CDEF press/leave/re-enter/release on original default OK bounds.
        move(464,175);button(True);snapshot('ok-held')
        move(100,300);snapshot('ok-left');move(464,175);snapshot('ok-reentered')
        emptied_start=time.monotonic();button(False)
        # UI dismissal precedes the durable catalog transaction and redraw.
        # Do not terminate/reboot this guest until Finder acknowledges completion.
        wait(lambda _: any(line.startswith('FINDER_TRASH_EMPTIED removed=')
                           for line in serial.read_text(errors='replace').split('\n')[:-1]))
        empty_completion_seconds=time.monotonic()-emptied_start
        move(16,444);snapshot('emptied')
        final_log=serial.read_text(errors='replace');(output/'second-boot.log').write_text(final_log)
        vm.qmp('quit',endpoint=endpoint);process.wait(timeout=10)
        process,endpoint=vm.launch(state_dir,writable=True,preserve=True)
        serial=state_dir/'serial.log';move(16,444);snapshot('rebooted-empty-trash')
        (output/'third-boot.log').write_text(serial.read_text(errors='replace'))
    finally:
        if process.poll() is None:
            vm.qmp('quit',endpoint=endpoint);process.wait(timeout=10)
    import subprocess
    raw=output/'persisted.hfs'
    subprocess.run(['qemu-img','convert','-f','qcow2','-O','raw','-S','4k',str(state_dir/'system7.qcow2'),str(raw)],check=True)
    report={'observations':observations,'base_before':baseline,'base_after':hashlib.sha256((vm.DIST/'system7.qcow2').read_bytes()).hexdigest(),'private_disk':str(raw),'empty_completion_seconds':empty_completion_seconds}
    (output/'capture.json').write_text(json.dumps(report,indent=2)+'\n')

if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__);parser.add_argument('--output',required=True)
    capture(parser.parse_args().output)
