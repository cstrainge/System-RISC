#!/usr/bin/env python3
"""Verify application/resource dependencies and resident Toolbox ownership."""
import hashlib
import json
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[2]
WORKSPACE = ROOT / 'System7'
TARGET = 'riscv64imac-unknown-none-elf'


def main():
    metadata = json.loads(subprocess.check_output(
        ['cargo', 'metadata', '--format-version', '1', '--locked', '--filter-platform', TARGET],
        cwd=WORKSPACE, text=True))
    packages = {p['id']: p for p in metadata['packages']}
    nodes = {n['id']: n for n in metadata['resolve']['nodes']}
    by_name = {p['name']: p['id'] for p in packages.values()}

    dependency_cache = {}
    def normal_dependencies(identity):
        # Metadata unifies dev features and retains inactive target dep_kinds.
        # Cargo's target-specific normal tree is the actual production graph.
        if identity not in dependency_cache:
            name = packages[identity]['name']
            tree = subprocess.check_output(
                ['cargo','tree','--locked','--target',TARGET,'--package',name,
                 '--edges','normal','--prefix','none','--format','{p}','--no-dedupe'],
                cwd=WORKSPACE,text=True)
            dependency_cache[identity] = sorted({line.split()[0] for line in tree.splitlines()[1:] if line.strip()})
        return dependency_cache[identity]

    assert not normal_dependencies(by_name['mac-abi']), 'ABI must be independent of implementations'
    for name in [p['name'] for p in packages.values() if p['name'].startswith('toolbox-')]:
        assert all(dep in ('mac-abi', 'mac-traps') or dep.startswith('toolbox-')
                   for dep in normal_dependencies(by_name[name])), (name, 'client depends on backend')
    assert not any(dep.startswith('toolbox-') for dep in normal_dependencies(by_name['mac-system'])), \
        'System must implement managers internally, without depending on public client bindings'

    for name in ['mac-finder', 'quickdraw-test', 'textedit-exerciser']:
        direct = sorted(packages[dep['pkg']]['name'] for dep in nodes[by_name[name]]['deps']
                        if any(kind['kind'] is None for kind in dep['dep_kinds']))
        assert direct == ['mac-toolbox'], (name, 'runtime must use only the aggregate Toolbox', direct)

    fonts = [(WORKSPACE / 'system/src/toolbox/quickdraw/assets' / name).read_bytes()
             for name in ['chicago-12.font', 'geneva-9.font']]
    reports = []
    release = ROOT / 'build/system7/target' / TARGET / 'release'
    for package, binary in [('mac-finder', 'Finder'), ('quickdraw-test', 'QuickDrawTest'), ('textedit-exerciser', 'TextEditExerciser'), ('native-wdef', 'WDEF'), ('native-cdef', 'CDEF'), ('native-scrollbar-cdef', 'SCROLLBAR'), ('native-mdef', 'MDEF')]:
        dependencies = normal_dependencies(by_name[package])
        assert 'mac-system' not in dependencies, (package, 'links resident implementation')
        assert 'linked_list_allocator' not in dependencies, (package, 'links resident heap')
        image = release / binary
        symbols = subprocess.check_output(['llvm-nm', '--defined-only', '--demangle', str(image)], text=True)
        assert 'mac_system::toolbox' not in symbols, (binary, 'contains resident manager symbols')
        data = image.read_bytes()
        assert all(font not in data for font in fonts), (binary, 'contains resident font strike')
        reports.append({'package': package, 'binary': binary, 'normal_dependencies': dependencies,
                        'resident_code_symbols': 0, 'resident_font_strikes': 0,
                        'elf_bytes': len(data), 'sha256': hashlib.sha256(data).hexdigest()})
    system_symbols = subprocess.check_output(
        ['llvm-nm', '--defined-only', '--demangle', str(release / 'System')], text=True)
    assert 'standard_window_definition' not in system_symbols, 'WDEF implementation belongs to the resource'
    assert 'standard_control_definition' not in system_symbols, 'CDEF implementation belongs to the resource'
    assert 'standard_menu_definition' not in system_symbols, 'MDEF implementation belongs to the resource'
    report = {'status': 'passed', 'abi_has_no_dependencies': True,
              'system_has_no_client_dependencies': True, 'definition_code_resident_in_resources': True,
              'applications': reports}
    path = ROOT / 'results/system7/toolbox-boundaries.json'
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(report, indent=2) + '\n')
    print('PASS: thin Toolbox clients; resident managers; independent WDEF/CDEF code and no copied fonts')


if __name__ == '__main__':
    main()
