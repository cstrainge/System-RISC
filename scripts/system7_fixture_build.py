"""Share fixture intermediates, keeping each fixture's final artifacts private."""
import os
from pathlib import Path

from system7_paths import BUILD_DIR_ENV, build_dir
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
