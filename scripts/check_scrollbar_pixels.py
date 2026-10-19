#!/usr/bin/env python3
"""Require exact original scrollbar pixels for both orientations and widths."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
from PIL import Image, ImageChops
ROOT = Path(__file__).resolve().parents[2]
BOUNDS = [(380,100,395,280),(400,100,416,280),(100,280,380,295),(100,305,380,321)]

def digest(path): return hashlib.sha256(path.read_bytes()).hexdigest()

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--no-build',action='store_true')
    args=parser.parse_args()
    target=ROOT/'build/system7/files-work'
    if not args.no_build:
        subprocess.run(['cargo','build','--locked','--manifest-path','System7/Cargo.toml','--target-dir',str(target),'-p','mac-system','--example','scrollbar_render'],cwd=ROOT,check=True)
    binary=target/'debug/examples/scrollbar_render'
    output=ROOT/'results/system7/scrollbars';output.mkdir(parents=True,exist_ok=True)
    report={'status':'passed','method':'Same native CDEF1 resource entry and resident Surface gate on host; exact RGB comparison of every control-owned pixel','masked_pixels':0,'pixels_compared':0,'different_pixels':0,'renderer_sha256':digest(binary),'cases':[]}
    labels=['minimum','middle','maximum','inactive','zero-range','up-arrow-pressed','down-arrow-pressed','thumb-pressed']
    for page,label in enumerate(labels):
        original=ROOT/f'results/finder-reference/cdef-page-{page}.png'
        native=output/f'page-{page}.ppm'
        subprocess.run([str(binary),str(native),str(page)],cwd=ROOT,check=True)
        reference=Image.open(original).convert('RGB');actual=Image.open(native).convert('RGB')
        difference=Image.new('RGB',reference.size)
        case={'state':label,'page':page,'reference':str(original.relative_to(ROOT)),'reference_sha256':digest(original),'native':str(native.relative_to(ROOT)),'native_sha256':digest(native),'controls':[],'different_pixels':0}
        for index,box in enumerate(BOUNDS):
            delta=ImageChops.difference(reference.crop(box),actual.crop(box))
            count=sum(p!=(0,0,0)for p in delta.getdata())
            area=(box[2]-box[0])*(box[3]-box[1])
            case['controls'].append({'index':index,'bounds':box,'pixels':area,'different_pixels':count,'difference_bounds':delta.getbbox()})
            case['different_pixels']+=count;report['different_pixels']+=count;report['pixels_compared']+=area
            difference.paste(delta,box)
        actual.save(output/f'page-{page}.png');difference.save(output/f'page-{page}-difference.png')
        report['cases'].append(case)
    hit_output=output/'hits.json'
    subprocess.run([str(binary),'--hits',str(hit_output)],cwd=ROOT,check=True)
    original_hits=ROOT/'results/finder-reference/cdef-hits.json'
    expected=json.loads(original_hits.read_text());actual_hits=json.loads(hit_output.read_text())
    mismatches=[{'expected':e,'actual':a}for e,a in zip(expected,actual_hits)if e!=a]
    report['hit_tests']={'reference':str(original_hits.relative_to(ROOT)),'reference_sha256':digest(original_hits),'native_sha256':digest(hit_output),'samples':len(expected),'different_samples':len(mismatches),'differences':mismatches}
    if len(expected)!=len(actual_hits)or mismatches:report['status']='failed'
    if report['different_pixels']:report['status']='failed'
    (ROOT/'results/system7/scrollbar-appearance-checks.json').write_text(json.dumps(report,indent=2)+'\n')
    print(f"Scrollbars: {len(report['cases'])*4} controls, {report['different_pixels']}/{report['pixels_compared']} different pixels")
    return 0 if report['status']=='passed'else 1
if __name__=='__main__':raise SystemExit(main())
