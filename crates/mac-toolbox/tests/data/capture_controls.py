#!/usr/bin/env python3
"""Package independently captured original CDEF pixels/hits; never run native code."""
from pathlib import Path
import hashlib,json,struct
from PIL import Image
ROOT=Path(__file__).resolve().parents[5]
OUT=Path(__file__).resolve().parent
REFERENCE=ROOT/'results/control-tracking-reference'
BOUNDS=[(120,120,240,144),(120,165,280,183),(120,200,280,218),(500,120,516,340),(120,350,460,366),(310,120,430,144),(310,165,470,183),(310,200,470,218)]
CASES=[];data=bytearray(b'CDF0'+struct.pack('>H',54))
for page in range(9):
 image=Image.open(REFERENCE/f'static-{page}.png').convert('RGB')
 for index,procedure in [(0,0),(1,1),(2,2),(5,8),(6,9),(7,10)]:
  bounds=BOUNDS[index];crop=image.crop(bounds);pixels=list(crop.getdata());palette=sorted(set(pixels));encoded=bytearray();last=-1;run=0
  for pixel in pixels+[None]:
   code=palette.index(pixel) if pixel is not None else -1
   if code!=last or run==65535:
    if run:encoded+=struct.pack('>HB',run,last)
    run=0;last=code
   run+=1
  hilite=0 if page<3 else (10 if procedure&7==0 else 11) if page<6 else 255
  data+=struct.pack('>HHHhhhHHI',procedure,crop.width,crop.height,page%3,hilite,3 if procedure>=8 else 0,9 if procedure>=8 else 12,len(palette),len(encoded))
  for rgb in palette:data+=bytes(rgb)
  data+=encoded
  CASES.append({'page':page,'control':index,'procedure':procedure,'bounds':bounds,'rgb_sha256':hashlib.sha256(crop.tobytes()).hexdigest(),'source_sha256':hashlib.sha256((REFERENCE/f'static-{page}.png').read_bytes()).hexdigest()})
(OUT/'original-control-pixels.bin').write_bytes(data)
hits=json.loads((REFERENCE/'hits.json').read_text());blob=bytearray(b'CDHT'+struct.pack('>H',len(hits)))
for row in hits:blob+=struct.pack('>BBhhh',row['page'],row['control'],row['x'],row['y'],row['part'])
(OUT/'original-control-hits.bin').write_bytes(blob)
manifest={'scope':'Original System7.6.1 CDEF0 six variations x nine states, plus original TestControl trace records. Glyph/title frames fully compared; no masks.','cases':CASES,'pixel_count':sum((r['bounds'][2]-r['bounds'][0])*(r['bounds'][3]-r['bounds'][1]) for r in CASES),'hit_records':len(hits),'distinct_hit_records':len({tuple(r[k] for k in ['page','control','x','y','part']) for r in hits}),'pixel_blob_sha256':hashlib.sha256(data).hexdigest(),'hit_blob_sha256':hashlib.sha256(blob).hexdigest(),'resource_sha256':'b647566a3fe540ae1e2477e6c3ced3d3a33cfffc131fcd8fbfa22c5774cc0dde','fixture_source':'fixtures/control-tracking-reference/ControlTrackingProbe.cc','fixture_source_sha256':hashlib.sha256((ROOT/'fixtures/control-tracking-reference/ControlTrackingProbe.cc').read_bytes()).hexdigest()}
(OUT/'original-controls.json').write_text(json.dumps(manifest,indent=2)+'\n')
# Existing independent original scrollbar probe: 15/16px, both orientations.
data=bytearray(b'CDF1'+struct.pack('>H',32));scroll_cases=[]
for page in range(8):
 image_path=ROOT/f'results/finder-reference/cdef-page-{page}.png';image=Image.open(image_path).convert('RGB')
 for bounds in [(380,100,395,280),(400,100,416,280),(100,280,380,295),(100,305,380,321)]:
  crop=image.crop(bounds);palette=sorted(set(crop.getdata()));encoded=bytearray();last=-1;run=0
  for pixel in list(crop.getdata())+[None]:
   code=palette.index(pixel) if pixel is not None else -1
   if code!=last or run==65535:
    if run:encoded+=struct.pack('>HB',run,last)
    run=0;last=code
   run+=1
  data+=struct.pack('>HHHHhhhHHI',crop.width,crop.height,bounds[0]%8,bounds[1]%8,50 if page==1 else 100 if page==2 else 0,0 if page==4 else 100,20 if page==5 else 21 if page==6 else 129 if page==7 else 0,0 if page==3 else 1,len(palette),len(encoded))
  for rgb in palette:data+=bytes(rgb)
  data+=encoded
  scroll_cases.append({'page':page,'bounds':bounds,'source_sha256':hashlib.sha256(image_path.read_bytes()).hexdigest(),'rgb_sha256':hashlib.sha256(crop.tobytes()).hexdigest()})
(OUT/'original-scrollbar-pixels.bin').write_bytes(data)
manifest['original_provenance']=['provenance/control-tracking-reference.json','provenance/control-tracking-supplement.json']
manifest['original_analysis']='analysis/control-tracking-reference.md'
manifest['scrollbars']={'case_count':32,'pixel_count':114080,'cases':scroll_cases,'blob_sha256':hashlib.sha256(data).hexdigest()}
(OUT/'original-controls.json').write_text(json.dumps(manifest,indent=2)+'\n')
