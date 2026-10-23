#!/usr/bin/env python3
"""Verify application/resource dependencies and resident Toolbox ownership."""
import argparse
import hashlib
import json
import os
import re
import shutil
from pathlib import Path
from system7_paths import build_dir
import subprocess

# The checkout may have any name and the caller may have any working directory.
WORKSPACE = Path(__file__).resolve().parents[1]
ROOT = WORKSPACE.parent
TARGET = 'riscv64imac-unknown-none-elf'


def symbol_reader(image, preferred=None):
    """Find a reader that actually understands this ELF and Rust mangling.

    A bare GNU nm is enough when its BFD supports RV64. LLVM's executable can
    also come from PATH, a versioned distro package, or Rust's own llvm-tools
    component; installing a global unversioned llvm-nm is not required.
    """
    candidates = []
    if preferred:
        candidates.append(str(preferred))
    else:
        for name in ['llvm-nm', 'riscv64-unknown-elf-nm', 'riscv64-linux-gnu-nm', 'nm']:
            if tool := shutil.which(name):
                candidates.append(tool)
        for directory in os.get_exec_path():
            candidates.extend(str(path) for path in sorted(Path(directory).glob('llvm-nm-*'))
                              if path.name.removeprefix('llvm-nm-').isdigit())
        try:
            sysroot = subprocess.check_output(
                [os.environ.get('RUSTC', 'rustc'), '--print', 'sysroot'],
                cwd=WORKSPACE, text=True, stderr=subprocess.PIPE).strip()
            candidates.extend(str(path) for pattern in ['*/bin/llvm-nm', '*/bin/llvm-nm.exe']
                              for path in (Path(sysroot) / 'lib/rustlib').glob(pattern))
        except (OSError, subprocess.CalledProcessError):
            pass
    errors = []
    for tool in dict.fromkeys(candidates):
        try:
            symbols = subprocess.check_output(
                [tool, '--defined-only', '--demangle', str(image)],
                text=True, stderr=subprocess.PIPE)
            if 'mac_toolbox::' not in symbols or re.search(
                    r'(?m)^\S+\s+\S\s+(?:_R[NIMCXYB][A-Za-z0-9_]*|_ZN\d.*17h[0-9a-f]{16}E)\b', symbols):
                errors.append(f'{tool}: no demangled Rust symbols (unsupported or stripped ELF)')
                continue
            return tool
        except (OSError, subprocess.CalledProcessError) as error:
            detail = getattr(error, 'stderr', None) or str(error)
            errors.append(f'{tool}: {detail.strip()}')
    detail = '\n'.join(errors)
    raise RuntimeError(
        'No compatible symbol reader found. Use --nm PATH or SYSTEM7_NM; '
        'GNU binutils with RV64 support also works. Alternatively run '
        '`rustup component add llvm-tools` for the active toolchain. '
        'The check cannot skip compiled-symbol validation.' + ('\n' + detail if detail else ''))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--nm', default=os.environ.get('SYSTEM7_NM'), metavar='PATH',
                        help='Rust-demangling ELF symbol reader (otherwise discovered automatically)')
    parser.add_argument('--release-dir', type=Path,
                        default=build_dir() / 'target' / TARGET / 'release',
                        help='guest release binaries produced by cargo xtask build')
    parser.add_argument('--output', type=Path,
                        default=ROOT / 'results/system7/toolbox-boundaries.json')
    args = parser.parse_args()
    try:
        check(args)
    except subprocess.CalledProcessError as error:
        parser.exit(1, f'error: command failed: {error.cmd}\n{error.stderr or ""}\n')
    except (OSError, RuntimeError, AssertionError) as error:
        parser.exit(1, f'error: {error}\n')


def check(args):
    metadata = json.loads(subprocess.check_output(
        [os.environ.get('CARGO', 'cargo'), 'metadata', '--format-version', '1', '--locked', '--filter-platform', TARGET],
        cwd=WORKSPACE, text=True, stderr=subprocess.PIPE))
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
                [os.environ.get('CARGO', 'cargo'),'tree','--locked','--target',TARGET,'--package',name,
                 '--edges','normal','--prefix','none','--format','{p}','--no-dedupe'],
                cwd=WORKSPACE,text=True,stderr=subprocess.PIPE)
            dependency_cache[identity] = sorted({line.split()[0] for line in tree.splitlines()[1:] if line.strip()})
        return dependency_cache[identity]

    assert not normal_dependencies(by_name['mac-abi']), 'ABI must be independent of implementations'
    # Decoded keyboard payloads use the shared allocation-free character table.
    # This wire-text codec contains no service state or resident implementation.
    assert not normal_dependencies(by_name['mac-text']), 'wire text codec must remain independent'
    for name in [p['name'] for p in packages.values() if p['name'].startswith('toolbox-')]:
        # The drag SDK's bounded app-local Waker registry needs synchronization,
        # not a resident service implementation. Events decodes its readiness
        # notifications. Keep these pure helper dependencies explicitly scoped.
        dependencies = normal_dependencies(by_name[name])
        helpers = {'spinning_top', 'lock_api', 'scopeguard'} if name == 'toolbox-drag' or 'toolbox-drag' in dependencies else set()
        assert all(dep in ('mac-abi', 'mac-traps', 'mac-text') or dep in helpers or dep.startswith('toolbox-')
                   for dep in dependencies), (name, 'client depends on backend')
    assert not any(dep.startswith('toolbox-') for dep in normal_dependencies(by_name['mac-system'])), \
        'System must implement managers internally, without depending on public client bindings'

    for name in ['mac-finder', 'quickdraw-test', 'text-edit', 'cursor-test', 'panic-test', 'resedit']:
        direct = sorted(packages[dep['pkg']]['name'] for dep in nodes[by_name[name]]['deps']
                        if any(kind['kind'] is None for kind in dep['dep_kinds']))
        # Pure application helpers do not provide System services. Keep their
        # reviewed allowlist explicit, just like ResEdit's instruction decoder.
        # Finder's synchronized BSS snapshots and Unicode caption boundaries
        # contain no resident manager implementation or private kernel calls.
        helpers = {
            'resedit': ['disc-v', 'mac-code-resource', 'mac-text'],
            'mac-finder': ['spinning_top', 'unicode-segmentation'],
            # Versioned font/view preferences only; all storage and UI still
            # go through mac-toolbox. Neither codec implements OS services.
            'text-edit': ['serde', 'serde-json-core'],
        }
        expected = sorted(['mac-toolbox', *helpers.get(name, [])])
        assert direct == expected, (name, 'services must use aggregate Toolbox; helper dependencies require review', direct)
        manifest = Path(packages[by_name[name]]['manifest_path'])
        source = (manifest.parent / 'src/main.rs').read_text()
        assert 'mac_toolbox::panic_handler!();' in source, (name, 'must use the shared panic handler')
        assert 'mac_toolbox::panic::install(services)' in source, (name, 'must install panic services')

    fonts = [(WORKSPACE / 'system/src/toolbox/quickdraw/assets' / name).read_bytes()
             for name in ['chicago-12.font', 'geneva-9.font']]
    reports = []
    release = args.release_dir.resolve()
    binaries = [
        ('mac-finder', 'Finder'),
        ('quickdraw-test', 'QuickDrawTest'),
        ('text-edit', 'TextEdit'),
        ('cursor-test', 'CursorTest'),
        ('panic-test', 'PanicTest'),
        ('resedit', 'ResEdit'),
        ('native-wdef', 'WDEF'),
        ('native-buttons-cdef', 'CDEF_BUTTONS'),
        ('native-scrollbar-cdef', 'CDEF_SCROLL_BAR'),
        ('native-progress-bar-cdef', 'CDEF_PROGRESS_BAR'),
        ('native-static-text-cdef', 'CDEF_STATIC_TEXT'),
        ('native-mdef', 'MDEF'),
    ]
    missing = [str(release / binary) for binary in ['System'] + [b for _, b in binaries]
               if not (release / binary).is_file()]
    if missing:
        raise RuntimeError('Guest binaries are missing. Run `cargo xtask build` from '
                           f'{WORKSPACE}, or supply --release-dir. Missing: ' + ', '.join(missing))
    nm = symbol_reader(release / 'Finder', args.nm)
    for package, binary in binaries:
        dependencies = normal_dependencies(by_name[package])
        assert 'mac-system' not in dependencies, (package, 'links resident implementation')
        assert 'linked_list_allocator' not in dependencies, (package, 'links resident heap')
        image = release / binary
        symbols = subprocess.check_output([nm, '--defined-only', '--demangle', str(image)], text=True)
        assert symbols.strip(), (binary, 'symbol table missing; rebuild without stripping')
        assert 'mac_system::toolbox' not in symbols, (binary, 'contains resident manager symbols')
        data = image.read_bytes()
        assert all(font not in data for font in fonts), (binary, 'contains resident font strike')
        reports.append({'package': package, 'binary': binary, 'normal_dependencies': dependencies,
                        'resident_code_symbols': 0, 'resident_font_strikes': 0,
                        'elf_bytes': len(data), 'sha256': hashlib.sha256(data).hexdigest()})
    system_symbols = subprocess.check_output(
        [nm, '--defined-only', '--demangle', str(release / 'System')], text=True)
    assert 'mac_system::toolbox' in system_symbols, 'System symbols missing; rebuild without stripping'
    assert 'mac_formats::elf::' not in system_symbols, 'System must not contain an ELF loader'
    assert not normal_dependencies(by_name['mac-code-resource']), 'wire codec must remain allocation-free and independent of services'
    assert 'standard_window_definition' not in system_symbols, 'WDEF implementation belongs to the resource'
    assert 'standard_control_definition' not in system_symbols, 'CDEF implementation belongs to the resource'
    assert 'standard_menu_definition' not in system_symbols, 'MDEF implementation belongs to the resource'
    report = {'status': 'passed', 'abi_has_no_dependencies': True,
              'system_has_no_client_dependencies': True, 'definition_code_resident_in_resources': True,
              'workspace': str(WORKSPACE), 'symbol_reader': nm,
              'applications': reports}
    path = args.output.resolve()
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(report, indent=2) + '\n')
    print('PASS: thin Toolbox clients; resident managers; independent WDEF/CDEF code and no copied fonts')


if __name__ == '__main__':
    main()
