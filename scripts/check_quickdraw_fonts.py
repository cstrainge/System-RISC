#!/usr/bin/env python3
"""Compare resident fonts with original System7 runtime; record TrueType gaps."""
import hashlib
import json
from pathlib import Path
from system7_paths import dist_dir
import re
import subprocess

from PIL import Image,ImageChops
from extract_standard_wdef_assets import resources

ROOT=Path(__file__).resolve().parents[2]
RESULTS=ROOT/'results/system7/fonts'
REFERENCE=ROOT/'results/quickdraw-reference'
BUILD=ROOT/'build/system7/font-work'


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def fields(line):
    return {key:int(value) for key,value in re.findall(r'(\w+)=(-?\d+)',line)}


def main():
    RESULTS.mkdir(parents=True,exist_ok=True)
    source=dist_dir()/'System.rsrc'
    bundle=json.loads((ROOT/'provenance/system7-font-bundle.json').read_text())
    packed={(kind.decode('mac_roman'),rid):hashlib.sha256(data).hexdigest()
            for kind,rid,_,_,data in resources(source.read_bytes())}
    for asset in bundle['assets']:
        assert packed.get((asset['type'],asset['id']))==asset['sha256'], 'packaged font mismatch: '+asset['asset']
    subprocess.run(['cargo','build','--manifest-path',str(ROOT/'System7/Cargo.toml'),
                    '--target-dir',str(BUILD),'-p','mac-system','--example','font_render'],check=True)
    original={}
    styles={}
    for line in (REFERENCE/'runtime.txt').read_text().splitlines():
        if line.startswith('font '):
            values=fields(line);original[values['page'],values['row']]=values
        if line.startswith('style '):
            values=fields(line);styles[values['face']]=values
    pages=[(0,'page-0-fonts-default-content.png',False),
           (1,'page-1-fonts-outline-content.png',False),
           (2,'page-2-font-styles-content.png',False),
           (1,'page-1-fonts-outline-content.png',True)]
    records=[]
    gates=[]
    for page,filename,unhinted in pages:
        label='unhinted' if unhinted else ['bitmap','hinted','styles'][page]
        path=RESULTS/f'{label}.ppm'
        command=[str(BUILD/'debug/examples/font_render'),str(source),str(path),str(page)]
        if unhinted:command.append('--unhinted')
        log=subprocess.check_output(command,text=True)
        (RESULTS/f'{label}.txt').write_text(log)
        with Image.open(REFERENCE/filename) as expected,Image.open(path) as actual:
            expected,actual=expected.convert('RGB'),actual.convert('RGB')
            assert expected.size==actual.size==(640,460)
            actual.save(RESULTS/f'{label}.png')
            difference=ImageChops.difference(expected,actual)
            difference.save(RESULTS/f'{label}-difference.png')
            rows=[]
            for line in log.splitlines():
                native=fields(line);row=native['row']
                gold=styles[native['face']] if page==2 else original[page,row]
                pairs=[('text_width','width' if page==2 else 'text_width'),
                       ('ascent','ascent'),('descent','descent'),('leading','leading')]
                if page!=2:pairs.extend([('advance','advance'),('max','max')])
                mismatch={key:{'original':gold[reference],'native':native[key]}
                          for key,reference in pairs if native[key]!=gold[reference]}
                area=(0,row*40,640,(row+1)*40)
                different=sum(pixel!=(0,0,0) for pixel in difference.crop(area).getdata())
                style_probe=(13,row*40+16,29,row*40+35) if page==2 else None
                style_probe_differences=(sum(pixel!=(0,0,0) for pixel in difference.crop(style_probe).getdata())
                                         if style_probe else None)
                rows.append({'row':row,'family':native['family'],'size':native['size'],'face':native['face'],
                             'source':line.split(' source=',1)[1],
                             'baseline':[16,32+row*40],'comparison_rect':list(area),
                             'compared_pixels':640*40,'different_pixels':different,
                             'exact_pixels':different==0,'metric_differences':mismatch,
                             'style_probe_rect':list(style_probe) if style_probe else None,
                             'style_probe_different_pixels':style_probe_differences,
                             'native_metrics':{key:native[key] for key,_ in pairs},
                             'original_metrics':{key:gold[reference] for key,reference in pairs}})
                if not unhinted:
                    gates.append(not mismatch)
                    if page==0:gates.append(different==0)
                    if page==2 and native['face'] in (8,16):gates.append(style_probe_differences==0)
            records.append({'mode':label,'page':page,'original':str((REFERENCE/filename).relative_to(ROOT)),
                            'original_sha256':sha(REFERENCE/filename),'native':str((RESULTS/f'{label}.png').relative_to(ROOT)),
                            'native_sha256':sha(RESULTS/f'{label}.png'),
                            'compared_pixels':sum(row['compared_pixels'] for row in rows),
                            'different_pixels':sum(row['different_pixels'] for row in rows),
                            'rows':rows})
    report={'status':'passed' if all(gates) else 'failed','strict_gates':'All ten default bitmap rows pixel-identical; all bitmap/hinted/style widths, advances and recorded font metrics match original runtime',
            'true_type_pixels':'Measured independently; residual differences remain and are not treated as exact parity.',
            'bundle_resources_verified':len(bundle['assets']),'families':bundle['families'],'sfnt_faces':bundle['scalable_faces'],
            'system_resource_fork_sha256':sha(source),'original_runtime_sha256':sha(REFERENCE/'runtime.txt'),
            'implementation':'Original sfnt TrueType programs interpreted by skrifa0.48.0 Target::Mono; nonzero-winding center-sampled monochrome rasterizer',
            'comparison_bounds':'Each full640x40 text row; blank lower60pixels excluded because original physical display corners are rounded.',
            'limitations':['Apple-specific compound accent placement differs from modern scaler behavior.','Original per-glyph SCANCTRL/SCANTYPE dropout settings are not exposed by the selected public outline API.','No automatic kerning or complex-script shaping.','Synthetic outline/shadow styling is approximate.'],
            'comparisons':records}
    (ROOT/'results/system7/font-comparison-checks.json').write_text(json.dumps(report,indent=2)+'\n')
    for case in records:
        print(f'{case["mode"]}: {case["different_pixels"]}/{case["compared_pixels"]} RGB pixels differ; '
              f'{sum(bool(row["metric_differences"]) for row in case["rows"])} metric rows differ')
    if not all(gates):raise SystemExit('original font comparison strict gates failed')


if __name__=='__main__':main()
