#!/usr/bin/env python3
"""Compare Rust standard WDEF pixels with controlled original Mac captures."""
import argparse
from collections import Counter
import hashlib
import json
from pathlib import Path
import subprocess

from PIL import Image

ROOT = Path(__file__).resolve().parents[2]
RESULTS = ROOT / 'results/wdef-reference'
BUILD = ROOT / 'build/system7/wdef-host'


def build_renderer():
    subprocess.run(['cargo','build','--manifest-path',str(ROOT/'System7/Cargo.toml'),
                    '--target-dir',str(BUILD),'-p','mac-system','--example','wdef_render'],check=True)


def check_guest_window(screenshot_path, results, bounds=None, title="About RISCy System", procedure=0, active=True, grow_clip=None):
    """Verify the production About window's loaded native WDEF against the host.

    The host renderer is separately checked against original Mac goldens. Its
    background sentinel and application content are excluded from this mask;
    every frame/title pixel and the grow drawing allowed by the application
    clip are checked. Finder's separately owned controls are compared against
    original Finder captures by check_finder_volumes.py.
    """
    build_renderer()
    results = Path(results)
    results.mkdir(parents=True,exist_ok=True)
    bounds = list(bounds) if bounds is not None else [380,150,614,405]
    path = results/'guest-wdef-host.ppm'
    subprocess.run([str(BUILD/'debug/examples/wdef_render'),str(path),str(procedure),str(int(active)),'0','0',
                    *map(str,bounds),title],check=True)
    with Image.open(screenshot_path) as guest, Image.open(path) as host:
        guest,host = guest.convert('RGB'),host.convert('RGB')
        assert guest.size == host.size == (640,480)
        actual,expected = guest.load(),host.load()
        left,top,right,bottom = bounds
        mask = Image.new('L',host.size)
        mask_pixels = mask.load()
        differences = []
        compared = 0
        color_pairs = Counter()
        diff = Image.new('RGB',host.size)
        diff_pixels = diff.load()
        for y in range(max(0,top-24),min(480,bottom+11)):
            for x in range(max(0,left-10),min(640,right+11)):
                inside = left <= x < right and top <= y < bottom
                grow_visible = grow_clip is None or (grow_clip[0] <= x-left < grow_clip[2] and grow_clip[1] <= y-top < grow_clip[3])
                owned = (not inside and expected[x,y] != (128,128,128)) or (
                    inside and grow_visible and (x == right-15 or y == bottom-15 or (x >= right-15 and y >= bottom-15)))
                if not owned:
                    continue
                mask_pixels[x,y] = 255
                compared += 1
                if actual[x,y] != expected[x,y]:
                    differences.append((x,y))
                    color_pairs[(expected[x,y],actual[x,y])] += 1
                    diff_pixels[x,y] = (255,0,255)
                else:
                    diff_pixels[x,y] = tuple(c//3 for c in actual[x,y])
        host.save(results/'guest-wdef-host.png')
        mask.save(results/'guest-wdef-mask.png')
        diff.save(results/'guest-wdef-diff.png')
    record = {'check':'loaded RV64 WDEF reproduces reference-checked frame/title/grow pixels',
              'status':'passed' if not differences else 'mismatch','procedure':procedure,'active':active,
              'content':bounds,'title':title,'compared_pixels':compared,'different_pixels':len(differences),
              'mask':'frame and title outside content; WDEF grow/separator drawing within the application clip',
              'grow_clip':grow_clip,
              'mask_image':str((results/'guest-wdef-mask.png').relative_to(ROOT)),
              'guest_screenshot':str(Path(screenshot_path).relative_to(ROOT)),
              'guest_sha256':hashlib.sha256(Path(screenshot_path).read_bytes()).hexdigest(),
              'host_sha256':hashlib.sha256((results/'guest-wdef-host.png').read_bytes()).hexdigest(),
              'original_reference_checks':'results/system7/wdef-appearance-checks.json',
              'color_mismatches':[{'expected':list(pair[0]),'actual':list(pair[1]),'pixels':count}
                                  for pair,count in color_pairs.most_common(16)]}
    (results/'guest-wdef-checks.json').write_text(json.dumps(record,indent=2)+'\n')
    assert not differences, f'Loaded guest WDEF differs at {len(differences)}/{compared} pixels; see {results}/guest-wdef-diff.png'
    return record


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--report-only', action='store_true', help='Write mismatch evidence without failing')
    parser.add_argument('--no-build', action='store_true')
    args = parser.parse_args()
    if not args.no_build:
        build_renderer()
    references = json.loads((RESULTS/'reference.json').read_text())
    output = RESULTS/'comparison'
    output.mkdir(parents=True,exist_ok=True)
    records = []
    for case in references['captures']:
        if case['title'] == 'Palette':
            continue
        name = Path(case['image']).stem
        path = output/(name+'.ppm')
        bounds = case['content']
        subprocess.run([str(BUILD/'debug/examples/wdef_render'),str(path),str(case['procedure']),
                        str(int(case['active'])),str(int(case['pressed']=='close')),
                        str(int(case['pressed']=='zoom')),*map(str,bounds),case['title']],check=True)
        with Image.open(ROOT/case['image']) as original, Image.open(path) as native:
            original,native = original.convert('RGB'),native.convert('RGB')
            left,top,right,bottom = bounds
            crop = (left-10,top-24,right+11,bottom+11)
            actual,expected = native.load(),original.load()
            differences = []
            color_pairs = Counter()
            diff = Image.new('RGB',original.size)
            diff_pixels = diff.load()
            compared = 0
            for y in range(crop[1],crop[3]):
                for x in range(crop[0],crop[2]):
                    value = actual[x,y]
                    # Host renderer marks its unpainted background with #808080.
                    # Substitute the actual Mac desktop checkerboard, preserving
                    # all window pixels, including missing or extra edge pixels.
                    if value == (128,128,128):
                        value = (135,135,135) if (x+y)%2 == 0 else (192,192,192)
                        actual[x,y] = value
                    compared += 1
                    if value != expected[x,y]:
                        differences.append((x,y))
                        color_pairs[(expected[x,y],value)] += 1
                        diff_pixels[x,y] = (255,0,255)
                    else:
                        diff_pixels[x,y] = tuple(c//3 for c in value)
            native.save(output/(name+'.png'))
            diff.save(output/(name+'-diff.png'))
            box = ([min(x for x,y in differences),min(y for x,y in differences),
                    max(x for x,y in differences)+1,max(y for x,y in differences)+1] if differences else None)
            records.append({'name':name,'status':'passed' if not differences else 'mismatch',
                            'procedure':case['procedure'],'active':case['active'],'pressed':case['pressed'],
                            'content':bounds,'title':case['title'],'compared_pixels':compared,
                            'comparison_rect':list(crop),'masked_pixels':0,
                            'different_pixels':len(differences),'different_bounds':box,
                            'matching_percent':round(100*(compared-len(differences))/compared,5),
                            'color_mismatches':[{'expected':list(pair[0]),'actual':list(pair[1]),'pixels':count}
                                                for pair,count in color_pairs.most_common(16)],
                            'original':case['image'],
                            'original_sha256':hashlib.sha256((ROOT/case['image']).read_bytes()).hexdigest(),
                            'native':str((output/(name+'.png')).relative_to(ROOT)),
                            'native_sha256':hashlib.sha256((output/(name+'.png')).read_bytes()).hexdigest(),
                            'difference':str((output/(name+'-diff.png')).relative_to(ROOT))})
    passed = sum(case['status']=='passed' for case in records)
    report = {'status':'passed' if passed==len(records) else 'mismatch','passed':passed,'total':len(records),
              'method':'Exact RGB comparison of standard WDEF + DrawGrowIcon pixels, same content geometry/title/style/state; unpainted host background uses actual original checkerboard',
              'reference_framebuffer':[640,480,8],'native_framebuffer':[640,480,32],
              'reference_manifest_sha256':hashlib.sha256((RESULTS/'reference.json').read_bytes()).hexdigest(),
              'total_compared_pixels':sum(case['compared_pixels'] for case in records),
              'total_different_pixels':sum(case['different_pixels'] for case in records),
              'cases':records}
    (RESULTS/'pixel-checks.json').write_text(json.dumps(report,indent=2)+'\n')
    report_path = ROOT/'results/system7/wdef-appearance-checks.json'
    report_path.parent.mkdir(parents=True,exist_ok=True)
    report_path.write_text(json.dumps(report,indent=2)+'\n')
    print(f'WDEF exact pixel comparison: {passed}/{len(records)} cases match; '
          f'{sum(case["different_pixels"] for case in records)} differing pixels')
    if passed != len(records) and not args.report_only:
        raise SystemExit(1)


if __name__=='__main__':
    main()
