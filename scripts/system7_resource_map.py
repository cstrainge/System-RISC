"""Resource metadata transport; the shared Rust codec validates both formats."""
import functools
import json
from pathlib import Path
import subprocess
import tempfile

REPO = Path(__file__).resolve().parents[1]


@functools.lru_cache(maxsize=8)
def resource_map(blob):
    with tempfile.TemporaryDirectory(prefix='system7-resource-map-') as temporary:
        path = Path(temporary) / 'fork.rsrc'
        path.write_bytes(blob)
        run = subprocess.run(['cargo', 'xtask', 'resource-map', str(path)], cwd=REPO,
                             capture_output=True, text=True)
        if run.returncode:
            raise RuntimeError('Resource metadata inspection failed:\n' + run.stderr.strip())
    return json.loads(run.stdout)


def resources(blob):
    for entry in resource_map(blob)['resources']:
        start, length = entry['payload_offset'], entry['length']
        yield (bytes(entry['kind']), entry['id'], entry['attributes'], start,
               blob[start:start + length])


def resource_names(blob):
    return {(bytes(entry['kind']).decode('mac_roman'), entry['id']):
            bytes(entry['name']).decode('mac_roman')
            for entry in resource_map(blob)['resources'] if entry['name'] is not None}
