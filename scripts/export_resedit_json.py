#!/usr/bin/env python3
"""Save a completed ResEdit JSON export from a guest's serial log.

In ResEdit choose File > Export JSON to Host (Ctrl-E), then run this tool.
Until the native File Manager can create/write ordinary files, this is the
explicit host transport. It never writes the source app or a guest resource fork.
"""
import argparse
import json
from pathlib import Path


def exports(serial):
    active = {}
    complete = []
    for line in serial.splitlines():
        fields = line.split()
        if len(fields) == 2 and fields[0] == 'RESEDIT_JSON_BEGIN':
            active[fields[1]] = {'sequence': 0, 'data': bytearray()}
        elif len(fields) == 4 and fields[0] == 'RESEDIT_JSON_CHUNK':
            token, sequence, payload = fields[1:]
            if token not in active:
                continue
            state = active[token]
            try:
                if int(sequence) != state['sequence']:
                    raise ValueError('missing or reordered chunk')
                state['data'].extend(bytes.fromhex(payload))
                state['sequence'] += 1
            except ValueError:
                del active[token]
        elif len(fields) == 4 and fields[0] == 'RESEDIT_JSON_END':
            token, length, expected_hash = fields[1:]
            state = active.pop(token, None)
            if state is None:
                continue
            data = bytes(state['data'])
            checksum = 0xcbf29ce484222325
            for byte in data:
                checksum = ((checksum ^ byte) * 0x100000001b3) & 0xffffffffffffffff
            if len(data) != int(length) or checksum != int(expected_hash, 16):
                continue
            manifest = json.loads(data)
            if manifest.get('archive') is not True or not isinstance(manifest.get('resources'), list):
                continue
            complete.append((token, data, manifest))
    return complete


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--serial', type=Path, default=Path(__file__).resolve().parents[2] / 'working/system7-vm/serial.log')
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--token', help='Choose one completed export; default is the latest.')
    args = parser.parse_args()
    candidates = [item for item in exports(args.serial.read_text(errors='replace')) if args.token is None or item[0] == args.token]
    if not candidates:
        parser.error('No complete, checksum-verified export found. Choose Export JSON to Host in ResEdit first.')
    token, data, manifest = candidates[-1]
    # Exclusive creation prevents a mistyped destination replacing source work.
    with args.output.open('xb') as output:
        output.write(data)
    print(f"Saved {manifest['name']}: {len(manifest['resources'])} resources, {len(data)} JSON bytes, token {token}, to {args.output}")


if __name__ == '__main__':
    main()
