"""Owned private capture processes and conservative generated-media disposal."""
import json
from pathlib import Path
import subprocess
import sys
from unittest.mock import patch

import system7_vm as vm

DISK_SUFFIXES = frozenset(('.raw', '.qcow2', '.qcow', '.hfs', '.hfsb', '.img'))


def stop_owned(process, endpoint=None):
    if process.poll() is None and endpoint is not None:
        try:
            vm.qmp('quit', endpoint=endpoint)
        except (OSError, ValueError, RuntimeError):
            pass
        # Quit may close QMP before its reply arrives. Give that owned child
        # time to finish gracefully before resorting to process termination.
        try:
            process.wait(timeout=2)
            return
        except subprocess.TimeoutExpired:
            pass
    if process.poll() is None:
        process.terminate()
    try:
        process.wait(timeout=10)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait(timeout=10)


class OwnedProcesses:
    """Retain exact children, including when a launcher raises before return.

    A monitor is usable only after that launch returns its private endpoint.
    Failure before then uses only the owned process handle. Cleanup never tries
    the regular desktop's default monitor or discovers processes by numeric PID.
    """
    def __init__(self):
        self.records = []

    def add(self, process, endpoint=None):
        if not any(record['process'] is process for record in self.records):
            self.records.append({'process': process, 'endpoint': endpoint})
        return process

    def launch(self, *args, **options):
        original = subprocess.Popen
        def spawn(*arguments, **keywords):
            return self.add(original(*arguments, **keywords))
        with patch.object(vm.subprocess, 'Popen', side_effect=spawn):
            process, endpoint = vm.launch(*args, **options)
        record = next(record for record in self.records if record['process'] is process)
        record['endpoint'] = endpoint
        return process, endpoint

    def cleanup(self, output, media_directories):
        """Attempt all teardown; retain every media file if any owner is live.

        The caller invokes this from finally. A simultaneous capture failure is
        preserved together with all cleanup causes, rather than replaced by the
        first teardown error. Reports, packages and arbitrary .bin files remain.
        """
        primary = sys.exc_info()[1]
        errors = []
        owners = []
        for record in self.records:
            process = record['process']
            try:
                stop_owned(process, record['endpoint'])
            except Exception as error:
                errors.append(error)
            if process.stdout is not None:
                try:
                    process.stdout.close()
                except Exception as error:
                    errors.append(error)
            try:
                code = process.poll()
                owners.append({'pid': process.pid, 'reaped': code is not None, 'exit_code': code})
            except Exception as error:
                errors.append(error)
                owners.append({'pid': process.pid, 'reaped': False, 'exit_code': None})
        media = sorted({path for folder in media_directories for path in Path(folder).rglob('*')
                        if path.is_file() and (path.suffix in DISK_SUFFIXES or path.name == 'bootrom.bin')})
        report = {'owners': owners, 'media': [str(path) for path in media]}
        if all(owner['reaped'] for owner in owners):
            for path in media:
                try:
                    path.unlink()
                except OSError as error:
                    errors.append(error)
            report['quarantined'] = False
        else:
            report['quarantined'] = True
            errors.append(RuntimeError('Private capture media quarantined: an owned process could '
                                       'not be positively reaped; retain these exact disks and '
                                       'owner state until that process exits'))
        report['errors'] = [str(error) for error in errors]
        try:
            (Path(output) / 'cleanup.json').write_text(json.dumps(report, indent=2) + '\n')
        except OSError as error:
            errors.append(error)
        if errors:
            raise BaseExceptionGroup('Private capture cleanup failed; inspect cleanup.json and '
                                     'retained owner state', ([primary] if primary is not None else []) + errors)
        return report
