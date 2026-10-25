#!/usr/bin/env python3
"""Drive a private public-SDK close fixture; Rust owns every acceptance decision."""
from system7_platform import tablet_axis
import argparse,hashlib,json,re,shutil,time
from pathlib import Path
import system7_vm as vm
import system7_observer as observer
from capture_system7_window_tracking import private_probe_disk

def capture(output):
    output=Path(output).resolve();output.mkdir(parents=True,exist_ok=True)
    base=vm.DIST/'system7.qcow2';before=hashlib.sha256(base.read_bytes()).hexdigest()
    private=private_probe_disk(output,deferred=True);vm.DIST=private
    process,endpoint=vm.launch(output/'vm');serial=output/'vm/serial.log'
    def text():return serial.read_text(errors='replace')
    # The UART file can end halfway through a record while the guest writes.
    def lines():return text().split('\n')[:-1]
    def wait(predicate):
        deadline=time.monotonic()+12
        while time.monotonic()<deadline:
            if predicate():return
            if process.poll() is not None:raise RuntimeError('Guest exited:\n'+text()[-3000:])
            time.sleep(.01)
        raise TimeoutError('Deferred close transport timed out:\n'+text()[-3000:])
    def send(events):vm.qmp('input-send-event',{'events':events},endpoint)
    def move(x,y):
        send([{'type':'abs','data':{'axis':'x','value':tablet_axis(x, 'x')}},
              {'type':'abs','data':{'axis':'y','value':tablet_axis(y, 'y')}}])
        wait(lambda:observer.read(endpoint,serial)['mouse']==f'{x},{y}')
    def button(down):
        previous=int(observer.read(endpoint,serial)['input_seq'])
        send([{'type':'btn','data':{'button':'left','down':down}}])
        wait(lambda:int(observer.read(endpoint,serial)['input_seq'])>previous)
    def key(code):
        send([{'type':'key','data':{'key':{'type':'qcode','data':code},'down':True}},
              {'type':'key','data':{'key':{'type':'qcode','data':code},'down':False}}])
    observations=[]
    def snapshot(name):
        seq=len(observations)+1;key('p')
        wait(lambda:any(line.startswith(f'DEFERRED_STATE query={seq} ')for line in lines()))
        line=next(line for line in lines()if line.startswith(f'DEFERRED_STATE query={seq} '))
        state={name:int(value)for name,value in (part.split('=',1)for part in line.split()[1:])}
        path=output/(name+'.ppm');vm.qmp('screendump',{'filename':str(path)},endpoint)
        observations.append({'name':name,'state':state,'frame':path.name})
    try:
        wait(lambda:re.search(r'DEFERRED_PAINT id=\d+ ', text()));move(16,444);snapshot('baseline')
        move(113,91);button(True);move(16,444);button(False);snapshot('cancelled')
        move(113,91);button(True);button(False);move(16,444);snapshot('accepted')
        snapshot('accepted-again')
        key('c');wait(lambda:re.search(r'AUTOMATIC_READY id=(\d+)\n', text()))
        automatic_id=re.search(r'AUTOMATIC_READY id=(\d+)\n', text()).group(1)
        wait(lambda:f'DEFERRED_PAINT id={automatic_id} ' in text())
        snapshot('explicit-close')
        move(113,91);button(True);button(False);move(16,444);snapshot('automatic-close')
        report={'observations':observations,'disk_sha256_before':before,'disk_sha256_after':hashlib.sha256(base.read_bytes()).hexdigest(),
                'requests':[line for line in lines()if line.startswith('DEFERRED_REQUEST ')],
                'disposals':[line for line in lines()if line.startswith('DEFERRED_DISPOSE ')],
                'automatic_notices':[line for line in lines()if line.startswith('AUTOMATIC_NOTICE ')],
                'launch':json.loads((output/'vm/command.json').read_text())}
        (output/'capture.json').write_text(json.dumps(report,indent=2)+'\n')
    finally:
        try:vm.qmp('quit',endpoint=endpoint)
        finally:process.wait(timeout=10)
        # Preserve logs/frames and remove obsolete test boot disks immediately.
        shutil.rmtree(private)
        for name in ['system7.qcow2','bootrom.bin']:
            (output/'vm'/name).unlink(missing_ok=True)

if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__);parser.add_argument('--output',required=True)
    capture(parser.parse_args().output)
