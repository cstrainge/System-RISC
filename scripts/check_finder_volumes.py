#!/usr/bin/env python3
"""Cold-boot two real HFS volumes and compare native Finder icons with System7."""
import hashlib,json,os,re,struct,subprocess,time
from pathlib import Path
from PIL import Image,ImageChops
import system7_vm as vm
import system7_observer as observer
ROOT=Path(__file__).resolve().parents[2]
OUT=ROOT/'results/system7/finder-volumes'

def sha(path):
 h=hashlib.sha256()
 with path.open('rb')as f:
  for data in iter(lambda:f.read(1024*1024),b''):h.update(data)
 return h.hexdigest()
def initialize_alpha_position(image):
 """Patch only our disposable fixture's Alpha cell to saved (0,0).

 Original Finder stores ordinary large-icon cell origins, not icon corners.
 The existing original comparison thus also verifies the +16,+28 conversion.
 """
 with image.open('r+b')as f:
  f.seek(1024);mdb=f.read(512);assert mdb[:2]==b'BD'
  block=struct.unpack_from('>I',mdb,20)[0];start=struct.unpack_from('>H',mdb,28)[0]*512
  extents=[struct.unpack_from('>HH',mdb,p)for p in (150,154,158)]
  catalog=bytearray()
  for base,count in extents:
   f.seek(start+base*block);catalog.extend(f.read(count*block))
  size=struct.unpack_from('>H',catalog,32)[0];node=struct.unpack_from('>I',catalog,24)[0];found=False
  while node:
   offset=node*size;count=struct.unpack_from('>H',catalog,offset+10)[0]
   for i in range(count):
    p=offset+struct.unpack_from('>H',catalog,offset+size-2*(i+1))[0]
    parent=struct.unpack_from('>I',catalog,p+2)[0];name=catalog[p+7:p+7+catalog[p+6]]
    data=p+((catalog[p]+2)&~1)
    if parent==2 and name==b'Alpha'and catalog[data]==1:
     flags=struct.unpack_from('>H',catalog,data+30)[0]
     struct.pack_into('>Hhh',catalog,data+30,flags|0x0100,0,0);found=True
   node=struct.unpack_from('>I',catalog,offset)[0]
  assert found,'fixture Alpha catalog record missing'
  offset=0
  for base,count in extents:
   length=count*block;f.seek(start+base*block);f.write(catalog[offset:offset+length]);offset+=length
def main():
 OUT.mkdir(parents=True,exist_ok=True)
 scratch=ROOT/'working/finder-volumes-check';scratch.mkdir(parents=True,exist_ok=True)
 image=scratch/'second.hfs';state=scratch/'hfs-state';state.mkdir(exist_ok=True)
 with image.open('wb')as f:f.truncate(64*1024*1024)
 tools=Path(os.environ.get('SYSTEM7_HFSUTILS',ROOT/'.toolchains/hfsutils/rootfs/usr/bin'))
 env=dict(os.environ,HOME=str(state),TZ='UTC')
 def hfs(tool,*args):return subprocess.check_output([str(tools/tool),*map(str,args)],env=env,text=True,stderr=subprocess.STDOUT)
 hfs('hformat','-l','Second Volume',image)
 try:
  for name in ['Alpha','Beta Folder','Long folder name for wrapping','System Folder Copy']:hfs('hmkdir',':'+name)
  hfs('hmkdir',':Hidden Desktop Data');hfs('hattrib','+i',':Hidden Desktop Data')
 finally:hfs('humount')
 initialize_alpha_position(image)
 before=sha(image);base=sha(vm.DIST/'system7.qcow2')
 report={'status':'running','method':'Actual ROM/HFS boot with additional read-only HFS device and native Finder using File Manager traps','checks':[],'comparisons':[]}
 process,endpoint=vm.launch(OUT/'vm',extra_disks=[image]);serial=OUT/'vm/serial.log'
 def log():return serial.read_text(errors='replace')
 def state_now():
  return observer.read(endpoint, serial)
 def wait(predicate,name):
  deadline=time.monotonic()+10
  while time.monotonic()<deadline:
   if 'FINDER_ERROR'in log()or'SYSTEM_PANIC'in log():raise AssertionError(log()[-3000:])
   current=state_now()
   if predicate(current):return current
   time.sleep(.02)
  raise AssertionError(name+' timed out: '+log()[-3000:])
 def send(events):vm.qmp('input-send-event',{'events':events},endpoint)
 def move(x,y):
  send([{'type':'abs','data':{'axis':'x','value':(x*32767+638)//639}},{'type':'abs','data':{'axis':'y','value':(y*32767+478)//479}}]);wait(lambda s:s.get('mouse')==f'{x},{y}','motion')
 def click(x,y,handled=False):
  previous=log().count("FINDER_CLICK_HANDLED")
  move(x,y)
  for down in [True,False]:
   before=int(state_now()['input_seq']);send([{'type':'btn','data':{'button':'left','down':down}}]);wait(lambda s:int(s.get('input_seq','0'))>before,'button')
  if handled:wait(lambda s:log().count('FINDER_CLICK_HANDLED')>previous,'Finder handles click')
 def key(code,ctrl=False):
  def k(code,down):return{'type':'key','data':{'key':{'type':'qcode','data':code},'down':down}}
  events=([k('ctrl',True)]if ctrl else[])+[k(code,True),k(code,False)]+([k('ctrl',False)]if ctrl else[])
  before=int(state_now()['input_seq']);send(events);wait(lambda s:int(s.get('input_seq','0'))>=before+len(events),'keyboard')
 def screenshot(name):
  move(16,460);time.sleep(.05);p=OUT/(name+'.png');vm.qmp('screendump',{'filename':str(p),'format':'png'},endpoint);return Image.open(p).convert('RGB')
 def compare(name,native,reference,rect,expected_rect):
  expected_path=ROOT/'results/finder-reference'/(reference+'.png');expected=Image.open(expected_path).convert('RGB').crop(expected_rect);actual=native.crop(rect)
  difference=ImageChops.difference(actual,expected);different=sum(p!=(0,0,0)for p in difference.getdata());difference.save(OUT/(name+'-difference.png'))
  result={'case':name,'compared_pixels':actual.width*actual.height,'different_pixels':different,'masked_pixels':0,'native_rect':list(rect),'original_rect':list(expected_rect),'original':str(expected_path.relative_to(ROOT)),'original_sha256':sha(expected_path)};report['comparisons'].append(result)
  assert different==0,f'{name}: {different} pixel differences'
 try:
  assert 'FINDER_VOLUME id=1 name=System RISC'in log()and'FINDER_VOLUME id=2 name=Second Volume'in log(),log()
  report['checks'].append('Both actual attached HFS volumes enumerated through resident File Manager')
  click(600,104,True);click(600,104,True)
  wait(lambda s:s.get('windows')=='2','second volume opens')
  parent_id=state_now()['front']
  wait(lambda s:f'FINDER_FOLDER_READY window={parent_id}'in log(),'second volume rendered')
  normal=screenshot('native-normal')
  compare('native-normal',normal,'reference-large-icons-normal',(24,82,413,235),(8,66,397,219))
  # Compare composed chrome, including the app's CDEF borders and clipped WDEF
  # grow image. Header rows omit only the variable volume statistics' text band.
  compare('header-above-text',normal,'reference-large-icons-normal',(24,62,428,67),(8,46,412,51))
  compare('header-below-text',normal,'reference-large-icons-normal',(24,75,428,82),(8,59,412,66))
  compare('vertical-scrollbar-and-grow',normal,'reference-large-icons-normal',(413,81,429,251),(397,65,413,235))
  compare('horizontal-scrollbar',normal,'reference-large-icons-normal',(23,235,413,251),(7,219,397,235))
  # Select Alpha, open it, then bring its parent forward to inspect its open icon.
  click(56,104,True)
  selected=screenshot('native-selected')
  compare('native-selected',selected,'alpha-selected-closed',(24,82,413,235),(8,66,397,219))
  key('o',True)
  wait(lambda s:s.get('windows')=='3','Alpha opens')
  child_id=state_now()['front']
  click(170,51)
  wait(lambda s:s.get('front')==parent_id,'parent activates')
  opened=screenshot('native-selected-open')
  compare('native-selected-open',opened,'alpha-selected-open',(24,82,413,235),(8,66,397,219))
  click(250,210,True)
  plain_open=screenshot('native-open')
  compare('native-open',plain_open,'alpha-normal-open',(24,82,413,235),(8,66,397,219))
  # Child's exposed right edge activates it; close and verify the parent icon again.
  click(436,200);wait(lambda s:s.get('front')==child_id,'child activates');closed_before=log().count('FINDER_CLOSE_COMMITTED');key('w',True);wait(lambda s:log().count('FINDER_CLOSE_COMMITTED')>closed_before,'Finder commits close')
  wait(lambda s:s.get('windows')=='2'and s.get('front')==parent_id,'child closes')
  closed=screenshot('native-closed')
  compare('native-closed',closed,'reference-large-icons-normal',(24,82,413,235),(8,66,397,219))
  # Resize through the real WDEF grow hit. The size box and adjoining inactive
  # vertical cap have identical original pixels after translation to new bounds.
  ready=log().count(f'FINDER_FOLDER_READY window={parent_id}')
  move(423,245)
  revision=int(state_now()['input_seq']);send([{'type':'btn','data':{'button':'left','down':True}}]);wait(lambda s:int(s.get('input_seq','0'))>revision,'begin resize')
  move(487,297)
  revision=int(state_now()['input_seq']);send([{'type':'btn','data':{'button':'left','down':False}}]);wait(lambda s:int(s.get('input_seq','0'))>revision,'end resize')
  wait(lambda s:log().count(f'FINDER_FOLDER_READY window={parent_id}')>ready,'Finder redraws resized window')
  resized=screenshot('native-resized')
  compare('resized-scrollbar-and-grow',resized,'reference-large-icons-normal',(477,271,493,303),(397,203,413,235))
  report['checks'].append('Resizing through WDEF updates the application grow clip and CDEF junction to the new window bounds')
  report['checks']+=['Volume double-click opens its real root catalog','Initialized Alpha catalog cell(0,0) renders at icon(16,28), matching original Finder coordinates','Native selected/open/closed folder appearance and long-label layout match original pixels','Folder child opens by Command-O, parent activation exposes open state, child close restores original pixels']
  assert before==sha(image)and base==sha(vm.DIST/'system7.qcow2'),'disk mutated'
  report.update(status='passed',disks_unchanged=True,extra_disk_sha256=before,boot_disk_sha256=base)
 except BaseException as error:
  report.update(status='failed',error=str(error));raise
 finally:
  vm.qmp('quit',endpoint=endpoint);process.wait(timeout=10)
  (ROOT/'results/system7/finder-volume-checks.json').write_text(json.dumps(report,indent=2)+'\n')
 print(f'Native Finder multi-volume checks passed; {len(report["comparisons"])} original comparisons')
if __name__=='__main__':main()
