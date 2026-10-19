#!/usr/bin/env python3
"""Independent PNG fixtures: Python struct/zlib encoder, explicit sample oracle."""
import hashlib,json,struct,zlib,subprocess
from pathlib import Path
ROOT=Path(__file__).resolve().parents[2];OUT=ROOT/'System7/crates/mac-toolbox/tests/data/png'
SIG=b'\x89PNG\r\n\x1a\n'
def chunk(kind,data):return struct.pack('>I',len(data))+kind+data+struct.pack('>I',zlib.crc32(kind+data))
def png(w,h,depth,color,raw,extra=b'',after=b'',interlace=0):return SIG+chunk(b'IHDR',struct.pack('>IIBBBBB',w,h,depth,color,0,0,interlace))+extra+chunk(b'IDAT',zlib.compress(raw))+after+chunk(b'IEND',b'')
def packed(values,depth):
 if depth==8:return bytes(values)
 if depth==16:return b''.join(struct.pack('>H',x)for x in values)
 out=bytearray((len(values)*depth+7)//8)
 for i,v in enumerate(values):out[i*depth//8]|=v<<(8-depth-i*depth%8)
 return bytes(out)
def scale(v,d):return v>>8 if d==16 else v*255//((1<<d)-1)
def paeth(a,b,c):
 p=a+b-c;ds=[abs(p-a),abs(p-b),abs(p-c)];return[a,b,c][ds.index(min(ds))]
def filtered(rows,bpp,filters):
 raw=bytearray();prev=bytes(len(rows[0]))
 for row,f in zip(rows,filters):
  raw.append(f)
  for i,v in enumerate(row):
   a=row[i-bpp]if i>=bpp else 0;b=prev[i];c=prev[i-bpp]if i>=bpp else 0
   p=[0,a,b,(a+b)//2,paeth(a,b,c)][f];raw.append((v-p)&255)
  prev=row
 return bytes(raw)
def main():
 OUT.mkdir(parents=True,exist_ok=True);oracle=[];bad=[]
 def write(name,data,expected=None,**meta):
  p=OUT/(name+'.png');p.write_bytes(data);row={'name':name,'sha256':hashlib.sha256(data).hexdigest(),'length':len(data),**meta}
  if expected is not None:row['rgba']=expected;oracle.append(row)
  else:bad.append(row)
 for color,label,depths in[(0,'gray',[1,2,4,8,16]),(2,'rgb',[8,16]),(3,'indexed',[1,2,4,8]),(4,'gray-alpha',[8,16]),(6,'rgba',[8,16])]:
  for depth in depths:
   maximum=(1<<depth)-1;channels={0:1,2:3,3:1,4:2,6:4}[color];palette=[(255,32,0),(0,200,255),(70,80,90),(250,250,250)][:min(4,1<<depth)];samples=[];expected=[]
   for i in range(6):
    if color==3:
     index=i%len(palette);values=[index];rgba=[*palette[index],[0,128,255,64][index]]
    else:
     values=[([0,maximum,maximum//2,maximum//3,1,maximum-1][(i+j)%6])for j in range(channels)]
     if color==0:rgba=[scale(values[0],depth)]*3+[255]
     elif color==2:rgba=[scale(v,depth)for v in values]+[255]
     elif color==4:rgba=[scale(values[0],depth)]*3+[scale(values[1],depth)]
     else:rgba=[scale(v,depth)for v in values]
    samples.append(values);expected+=rgba
   rows=[packed(sum(samples[y*3:(y+1)*3],[]),depth)for y in range(2)]
   extra=(chunk(b'PLTE',bytes(sum((list(p)for p in palette),[])))+chunk(b'tRNS',bytes([0,128,255,64][:len(palette)])))if color==3 else b''
   write(f'{label}{depth}',png(3,2,depth,color,filtered(rows,max(1,channels*depth//8),[0,1]),extra),expected,width=3,height=2,depth=depth,color=color,alpha=color in(3,4,6))
 for color,label in[(0,'gray'),(2,'rgb')]:
  for depth in[8,16]:
   target=[128 if depth==8 else 32768]if color==0 else[128 if depth==8 else 32768,31 if depth==8 else 0x4567,235 if depth==8 else 0xabcd]
   other=target.copy();other[0]+=1 if depth==8 else 255;samples=[target,other,target,other,other,target];channels=len(target);raw=b''.join(b'\0'+packed(sum(samples[y*3:(y+1)*3],[]),depth)for y in range(2));expected=[]
   for v in samples:expected+=([scale(v[0],depth)]*3 if color==0 else[scale(x,depth)for x in v])+[0 if v==target else 255]
   write(f'{label}{depth}-trns',png(3,2,depth,color,raw,chunk(b'tRNS',b''.join(struct.pack('>H',v)for v in target))),expected,width=3,height=2,depth=depth,color=color,alpha=True)
 values=[(255,0,0,255),(0,255,0,128),(0,0,255,0),(200,100,50,64),(15,30,45,192),(255,255,255,1)];raw=b''.join(b'\0'+bytes(sum((list(x)for x in values[y*3:(y+1)*3]),[]))for y in range(2));rgba=sum((list(v)for v in values),[])
 write('alpha-composite',png(3,2,8,6,raw),rgba,width=3,height=2,depth=8,color=6,alpha=True)
 extra=chunk(b'iTXt',b'invalid\0\x00\x00\0\0\xff\xfe')+chunk(b'iCCP',b'ignored\0\0'+zlib.compress(b'x'*(2*1024*1024)))
 write('ignored-metadata',png(3,2,8,6,raw,extra),rgba,width=3,height=2,depth=8,color=6,alpha=True)
 rows=[bytes((x*47+y*31+c*61)%256 for x in range(5)for c in range(4))for y in range(5)];write('all-filters',png(5,5,8,6,filtered(rows,4,range(5))),list(b''.join(rows)),width=5,height=5,depth=8,color=6,alpha=True)
 # Independently encoded Adam7 data for a3x2RGBA image.
 passes=[]
 for sx,sy,dx,dy in[(0,0,8,8),(4,0,8,8),(0,4,4,8),(2,0,4,4),(0,2,2,4),(1,0,2,2),(0,1,1,2)]:
  for y in range(sy,2,dy):
   row=[values[y*3+x]for x in range(sx,3,dx)]
   if row:passes.append(b'\0'+bytes(sum((list(p)for p in row),[])))
 write('adam7',png(3,2,8,6,b''.join(passes),interlace=1),kind='unsupported')
 animation=chunk(b'acTL',struct.pack('>II',1,0))+chunk(b'fcTL',struct.pack('>IIIIIHHBB',0,3,2,0,0,1,10,0,0));write('apng',png(3,2,8,6,raw,animation),kind='unsupported')
 write('short-inflate',png(3,2,8,6,raw[:-1]),kind='bad-compressed')
 write('long-inflate',png(3,2,8,6,raw+b'\0'),kind='bad-compressed')
 write('bad-filter',png(3,2,8,6,b'\x05'+raw[1:]),kind='bad-compressed')
 write('inflate-bomb',png(3,2,8,6,b'\0'*(1024*1024)),kind='bad-compressed')
 write('palette-index-outside',png(3,2,2,3,b'\0\xff\0\xff',chunk(b'PLTE',b'\0\0\0\xff\xff\xff')),kind='bad-palette')
 compressed=zlib.compress(raw);write('bad-adler',SIG+chunk(b'IHDR',struct.pack('>IIBBBBB',3,2,8,6,0,0,0))+chunk(b'IDAT',compressed[:-1]+bytes([compressed[-1]^1]))+chunk(b'IEND',b''),kind='bad-compressed')
 # Many legitimate files split compressed payload across arbitrarily smallIDATs.
 write('split-idat',SIG+chunk(b'IHDR',struct.pack('>IIBBBBB',3,2,8,6,0,0,0))+b''.join(chunk(b'IDAT',compressed[i:i+1])for i in range(len(compressed)))+chunk(b'IEND',b''),rgba,width=3,height=2,depth=8,color=6,alpha=True)
 w,h=96,64;demo=bytearray()
 for y in range(h):
  demo.append(0)
  for x in range(w):demo+=bytes((x*255//(w-1),y*255//(h-1),255-x*255//(w-1),min(255,(x+y)*3)))
 write('demo-rgba',png(w,h,8,6,bytes(demo)),kind='demo')
 (OUT/'oracle.json').write_text(json.dumps({'generator':'System7/scripts/generate_png_fixtures.py','encoding':'Independent Python struct/zlib PNG encoder; explicit samples→RGBA oracle; no production PNG implementation used.','valid':oracle,'other':bad},indent=2)+'\n')
 rust=OUT.parent.parent/'conformance/png_oracle.rs'
 source=['// Generated from independently encoded source samples by scripts/generate_png_fixtures.py.','pub const CASES: &[Case] = &[']
 for row in oracle:
  source.append('Case { name: '+json.dumps(row['name'])+', data: include_bytes!("../data/png/'+row['name']+'.png"), width: '+str(row['width'])+', height: '+str(row['height'])+', depth: '+str(row['depth'])+', color: '+str(row['color'])+', alpha: '+str(row['alpha']).lower()+', rgba: &'+str(row['rgba'])+' },')
 source.append('];');rust.write_text('\n'.join(source)+'\n')
 subprocess.run(['rustfmt','+nightly','--config-path',str(ROOT/'System7/rustfmt.toml'),'--edition','2024',str(rust)],check=True)
 print('Generated',len(oracle),'valid sample oracles and',len(bad),'unsupported/malformed/demo fixtures, plus the shared no_std Rust oracle.')
if __name__=='__main__':main()
