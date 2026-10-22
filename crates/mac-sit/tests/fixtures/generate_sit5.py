#!/usr/bin/env python3
"""Build project-owned SIT5 fixtures with an independent reference compressor.

Pass stuffit-0.4.0/src/lib.rs from the published MIT/Apache-2.0 crate, plus
an output directory. Its encoder is compiled privately, never linked to System.
The published encoder omits Arsenic's CRC trailer/lookahead padding; explicit
patches below add those fields. Randomization is also enabled in one fixture.
"""
import argparse
import hashlib
import os
from pathlib import Path
import struct
import subprocess
import tempfile
import zlib


def crc16(data):
    crc = 0
    for byte in data:
        crc ^= byte
        for _ in range(8):
            crc = (crc >> 1) ^ (0xa001 if crc & 1 else 0)
    return crc


def entropy(count):
    state = 0x12345678
    data = bytearray()
    for _ in range(count):
        state ^= (state << 13) & 0xffffffff
        state ^= state >> 17
        state ^= (state << 5) & 0xffffffff
        state &= 0xffffffff
        data.append(state & 255)
    return bytes(data)


def archive(data, resource, data_packed, resource_packed):
    header = bytearray(114)
    header[:80] = b'StuffIt (c)1997-2026 Aladdin Systems, Inc., http://www.aladdinsys.com/StuffIt/\r\n'
    header[82:84] = bytes([5, 0x10])
    struct.pack_into('>H', header, 92, 1)
    struct.pack_into('>I', header, 94, 114)
    name = b'Arsenic'
    comment = b'Project-owned bounded decoder fixture'
    entry = bytearray(48)
    entry[:4] = b'\xa5' * 4
    entry[4] = 1
    struct.pack_into('>II', entry, 10, 3875171696, 3875171700)
    struct.pack_into('>H', entry, 30, len(name))
    struct.pack_into('>IIH', entry, 34, len(data), len(data_packed), 0)
    entry[46] = 15
    entry.extend(name + struct.pack('>HH', len(comment), 0) + comment)
    struct.pack_into('>H', entry, 6, len(entry))
    struct.pack_into('>H', entry, 32, crc16(entry))
    metadata = bytearray(36)
    metadata[1] = 1
    metadata[4:12] = b'TEXTTEST'
    resource_record = struct.pack('>IIHHBB', len(resource), len(resource_packed), 0, 0, 15, 0)
    body = entry + metadata + resource_record + resource_packed + data_packed
    struct.pack_into('>I', header, 84, len(header) + len(body))
    struct.pack_into('>H', header, 98, crc16(header))
    return header + body


def generate(reference, output):
    source = reference.read_text()
    # Pin the reference implementation, not whatever a registry supplies later.
    expected = '134b16dc9daf48ff261d4be4cbc231e669a87e16d7f05ce87ba0a8d8a5fe1bad'
    digest = hashlib.sha256(source.encode()).hexdigest()
    if digest != expected:
        raise ValueError(f'Expected stuffit 0.4.0 lib.rs SHA256 {expected}; got {digest}')

    def section(start, end):
        return source[source.index(start):source.index(end)]

    model = section('struct ArithmeticModel {', "struct ArithmeticDecoder<'a>") + 'const ARITH_ONE:u32=1<<25; const ARITH_HALF:u32=1<<24;\n'
    encoder = section('struct ArithmeticEncoder {', '// --- Arsenic Encoder')
    encoder = encoder.replace('        self.data\n', '        self.data.extend_from_slice(&[0;4]);\n        self.data\n')
    arsenic = section('fn arsenic_rle_encode_block(', "struct SitArsenicDecoder<'a>")
    arsenic = arsenic.replace('let block = rle_block.as_slice();', '''let mut randomized_block = rle_block.clone();
            if RANDOMIZED {
                let mut index=0;let mut at=RANDOMIZATION_TABLE[0] as usize;
                while at<randomized_block.len(){randomized_block[at]^=1;index=(index+1)&255;at+=RANDOMIZATION_TABLE[index] as usize;}
            }
            let block = randomized_block.as_slice();''')
    arsenic = arsenic.replace('// No randomization\n            self.encoder.encode_symbol(&mut initial_model, 0);',
                              '// Fixture optionally uses block randomization\n            self.encoder.encode_symbol(&mut initial_model, u16::from(RANDOMIZED));')
    arsenic = arsenic.replace('        self.encoder.finish()',
                              '        self.encoder.write_bit_string(&mut initial_model, crc32(data) ^ u32::from(std::env::var_os("SIT_FIXTURE_BAD_CRC").is_some()), 32);\n        self.encoder.write_bit_string(&mut initial_model, 0, 32);\n        self.encoder.finish()')
    table_source = (Path(__file__).resolve().parents[2] / 'src/decode/arsenic_table.rs').read_text().replace('pub(super) ', '')
    main = '''
fn crc32(data:&[u8])->u32{let mut c=u32::MAX;for b in data{c^=u32::from(*b);for _ in 0..8{c=(c>>1)^if c&1!=0{0xedb88320}else{0};}}!c}
fn main(){let a:Vec<_>=std::env::args().collect();let d=std::fs::read(&a[1]).unwrap();std::fs::write(&a[2],SitArsenicEncoder::new(15).compress(&d)).unwrap();}
'''
    output.mkdir(parents=True, exist_ok=True)
    data = entropy(32768) + b'\x90' * 1100 + b'AAAA' + bytes(range(256)) * 2
    resource = b'Resource fork\r' + bytes(range(256)) * 4
    with tempfile.TemporaryDirectory(prefix='system-risc-sit-writer-') as temporary:
        temporary = Path(temporary)
        for randomized, bad_crc in [(False, False), (True, False), (False, True)]:
            rust = temporary / 'writer.rs'
            rust.write_text(f'const RANDOMIZED:bool={str(randomized).lower()};\n' + table_source + model + encoder + arsenic + main)
            subprocess.run(['rustc', '--edition=2021', '-O', str(rust), '-o', str(temporary / 'writer')], check=True)
            forks = []
            for kind, plain in [('data', data), ('resource', resource)]:
                (temporary / kind).write_bytes(plain)
                environment = dict(os.environ)
                environment.pop('SIT_FIXTURE_BAD_CRC', None)
                if bad_crc and kind == 'data': environment['SIT_FIXTURE_BAD_CRC'] = '1'
                subprocess.run([str(temporary / 'writer'), str(temporary / kind), str(temporary / (kind + '.packed'))], check=True, env=environment)
                forks.append((temporary / (kind + '.packed')).read_bytes())
            name = 'arsenic-bad-crc-v5.sit' if bad_crc else ('arsenic-randomized-v5.sit' if randomized else 'arsenic-v5.sit')
            result = archive(data, resource, *forks)
            (output / name).write_bytes(result)
            print(name, len(result), hashlib.sha256(result).hexdigest())
    print('data', len(data), hex(zlib.crc32(data)), hashlib.sha256(data).hexdigest())
    print('resource', len(resource), hex(zlib.crc32(resource)), hashlib.sha256(resource).hexdigest())


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('reference', type=Path)
    parser.add_argument('output', type=Path)
    args = parser.parse_args()
    generate(args.reference, args.output)
