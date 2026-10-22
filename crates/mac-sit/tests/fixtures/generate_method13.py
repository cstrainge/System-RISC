#!/usr/bin/env python3
"""Project-owned method-13 format vectors; encoder tokens are independent of the decoder."""
from pathlib import Path
import argparse
import hashlib
import re
import struct
import subprocess
import tempfile
from generate_sit5 import crc16


def tables():
    source = (Path(__file__).resolve().parents[2] / 'src/decode/lz_huffman_tables.rs').read_text()
    def values(name, following):
        part = source[source.index('const ' + name):source.index('const ' + following)].split('=',1)[1]
        return [[int(x) for x in re.findall(r'-?\d+', block)] for block in re.findall(r'&\[(.*?)\]', part, re.S)]
    first = values('FIRST_CODE_LENGTHS', 'SECOND_CODE_LENGTHS')
    second = values('SECOND_CODE_LENGTHS', 'OFFSET_CODE_LENGTHS')
    part = source[source.index('const OFFSET_CODE_LENGTHS'):].split('=',1)[1]
    offsets = [[int(x) for x in re.findall(r'-?\d+', block)] for block in re.findall(r'&\[(.*?)\]', part, re.S)]
    def flat(name):
        part = source[source.index('const ' + name):].split('=', 1)[1].split('[', 1)[1].split('];', 1)[0]
        return [int(x, 0) for x in re.findall(r'0x[0-9a-fA-F]+|\d+', part)]
    return first, second, offsets, flat('META_CODES'), flat('META_CODE_LENGTHS')


class Bits:
    def __init__(self): self.bits = []
    def write(self, value, count): self.bits.extend((value >> i) & 1 for i in range(count))
    def finish(self):
        return bytes(sum(bit << j for j, bit in enumerate(self.bits[i:i+8])) for i in range(0, len(self.bits), 8))


def canonical(lengths):
    result = {}
    code = 0
    for length in range(1, 33):
        for symbol, bits in enumerate(lengths):
            if bits != length: continue
            if code >= 1 << length: raise ValueError('overfull fixture codebook')
            reversed_code = int(f'{code:0{length}b}'[::-1], 2)
            result[symbol] = (reversed_code, length)
            code += 1
        code <<= 1
    return result


def length_table(bits, count, meta, meta_lengths, varied=False):
    lengths = []
    current = 0
    def symbol(value): bits.write(meta[value], meta_lengths[value])
    def repeat(value, extra, extra_bits, n):
        symbol(value); bits.write(extra, extra_bits); lengths.extend([current] * n)
    if varied:
        symbol(8); current=9; lengths.append(current)
        symbol(32); current+=1; lengths.append(current)
        symbol(33); current-=1; lengths.append(current)
        repeat(34,1,1,2); repeat(35,0,3,3); repeat(36,0,6,11)
    else:
        current=9 if count==321 else 5
        symbol(current-1); lengths.append(current)
    while len(lengths)<count:
        # Codes 315/316 are unused by these tokens: -1 and zero lengths are
        # both absent. Restore length 9 before the three length/end symbols.
        if varied and len(lengths)==315:
            symbol(31); current=-1; lengths.append(current)
            symbol(32); current=0; lengths.append(current)
            symbol(8); current=9; lengths.append(current)
            continue
        stop=315 if varied and len(lengths)<315 else count
        remaining=stop-len(lengths)
        if remaining>=11:
            n=min(74,remaining)
            repeat(36,n-11,6,n)
        elif remaining>=3:
            n=min(10,remaining)
            repeat(35,n-3,3,n)
        else: repeat(34,remaining-1,1,remaining)
    return lengths


def compress(tokens, book, shared):
    first,second,offsets,meta,meta_lengths=tables()
    bits=Bits()
    if book:
        bits.write(book<<4,8)
        a,b,c=first[book-1],second[book-1],offsets[book-1]
    else:
        bits.write(7 | (8 if shared else 0),8)
        a=length_table(bits,321,meta,meta_lengths,varied=True)
        b=a if shared else length_table(bits,321,meta,meta_lengths)
        c=length_table(bits,17,meta,meta_lengths)
    codes=[canonical(a),canonical(b),canonical(c)]
    second_code=False
    output=bytearray()
    def word(table,symbol): bits.write(*codes[table][symbol])
    for token in tokens:
        table=int(second_code)
        if isinstance(token,int):
            word(table,token);output.append(token);second_code=False
        else:
            length,distance=token
            if length<=64: word(table,length+253)
            elif length<=1088: word(table,318);bits.write(length-65,10)
            else: word(table,319);bits.write(length-65,15)
            count=(distance-1).bit_length()
            word(2,count)
            if count>1: bits.write(distance-1-(1<<(count-1)),count-1)
            for _ in range(length): output.append(output[-distance] if distance<=len(output) else 0)
            second_code=True
    word(int(second_code),320)
    return bytes(output),bits.finish()


def archive(data,resource,dp,rp,version,bad=False):
    name=b'Method13'
    if version==2:
        entry=bytearray(112);entry[:2]=bytes([13,13]);entry[2]=len(name);entry[3:3+len(name)]=name
        entry[66:74]=b'TEXTTEST';struct.pack_into('>II',entry,76,3875171696,3875171700)
        struct.pack_into('>IIIIHH',entry,84,len(resource),len(data),len(rp),len(dp),crc16(resource),crc16(data)^int(bad))
        struct.pack_into('>H',entry,110,crc16(entry[:110]))
        header=bytearray(22);header[:4]=b'SIT2';struct.pack_into('>HI',header,4,1,22+len(entry)+len(rp)+len(dp));header[10:14]=b'rLau';header[14]=2;struct.pack_into('>I',header,16,22)
        return header+entry+rp+dp
    header=bytearray(114);header[:80]=b'StuffIt (c)1997-2026 Aladdin Systems, Inc., http://www.aladdinsys.com/StuffIt/\r\n';header[82:84]=bytes([5,0x10]);struct.pack_into('>H',header,92,1);struct.pack_into('>I',header,94,114)
    entry=bytearray(48);entry[:4]=b'\xa5'*4;entry[4]=1;struct.pack_into('>H',entry,30,len(name));struct.pack_into('>IIH',entry,34,len(data),len(dp),crc16(data)^int(bad));entry[46]=13;entry.extend(name);struct.pack_into('>H',entry,6,len(entry));struct.pack_into('>H',entry,32,crc16(entry))
    metadata=bytearray(36);metadata[1]=1;metadata[4:12]=b'TEXTTEST'
    resource_record=struct.pack('>IIHHBB',len(resource),len(rp),crc16(resource),0,13,0)
    body=entry+metadata+resource_record+rp+dp;struct.pack_into('>I',header,84,len(header)+len(body));struct.pack_into('>H',header,98,crc16(header));return header+body


def generate(reference=None):
    out=Path(__file__).resolve().parent
    data_tokens=[(3,1)]+list(range(256))+[(1792,256),0x90,ord('X'),(96,1),(3,2),ord('!')]
    resource_tokens=list(b'Resource fork\r')+[(400,14),0,0x90]
    for book,shared in [(book,False) for book in range(1,6)]+[(0,True),(0,False)]:
        data,dp=compress(data_tokens,book,shared);resource,rp=compress(resource_tokens,book,shared)
        stem=f'method13-book{book}' if book else 'method13-shared' if shared else 'method13-separate'
        for version in [2,5]:
            result=archive(data,resource,dp,rp,version)
            (out/f'{stem}-v{version}.sit').write_bytes(result)
        if book==0 and not shared:
            (out/'method13-bad-crc-v5.sit').write_bytes(archive(data,resource,dp,rp,5,True))
        print(stem,len(data),len(resource),len(dp),len(rp))
    # More than one complete history window, all three length forms, and the
    # largest representable distance. Every output byte remains index modulo 256.
    tokens=list(range(256))+[(32832,256),(32832,256),(64,65536),(65,65536),(1088,65536),(32832,65536)]
    data,dp=compress(tokens,0,True)
    resource,rp=compress(resource_tokens,0,True)
    (out/'method13-window-v5.sit').write_bytes(archive(data,resource,dp,rp,5))
    assert data==bytes(i&255 for i in range(99969))
    if reference is not None:
        source=reference.read_text()
        expected='134b16dc9daf48ff261d4be4cbc231e669a87e16d7f05ce87ba0a8d8a5fe1bad'
        if hashlib.sha256(source.encode()).hexdigest()!=expected:
            raise ValueError('Expected published stuffit 0.4.0 src/lib.rs; SHA256 mismatch')
        encoder=source[source.index('struct BitWriter {'):source.index('// --- StuffIt 13 Implementation ---')]
        constants=source[source.index('const META_CODES:'):source.index('#[cfg(test)]')]
        main='''
fn main(){let a:Vec<_>=std::env::args().collect();let d=std::fs::read(&a[1]).unwrap();std::fs::write(&a[2],compress_sit13(&d)).unwrap();}
'''
        with tempfile.TemporaryDirectory(prefix='system-risc-method13-') as directory:
            temporary=Path(directory)
            rust=temporary/'writer.rs'
            rust.write_text(encoder+constants+main)
            subprocess.run(['rustc','--edition=2021','-O',str(rust),'-o',str(temporary/'writer')],check=True)
            data,_=compress(data_tokens,1,False)
            resource,_=compress(resource_tokens,1,False)
            forks=[]
            for raw in [data,resource]:
                (temporary/'input').write_bytes(raw)
                subprocess.run([str(temporary/'writer'),str(temporary/'input'),str(temporary/'packed')],check=True)
                forks.append((temporary/'packed').read_bytes())
            (out/'method13-independent-v5.sit').write_bytes(archive(data,resource,*forks,5))


if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--reference',type=Path,help='published stuffit 0.4.0 src/lib.rs for independent-writer vector')
    generate(parser.parse_args().reference)
