#!/usr/bin/env python3
"""Compare resident icon drawing with retained original System 7.6.1 Icon Utilities."""
import argparse
import hashlib
import json
from pathlib import Path
from system7_paths import host_dir
from system7_paths import dist_dir, describe
import subprocess
from PIL import Image

ROOT = Path(__file__).resolve().parents[2]
RESULTS = ROOT / 'results/quickdraw-reference'
BUILD = host_dir()

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--no-build', action='store_true')
    parser.add_argument('--report-only', action='store_true')
    args = parser.parse_args()
    if not args.no_build:
        subprocess.run(['cargo','build','--manifest-path',str(ROOT/'System7/Cargo.toml'),
                        '--target-dir',str(BUILD),'-p','mac-system','--example','icons_reference'],
                       check=True)
    binary = BUILD/'debug/examples/icons_reference'
    original_validation = subprocess.run([str(binary),'--validate'], check=True,capture_output=True,text=True)
    validation = subprocess.run([str(binary),'--validate','--bundled'], check=True,capture_output=True,text=True)
    output = RESULTS/'icons-comparison'
    output.mkdir(parents=True,exist_ok=True)
    metadata = json.loads((RESULTS/'icons.json').read_text())
    cases=[]
    for item in metadata['icons']:
        case=item.copy()
        if case['source']=='System':
            page={3:16,16387:17}.get(case['transform'],9)
        elif case['source']=='Finder':page=10
        elif case['source'] in ['Synthetic8','Synthetic4']:
            page={16385:13,3:14,16387:15}.get(case['transform'],12)
        else:raise AssertionError(f"Unknown original icon source {case['source']}")
        case['page']=page
        cases.append(case)
    pages=[]
    descriptors=[(9,'System',None,'system'),(10,'Finder',None,'finder'),
                 (12,'Synthetic',None,'synthetic'),(13,'Synthetic','combined','combined'),
                 (14,'Synthetic','open','open'),(15,'Synthetic','selected-open','selected-open'),
                 (16,'System','open','open-stock'),(17,'System','selected-open','selected-open-stock')]
    for page,source,mode,name in descriptors:
        native=output/f'page-{page}.ppm'
        command=[str(binary),source,str(native)]
        if mode:command.append(mode)
        subprocess.run(command+['--bundled'],check=True,capture_output=True)
        original=RESULTS/f'page-{page}-{name}-icons-content.png'
        actual=Image.open(native).convert('RGB')
        expected=Image.open(original).convert('RGB')
        differences=[(x,y) for y in range(460) for x in range(640)
                     if actual.getpixel((x,y)) != expected.getpixel((x,y))]
        actual.save(output/f'page-{page}.png')
        diff=Image.new('RGB',(640,460),'black')
        for position in differences:diff.putpixel(position,(255,0,255))
        diff.save(output/f'page-{page}-difference.png')
        for case in cases:
            if case['page']!=page:continue
            x,y,size=case['x'],case['y'],case['size']
            case['different_pixels']=sum(actual.getpixel((x+i,y+j))!=expected.getpixel((x+i,y+j))
                                         for j in range(size) for i in range(size))
            case['status']='passed' if case['error']==0 and not case['different_pixels'] else 'mismatch'
        pages.append({'source':source,'page':page,'comparison_rect':[0,0,640,460],
                      'compared_pixels':640*460,'different_pixels':len(differences),'masked_pixels':0,
                      'original':str(original.relative_to(ROOT)),
                      'original_sha256':hashlib.sha256(original.read_bytes()).hexdigest(),
                      'native_sha256':hashlib.sha256(native.read_bytes()).hexdigest()})
    passed=all(not page['different_pixels'] for page in pages) and all(case['status']=='passed' for case in cases)
    report={'status':'passed' if passed else 'mismatch','pages':pages,'cases':cases,
            'passed_cases':sum(case['status']=='passed' for case in cases),'total_cases':len(cases),
            'total_compared_pixels':sum(page['compared_pixels'] for page in pages),
            'total_different_pixels':sum(page['different_pixels'] for page in pages),
            'resource_validation':validation.stdout.strip().splitlines(),
            'original_resource_validation':original_validation.stdout.strip().splitlines(),
            'bundled_artifacts':[{'path':describe(path), 'sha256':hashlib.sha256(path.read_bytes()).hexdigest()} for path in [dist_dir()/'System.rsrc',dist_dir()/'Finder.rsrc',ROOT/'System7/system/assets/icons/system7-indexed-display.bin']],
            'method':'Whole-page exact RGB comparison, no masks, using the current bundled System/Finder resource forks, the original source snapshots and original synthetic full-palette families. Display and transformation tables are measured separately from resource decoding.'}
    (RESULTS/'icon-checks.json').write_text(json.dumps(report,indent=2)+'\n')
    print(f'Icons: {report["passed_cases"]}/{report["total_cases"]} cases; '
          f'{report["total_compared_pixels"]} pixels, {report["total_different_pixels"]} differences')
    if not passed and not args.report_only:raise SystemExit(1)

if __name__=='__main__':main()
