#!/usr/bin/env python3
"""Verify original font/icon/picture resources in the actual packaged forks."""
import hashlib,json,struct
from pathlib import Path
from system7_paths import dist_dir, describe
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
    app_manifests={'QuickDraw Test':'apps/quickdraw-test', 'Text Edit':'apps/text-edit', 'Cursor Test':'apps/cursor-test', 'Panic Test':'apps/panic-test', 'Res Edit':'apps/resedit'}
    forks={}
    for name in ['System','Finder',*app_manifests]:
        path=dist_dir()/(name+'.rsrc');blob=path.read_bytes()
        names=resource_names(blob)
        packed={(kind.decode('mac_roman'),rid):(attrs,names.get((kind.decode('mac_roman'),rid)),data)
                for kind,rid,attrs,_,data in resources(blob)}
        forks[name]=packed
        package=next((p for p in visual['packages']if p['name']==name), {'assets':[]})
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
        elif name in app_manifests:
            manifest_path=ROOT/'System7'/app_manifests[name]/'resources.json'
            manifest=json.loads(manifest_path.read_text())
            for definition in manifest['resources']:
                key=definition['type'],definition['id']
                assert key in packed, f'missing application resource {name} {key}'
                attrs,resource_name,data=packed[key]
                assert attrs==definition.get('attributes',0), f'attributes changed: {name} {key}'
                assert resource_name==definition.get('name'), f'resource name changed: {name} {key}'
                payload=definition['payload']
                if payload['format']=='file':
                    expected=(manifest_path.parent/payload['path']).read_bytes()
                elif payload['format']=='hex':
                    expected=bytes.fromhex(payload['hex'])
                elif payload['format']=='pstring':
                    text=payload['text'].encode('mac_roman');expected=bytes([len(text)])+text
                elif payload['format']=='strings':
                    values=[text.encode('mac_roman') for text in payload['strings']]
                    expected=struct.pack('>H',len(values))+b''.join(bytes([len(v)])+v for v in values)
                else:
                    assert payload['format']=='menu', f'unchecked app resource format {payload["format"]}'
                    expected=data
                    assert struct.unpack_from('>h',data)[0]==key[1]
                    title=payload['title'].encode('mac_roman')
                    assert data[12:13+len(title)]==bytes([len(title)])+title
                    commands=b''.join((item.get('command','\0'*4).encode('mac_roman')
                                       if isinstance(item,dict) else bytes(4))
                                      for item in payload['items'])
                    assert packed['MCMD',key[1]][2]==commands, f'command table changed: {name} {key}'
                assert data==expected, f'payload changed: {name} {key}'
                checked.append({'type':key[0],'id':key[1],'length':len(data),'sha256':sha(data)})
            runtime=dist_dir()/(name+'.runtime.elf')
            elf=runtime.read_bytes();entry=struct.unpack_from('<Q',elf,24)[0]
            expected_abi=(b'RVN1'+struct.pack('>HHIIQQQ',1,2,manifest['abi_version'],0,
                                          int(manifest['load_base'],16),int(manifest['load_limit'],16),entry))
            assert packed['NABI',0][2]==expected_abi, f'application ABI/reservation changed: {name}'
            runtime=dist_dir()/(name+'.runtime.elf')
            # ELF exists only on the build host. Compare the exact initialized
            # PT_LOAD bytes against the resource records, with no extra baggage.
            phoff=struct.unpack_from('<Q',elf,32)[0];count=struct.unpack_from('<H',elf,56)[0]
            segments=[]
            for i in range(count):
                kind,flags,offset,address,_,size,_,_=struct.unpack_from('<IIQQQQQQ',elf,phoff+i*56)
                if kind==1 and size:segments.append((address-int(manifest['load_base'],16),flags,elf[offset:offset+size]))
            segments.sort()
            expected=b'RV64'+struct.pack('>HH',1,len(segments))+b''.join(struct.pack('>QQI',offset,len(data),flags)+data for offset,flags,data in segments)
            assert packed['RV64',0][2]==expected, f'application segment fork differs: {name}'
            inventory=json.loads((dist_dir()/(name+'.json')).read_text())
            for entry in inventory['resources']:
                key=entry['type'],entry['id']
                assert key in packed and sha(packed[key][2])==entry['sha256'], f'package inventory differs: {name} {key}'
            version=packed['vers',1][2]
            assert version[7:7+version[6]].decode('mac_roman')==inventory['cargo_version']
            binary=(dist_dir()/(name+'.bin')).read_bytes()
            assert binary[2:2+binary[1]].decode('mac_roman')==name
            assert binary[65:69]==b'APPL' and binary[69:73]==manifest['creator'].encode('mac_roman')
            flags=(binary[73]<<8)|binary[101]
            assert flags&0x2000, f'application icon bundle metadata missing: {name}'

        reports.append({'package':name,'fork':describe(path),'fork_sha256':sha(blob),'total_resources':len(packed),
            'original_visual_resources_verified':len(package['assets']),'font_resources_verified':len(fonts['assets'])if name=='System'else 0,
            'checked_payloads':checked})
    assert ('PICT',24000) in forks['QuickDraw Test'], 'PICT exercise belongs to QuickDraw Test'
    assert ('PNG ',128) in forks['QuickDraw Test'], 'PNG exercise belongs to QuickDraw Test'
    assert ('WIND',2003) in forks['Text Edit'], 'editor layout belongs to Text Edit'
    assert ('WIND',2003) in forks['Cursor Test'], 'cursor window layout belongs to Cursor Test'
    assert ('WIND',2003) in forks['Panic Test'], 'panic window layout belongs to Panic Test'
    for key in [('PICT',24000),('PNG ',128),('WIND',2003),('MENU',1257)]:
        assert key not in forks['Finder'], f'embedded development resource remains in Finder: {key}'
    report={'status':'passed','method':'Read actual generated System/Finder and native app resource forks; verify every imported visual/font payload and attributes, all FOND association targets, and separate application assets/commands/ABI/code/version/metadata',
            'font_families':fonts['families'],'true_type_faces':fonts['scalable_faces'],'packages':reports,
            'resource_api_tests':'System7/crates/mac-toolbox/tests plus auxiliary system/tests/resource_manager_api.rs',
            'scope':'Read-only resident System/application resource chain; disk editing and original 68k Handle ABI remain future work.'}
    path=ROOT/'results/system7/resource-manager-checks.json';path.write_text(json.dumps(report,indent=2)+'\n')
    print(f'PASS: {len(fonts["assets"])} font resources and {sum(len(p["assets"]) for p in visual["packages"])} visual resources preserved in packaged System/Finder; all native application forks and all FOND references verified')
if __name__=='__main__':main()
