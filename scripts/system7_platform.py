"""Build/VM platform selection shared with mac-build; no app policy lives here."""
import json
import os
from pathlib import Path
import shlex

PLATFORM = json.loads((Path(__file__).resolve().parents[1] / 'platforms/riscv64.json').read_text())
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
