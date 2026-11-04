"""Build/VM platform selection shared with mac-build; no app policy lives here."""
import json
import os
from pathlib import Path
import shlex

NAME = os.environ.get('SR_PLATFORM', 'qemu_riscv')
if not NAME or not all(c.isascii() and (c.isalnum() or c in '-_') for c in NAME):
    raise ValueError('Invalid platform name: expected ASCII letters, digits, - or _')
DEFINITION = Path(__file__).resolve().parents[1] / 'platforms' / (NAME + '.json')
if not DEFINITION.is_file():
    raise ValueError(f"Unknown platform {NAME!r}; known platform: qemu_riscv")


def _merge(included, overlay):
    """Compose one record over another exactly as the build tool does.

    Scalars and plain arrays replace what they overlay, objects merge key by
    key, and `{"$append": [...]}` extends an inherited array. A record that
    layers on a base platform therefore reads the same through Python as
    through `cargo xtask`.
    """
    if isinstance(included, dict) and isinstance(overlay, dict):
        merged = dict(included)
        for key, value in overlay.items():
            if isinstance(value, dict) and set(value) == {'$append'}:
                merged[key] = list(included.get(key, [])) + list(value['$append'])
            else:
                merged[key] = _merge(included.get(key), value)
        return merged
    return overlay


def _load(path, seen=()):
    if path in seen:
        chain = ' -> '.join(item.name for item in (*seen, path))
        raise ValueError(f'Platform include cycle: {chain}')
    record = json.loads(path.read_text())
    parent = record.pop('include', None)
    if parent is None:
        return record
    return _merge(_load(path.parent / parent, (*seen, path)), record)


PLATFORM = _load(DEFINITION)
if PLATFORM['name'] != NAME:
    raise ValueError(f"Platform {NAME!r} declares a different name")
EMULATION = PLATFORM['emulation']
TARGET = PLATFORM['rust_target']
CPU = EMULATION.get('cpu')
if not CPU:
    raise ValueError('Platform emulation.cpu must name the CPU model the machine is given')
RAM = EMULATION['ram']
HARTS = EMULATION['harts']
if type(HARTS) is not int or HARTS < 1:
    raise ValueError('Platform emulation.harts must be a positive integer')

def cpu_model(vector_bits=None):
    if vector_bits is None:
        return CPU
    # Architectural RVV bounds, not a kernel allocation quota.
    if type(vector_bits) is not int or not 128 <= vector_bits <= 65536 or vector_bits & (vector_bits - 1):
        raise ValueError('RVA23 vector length must be a power of two from 128 to 65536 bits')
    return CPU + ',vlen=' + str(vector_bits)

def guest_env():
    env = dict(os.environ)
    flags = (env['CARGO_ENCODED_RUSTFLAGS'].split('\x1f') if 'CARGO_ENCODED_RUSTFLAGS' in env
             else shlex.split(env.get('RUSTFLAGS', '')))
    for index, flag in enumerate(flags):
        option = (flags[index + 1] if index + 1 < len(flags) else '') if flag in ('-C', '--codegen') else (flag[2:] if flag.startswith('-C') else flag.removeprefix('--codegen='))
        if option.startswith('relocation-model=') and option != 'relocation-model=pic':
            raise ValueError('Native dependencies require relocation-model=pic; remove the conflicting override and rebuild')
    flags.extend(['-C', 'target-feature=+' + PLATFORM['llvm_profile'], '-C', 'relocation-model=pic'])
    env['CARGO_ENCODED_RUSTFLAGS'] = '\x1f'.join(flags)
    return env


def display_size():
    record = PLATFORM['memory']['display']
    width, height = record['width'], record['height']
    if type(width) is not int or type(height) is not int or not 0 < width <= 32767 or not 21 < height <= 32767:
        raise ValueError('Platform display must be positive, taller than 21 and fit signed 16-bit geometry')
    return width, height


def tablet_axis(pixel, axis):
    extent = display_size()[{'x': 0, 'y': 1}[axis]]
    if not 0 <= pixel < extent:
        raise ValueError(f'Tablet {axis} pixel {pixel} is outside the configured {extent}-pixel display')
    return 0 if extent == 1 else (pixel * 32767 + extent - 2) // (extent - 1)
