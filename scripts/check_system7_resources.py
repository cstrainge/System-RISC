#!/usr/bin/env python3
"""Verify original font/icon/picture resources in the actual packaged forks."""
import hashlib,json,struct
from pathlib import Path
from extract_standard_wdef_assets import resources
ROOT=Path(__file__).resolve().parents[2]
def sha(data):return hashlib.sha256(data).hexdigest()
def resource_names(raw):
    _,map_offset,_,_=struct.unpack_from('>4I',raw)
    types_offset,names_offset=struct.unpack_from('>HH',raw,map_offset+24)
    types,names=map_offset+types_offset,map_offset+names_offset
    out={}
    for i in range(struct.unpack_from('>H',raw,types)[0]+1):
        kind,last,refs=struct.unpack_from('>4sHH',raw,types+2+i*8)
        for j in range(last+1):
            rid,name=struct.unpack_from('>hh',raw,types+refs+j*12)
            if name>=0:
                length=raw[names+name]
                out[kind.decode('mac_roman'),rid]=raw[names+name+1:names+name+1+length].decode('mac_roman')
    return out
def main():
    visual=json.loads((ROOT/'provenance/system7-visual-resource-bundle.json').read_text())
    fonts=json.loads((ROOT/'provenance/system7-font-bundle.json').read_text())
    reports=[]
    for name in ['System','Finder']:
        path=ROOT/'build/system7/dist'/(name+'.rsrc');blob=path.read_bytes()
        names=resource_names(blob)
        packed={(kind.decode('mac_roman'),rid):(attrs,names.get((kind.decode('mac_roman'),rid)),data)
                for kind,rid,attrs,_,data in resources(blob)}
        package=next(p for p in visual['packages']if p['name']==name)
        required=package['assets']+(fonts['assets']if name=='System'else[])
        checked=[]
        for asset in required:
            key=asset['type'],asset['id'];assert key in packed, f'missing {name} {key}'
            attrs,resource_name,data=packed[key]
            assert sha(data)==asset['sha256'],f'payload changed: {name} {key}'
            assert not attrs&1,f'unhandled compressed resource {name} {key}'
            if 'attributes'in asset:assert attrs==asset['attributes'],f'attributes changed: {name} {key}'
            assert resource_name==asset.get('name'),f'resource name changed: {path.stem} {key}: {resource_name!r}'
            checked.append({'type':key[0],'id':key[1],'length':len(data),'sha256':sha(data)})
        if name=='System':
            for kind,rid in [('clut',4),('clut',8),('ICN#',-3999),('ICN#',-3983),('ICN#',-3993)]:assert(kind,rid)in packed
            for (kind,rid),(_,_,data) in packed.items():
                if kind!='FOND':continue
                count=struct.unpack_from('>H',data,52)[0]+1
                for index in range(count):
                    size,style,target=struct.unpack_from('>3h',data,54+index*6)
                    assert (('sfnt',target)in packed if size==0 else ('NFNT',target)in packed or ('FONT',target)in packed), f'broken family association FOND{rid} size{size} face{style} ->{target}'
        else:assert('PICT',24000)in packed,'Finder PICT exercise must resolve through Resource Manager'
        reports.append({'package':name,'fork':str(path.relative_to(ROOT)),'fork_sha256':sha(blob),'total_resources':len(packed),
            'original_visual_resources_verified':len(package['assets']),'font_resources_verified':len(fonts['assets'])if name=='System'else 0,
            'checked_payloads':checked})
    report={'status':'passed','method':'Read actual generated System/Finder resource forks; verify every imported visual/font payload and attributes, all FOND association targets, and resource-backed Finder PICT',
            'font_families':fonts['families'],'true_type_faces':fonts['scalable_faces'],'packages':reports,
            'resource_api_tests':'System7/system/tests/resource_manager_api.rs',
            'scope':'Read-only resident System/application resource chain; disk editing and original 68k Handle ABI remain future work.'}
    path=ROOT/'results/system7/resource-manager-checks.json';path.write_text(json.dumps(report,indent=2)+'\n')
    print(f'PASS: {len(fonts["assets"])} font resources and {sum(len(p["assets"]) for p in visual["packages"])} visual resources preserved in packaged System/Finder; all FOND references resolve')
if __name__=='__main__':main()
