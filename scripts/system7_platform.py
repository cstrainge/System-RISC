"""Build/VM platform selection shared with mac-build; no app policy lives here."""
import json
import os
from pathlib import Path
import shlex

NAME = os.environ.get('SYSTEM7_PLATFORM', 'QemuRISC-V')
if not NAME or not all(c.isascii() and (c.isalnum() or c in '-_') for c in NAME):
    raise ValueError('Invalid platform name: expected ASCII letters, digits, - or _')
DEFINITION = Path(__file__).resolve().parents[1] / 'platforms' / (NAME + '.json')
if not DEFINITION.is_file():
    raise ValueError(f"Unknown platform {NAME!r}; known platform: QemuRISC-V")
PLATFORM = json.loads(DEFINITION.read_text())
if PLATFORM['name'] != NAME:
    raise ValueError(f"Platform {NAME!r} declares a different name")
TARGET = PLATFORM['rust_target']
CPU = PLATFORM['qemu_cpu']

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
    flags.extend(['-C', 'target-feature=+' + PLATFORM['llvm_profile']])
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
