"""Fixture cache sharing cannot redirect the final package or another build."""
import os
from pathlib import Path
import sys
from tempfile import TemporaryDirectory
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import system7_fixture_build as fixture
from system7_platform import TARGET


class FixtureBuildTests(unittest.TestCase):
    def test_guest_fixtures_share_intermediates_but_keep_distinct_final_outputs(self):
        with TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            incoming = {'SYSTEM7_BUILD_DIR': str(root),
                        'CARGO_BUILD_BUILD_DIR': str(root / 'wrong-cache'),
                        'SYSTEM7_CARGO_ARTIFACT_DIR': str(root / 'wrong-output'),
                        'CARGO_ENCODED_RUSTFLAGS': '-C\x1fdebuginfo=0',
                        'SYSTEM7_EVENT_TRACE': '1',
                        'CARGO_PROFILE_DEV_DEBUG': '0'}
            with patch.dict(os.environ, incoming, clear=True):
                one = fixture.guest_env(root / 'fixtures/one')
                two = fixture.guest_env(root / 'fixtures/two')
                self.assertEqual(dict(os.environ), incoming)
            self.assertEqual(one['CARGO_BUILD_BUILD_DIR'], str(root / 'fixture-cache'))
            self.assertEqual(one['CARGO_BUILD_BUILD_DIR'], two['CARGO_BUILD_BUILD_DIR'])
            self.assertEqual(one['CARGO_TARGET_DIR'], str(root / 'fixtures/one'))
            self.assertEqual(one[fixture.ARTIFACT_DIR_ENV],
                             str(root / 'fixtures/one' / TARGET / 'release'))
            self.assertNotEqual(one[fixture.ARTIFACT_DIR_ENV], two[fixture.ARTIFACT_DIR_ENV])
            self.assertEqual(one['CARGO_ENCODED_RUSTFLAGS'].split('\x1f'),
                             ['-C', 'debuginfo=0', '-C', 'target-feature=+rva23u64'])
            self.assertEqual(one['SYSTEM7_EVENT_TRACE'], '1')
            self.assertEqual(one['CARGO_PROFILE_DEV_DEBUG'], '0')

    def test_host_packager_uses_its_own_final_profile_without_guest_flag_changes(self):
        with TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            incoming = {'SYSTEM7_BUILD_DIR': str(root), 'RUSTFLAGS': '-C debuginfo=0'}
            with patch.dict(os.environ, incoming, clear=True):
                env = fixture.host_env(root / 'fixtures/one')
                self.assertEqual(dict(os.environ), incoming)
            self.assertEqual(env[fixture.ARTIFACT_DIR_ENV], str(root / 'fixtures/one/debug'))
            self.assertEqual(env['CARGO_BUILD_BUILD_DIR'], str(root / 'fixture-cache'))
            self.assertEqual(env['RUSTFLAGS'], '-C debuginfo=0')
            self.assertNotIn('CARGO_ENCODED_RUSTFLAGS', env)

    def test_final_fixture_targets_cannot_escape_the_selected_build(self):
        with TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            with patch.dict(os.environ, {'SYSTEM7_BUILD_DIR': str(root)}, clear=True):
                for target in [root, root / 'fixtures', root / 'host', root / 'fixtures/../outside']:
                    with self.assertRaisesRegex(ValueError, 'must be a child of'):
                        fixture.guest_env(target)


if __name__ == '__main__':
    unittest.main()
