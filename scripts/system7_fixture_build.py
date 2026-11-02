"""Share fixture intermediates, keeping each fixture's final artifacts private."""
import os
import json
import shutil
import subprocess
from pathlib import Path

from system7_paths import BUILD_DIR_ENV, SOURCE, build_dir, host_dir
from system7_platform import TARGET, guest_env as platform_guest_env

ARTIFACT_DIR_ENV = 'SYSTEM7_CARGO_ARTIFACT_DIR'


def _environment(target, env, guest):
    root = build_dir()
    target = Path(target).resolve()
    fixtures = root / 'fixtures'
    if target == fixtures or not target.is_relative_to(fixtures):
        raise ValueError(f'Fixture target {target} must be a child of {fixtures}')
    env[BUILD_DIR_ENV] = str(root)
    env['CARGO_TARGET_DIR'] = str(target)
    env['CARGO_BUILD_BUILD_DIR'] = str(root / 'fixture-cache')
    profile = target / TARGET / 'release' if guest else target / 'debug'
    env[ARTIFACT_DIR_ENV] = str(profile)
    return env


def guest_env(target):
    """Keep the selected optimized guest profile/ISA and trace configuration."""
    return _environment(target, platform_guest_env(), True)


def host_env(target):
    """Use the same intermediate cache for a fixture's host packager."""
    return _environment(target, dict(os.environ), False)


def build_application(manifest, output, features=()):
    """Build a real application through the same std SDK as installed apps.

    Low-level provider/definition fixtures keep guest_env. This only selects
    the genuine compiler/packager and copies its named package; it supplies no
    guest service or alternate startup implementation.
    """
    root = build_dir()
    command = [os.environ.get('CARGO', 'cargo'), 'run', '--locked',
               '--manifest-path', str(SOURCE / 'Cargo.toml'), '--package', 'xtask',
               '--target-dir', str(host_dir()), '--', 'sdk', 'app', str(manifest)]
    if features:
        command += ['--features', ','.join(features)]
    command += ['--build-dir', str(root)]
    env = dict(os.environ)
    env.pop('CARGO_BUILD_BUILD_DIR', None)
    subprocess.run(command, cwd=SOURCE, env=env, check=True)
    receipt = json.loads((root / 'sdk/application.json').read_text())
    if receipt['state'] != 'packaged-not-executed' or receipt['returncode'] != 0:
        raise RuntimeError('SDK did not produce a packaged application')
    source = Path(receipt['output'])
    output = Path(output)
    output.mkdir(parents=True, exist_ok=True)
    name = receipt['package']['name']
    for suffix in ('elf', 'runtime.elf', 'data', 'rsrc', 'bin', 'json', 'sdk.json'):
        shutil.copyfile(source / f'{name}.{suffix}', output / f'{name}.{suffix}')
    return receipt
