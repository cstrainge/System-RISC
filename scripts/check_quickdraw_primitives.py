#!/usr/bin/env python3
"""Compare resident QuickDraw primitives against retained original System 7.6.1 execution."""
from collections import Counter
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
from PIL import Image

ROOT = Path(__file__).resolve().parents[2]
RESULTS = ROOT / 'results/quickdraw-reference'
BUILD = ROOT / 'build/system7/quickdraw-host'

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--no-build', action='store_true')
    parser.add_argument('--report-only', action='store_true')
    args = parser.parse_args()
    if not args.no_build:
        subprocess.run(['cargo', 'build', '--manifest-path', str(ROOT/'System7/Cargo.toml'),
                        '--target-dir', str(BUILD), '-p', 'mac-system', '--example',
                        'quickdraw_reference'], check=True)
    output = RESULTS/'primitives-comparison'
    output.mkdir(parents=True, exist_ok=True)
    binary = BUILD/'debug/examples/quickdraw_reference'
    cases = []
    for page, name, crop in [(3,'shapes',(0,0,640,460)),
                              (4,'bitmodes',(20,20,550,220)),
                              (5,'regions',(0,0,640,460)),
                              (7,'direct-modes',(0,0,640,460))]:
        path = output/f'page-{page}.ppm'
        subprocess.run([str(binary), str(page), str(path)], check=True, capture_output=True)
        original = RESULTS/f'page-{page}-{name}-content.png'
        expected = Image.open(original).convert('RGB')
        actual = Image.open(path).convert('RGB')
        differences = []
        colors = Counter()
        for y in range(crop[1],crop[3]):
            for x in range(crop[0],crop[2]):
                a,b = actual.getpixel((x,y)),expected.getpixel((x,y))
                if a != b:
                    differences.append((x,y))
                    colors[(b,a)] += 1
        actual.save(output/f'page-{page}.png')
        cases.append({'page':page,'name':name,'status':'passed' if not differences else 'mismatch',
                      'comparison_rect':list(crop),'compared_pixels':(crop[2]-crop[0])*(crop[3]-crop[1]),
                      'different_pixels':len(differences),'masked_pixels':0,
                      'different_bounds':([min(x for x,y in differences),min(y for x,y in differences),
                                          max(x for x,y in differences)+1,max(y for x,y in differences)+1]
                                          if differences else None),
                      'color_mismatches':[{'expected':list(pair[0]),'actual':list(pair[1]),'pixels':count}
                                           for pair,count in colors.most_common(12)],
                      'original':str(original.relative_to(ROOT)),
                      'original_sha256':hashlib.sha256(original.read_bytes()).hexdigest(),
                      'native_sha256':hashlib.sha256(path.read_bytes()).hexdigest()})
    edge_path = output/'edge-corpus.json'
    subprocess.run([str(binary),'6',str(edge_path)],check=True)
    expected = json.loads((RESULTS/'edge-corpus.json').read_text())
    actual = json.loads(edge_path.read_text())
    lines = []
    for reference in expected['lines']:
        candidate = next(item for item in actual['lines'] if item['delta']==reference['delta'])
        difference = set(map(tuple,candidate['points'])) ^ set(map(tuple,reference['points']))
        lines.append({'delta':reference['delta'],'different_pixels':len(difference)})
    ovals = []
    for reference in expected['oval_regions']:
        candidate = next(item for item in actual['oval_regions'] if item['name']==reference['name'])
        ovals.append({'name':reference['name'],'exact_region_match':candidate['bytes_hex']==reference['bytes_hex']})
    passed = (all(case['status']=='passed' for case in cases)
              and not any(line['different_pixels'] for line in lines)
              and all(oval['exact_region_match'] for oval in ovals))
    report = {'status':'passed' if passed else 'mismatch','cases':cases,
              'total_compared_pixels':sum(case['compared_pixels'] for case in cases),
              'total_different_pixels':sum(case['different_pixels'] for case in cases),
              'line_corpus':{'passed':sum(not item['different_pixels'] for item in lines),'total':len(lines),'cases':lines},
              'supplemental_oval_corpus':{'passed':sum(item['exact_region_match'] for item in ovals),
                                         'total':len(ovals),'cases':ovals,
                                         'required':True,
                                         'method':'Original fixed-point oval edge recurrence, including truncated FixRatio and retained half-pixel edge state.'},
              'method':'Exact RGB comparison of original execution with the same operations, local geometry, patterns and initial screen matte; no masked pixels. Colored Boolean modes use the original 32-bit GWorld, because indexed CLUT inversion has different documented semantics.'}
    (RESULTS/'primitive-checks.json').write_text(json.dumps(report,indent=2)+'\n')
    print(f'QuickDraw pixels: {sum(case["status"]=="passed" for case in cases)}/{len(cases)} cases, '
          f'{report["total_different_pixels"]} differences; lines {report["line_corpus"]["passed"]}/{len(lines)}; '
          f'oval regions {report["supplemental_oval_corpus"]["passed"]}/{len(ovals)}')
    if not passed and not args.report_only:
        raise SystemExit(1)

if __name__ == '__main__':
    main()
