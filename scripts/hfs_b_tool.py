"""Native fixture transport through the shared Linux HFS-B command-line tool.

No catalog/resource mutation algorithms live in capture scripts. This helper
only invokes the Rust host adapter on offline private images.
"""
from functools import cache
import json
from pathlib import Path
import subprocess
from system7_paths import host_dir

ROOT = Path(__file__).resolve().parents[2]


@cache
def tool():
    target = host_dir()
    subprocess.run(['cargo', 'build', '--locked', '-p', 'hfs-b', '--target-dir',
                    str(target)], cwd=ROOT / 'System7', check=True)
    return target / 'debug/hfs-b'


class Image:
    def __init__(self, image):
        self.image = Path(image)

    def run(self, command, *arguments):
        return subprocess.check_output([str(tool()), command, str(self.image),
                                        *map(str, arguments)], text=True)

    def stat(self, path):
        return json.loads(self.run('stat', path))

    def mkdir(self, path, exist_ok=False):
        parent, _, name = path.rstrip('/').rpartition('/')
        if exist_ok:
            try:
                current = self.stat(path)
            except subprocess.CalledProcessError:
                pass
            else:
                if current['directory']:
                    return
                raise ValueError('existing fixture entry is not a directory: ' + path)
        self.run('mkdir', parent or '/', name)

    def remove(self, path):
        self.run('remove', path)

    def import_macbinary(self, source, directory, name=None):
        args = [source] if name is None else [source, name]
        self.run('import', directory, *args)

    def extract(self, path, destination, fork='data'):
        self.run('extract', path, destination, fork)
