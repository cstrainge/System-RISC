#!/usr/bin/env python3
"""Strict original Finder icon-view comparison using the production view policy."""
import argparse
import hashlib
import json
from pathlib import Path
from system7_paths import dist_dir, describe
import subprocess
from PIL import Image, ImageChops
ROOT=Path(__file__).resolve().parents[2]
def sha(path):return hashlib.sha256(path.read_bytes()).hexdigest()
def main():
    parser=argparse.ArgumentParser(description=__doc__);parser.add_argument('--no-build',action='store_true');args=parser.parse_args()
    target=ROOT/'build/system7/finder-host'
    if not args.no_build:
        subprocess.run(['cargo','build','--locked','--manifest-path',str(ROOT/'System7/Cargo.toml'),'--target-dir',str(target),'-p','mac-system','--example','finder_reference'],cwd=ROOT,check=True)
    output=ROOT/'results/finder-reference/native';output.mkdir(parents=True,exist_ok=True)
    records=[]
    for mode,reference in [('normal','reference-large-icons-normal'),('selected','alpha-selected-closed'),('open','alpha-normal-open'),('selected-open','alpha-selected-open')]:
        path=output/(mode+'.ppm');original=ROOT/'results/finder-reference'/(reference+'.png')
        result=subprocess.run([str(target/'debug/examples/finder_reference'),mode,str(path)],cwd=ROOT,text=True,capture_output=True,check=True)
        (output/(mode+'.txt')).write_text(result.stdout)
        actual=Image.open(path).convert('RGB');expected=Image.open(original).convert('RGB').crop((8,46,412,234));rect=(0,20,389,173)
        difference=ImageChops.difference(actual.crop(rect),expected.crop(rect));count=sum(p!=(0,0,0)for p in difference.getdata())
        actual.save(output/(mode+'.png'));difference.save(output/(mode+'-difference.png'))
        records.append({'case':mode,'status':'passed'if count==0 else'failed','original':str(original.relative_to(ROOT)),'original_sha256':sha(original),'native_sha256':sha(path),'content_origin':[8,46],'comparison_rect':list(rect),'compared_pixels':389*153,'different_pixels':count,'masked_pixels':0})
    for fixture in json.loads((ROOT/'results/finder-reference/gap-boundaries.json').read_text()):
        case=fixture['case'];mode='gap-'+str(case);reference=('gap-touch-'+str(case)[5:])if str(case).startswith('touch')else('gap-test-'+str(case))
        path=output/(mode+'.ppm');original=ROOT/'results/finder-reference'/(reference+'.png')
        subprocess.run([str(target/'debug/examples/finder_reference'),mode,str(path),*fixture['names']],cwd=ROOT,capture_output=True,check=True)
        actual=Image.open(path).convert('RGB');expected=Image.open(original).convert('RGB').crop((8,46,412,234));rect=(0,20,389,173)
        difference=ImageChops.difference(actual.crop(rect),expected.crop(rect));count=sum(p!=(0,0,0)for p in difference.getdata());difference.save(output/(mode+'-difference.png'))
        records.append({'case':mode,'status':'passed'if count==0 else'failed','names':fixture['names'],'observed_centers':fixture['observed_local_centers'],'original':str(original.relative_to(ROOT)),'original_sha256':sha(original),'comparison_rect':list(rect),'compared_pixels':389*153,'different_pixels':count,'masked_pixels':0})
    for mode,reference in [('desktop-normal','volume-normal-closed'),('desktop-selected','volume-selected-closed'),('desktop-selected-open','root-open-volume-normal'),('desktop-two-normal','two-volumes-normal'),('desktop-two-selected','two-volumes'),('desktop-two-open','second-volume-normal-open'),('desktop-two-selected-open','second-volume-selected-open')]:
        path=output/(mode+'.ppm');original=ROOT/'results/finder-reference'/(reference+'.png')
        subprocess.run([str(target/'debug/examples/finder_reference'),mode,str(path)],cwd=ROOT,capture_output=True,check=True)
        actual=Image.open(path).convert('RGB');expected=Image.open(original).convert('RGB');rect=(552,22,640,132)if 'two'in mode else(552,22,640,80)
        difference=ImageChops.difference(actual.crop(rect),expected.crop(rect));count=sum(p!=(0,0,0)for p in difference.getdata())
        difference.save(output/(mode+'-difference.png'));actual.save(output/(mode+'.png'))
        records.append({'case':mode,'status':'passed'if count==0 else'failed','original':str(original.relative_to(ROOT)),'original_sha256':sha(original),'native_sha256':sha(path),'comparison_rect':list(rect),'compared_pixels':(rect[2]-rect[0])*(rect[3]-rect[1]),'different_pixels':count,'masked_pixels':0})
    actual=Image.open(output/'desktop-normal.ppm').convert('RGB');original=ROOT/'results/finder-reference/volume-normal-closed.png';expected=Image.open(original).convert('RGB');rect=(560,414,625,472)
    difference=ImageChops.difference(actual.crop(rect),expected.crop(rect));count=sum(p!=(0,0,0)for p in difference.getdata());difference.save(output/'trash-difference.png')
    records.append({'case':'desktop-trash','status':'passed'if count==0 else'failed','original':str(original.relative_to(ROOT)),'original_sha256':sha(original),'comparison_rect':list(rect),'compared_pixels':65*58,'different_pixels':count,'masked_pixels':0})
    report={'status':'passed'if all(r['status']=='passed'for r in records)else'failed','method':'Production Finder icon_view.rs placement, caption and selection policy with resident font/icon engines and current System resources; full original item-area and mounted-volume desktop-cell RGB comparisons, no pixel masks or tolerances',
            'font':'Original Geneva9, family3, pointsize9, face0','grid':[64,52],'icon_first_rect':[16,28,48,60],'label_baseline_offset':42,'cases':records,
            'total_compared_pixels':sum(r['compared_pixels']for r in records),'total_different_pixels':sum(r['different_pixels']for r in records),
            'artifacts':[{'path':describe(p),'sha256':sha(p)}for p in [dist_dir()/'System.rsrc',ROOT/'System7/finder/src/icon_view.rs',ROOT/'System7/system/examples/finder_reference.rs']],
            'scope':'Exact bounded original fixture states, including long-name grid spacing. Scrollbars/info-band evidence are tracked separately; this is not a claim of full Finder behavior.'}
    path=ROOT/'results/system7/finder-icon-checks.json';path.write_text(json.dumps(report,indent=2)+'\n')
    print(f'Finder icons: {sum(r["status"]=="passed"for r in records)}/{len(records)} cases; {report["total_compared_pixels"]} pixels, {report["total_different_pixels"]} differences')
    if report['status']!='passed':raise SystemExit(1)
if __name__=='__main__':main()
