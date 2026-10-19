#!/usr/bin/env python3
"""Summarize measured trace work; canonical Rust tests decide acceptance."""
import argparse
from collections import Counter, defaultdict, deque
import json
from pathlib import Path


def summarize(stage):
    records = stage['records']
    delivered = Counter(str(r[4]) for r in records if r[2] in (2, 4))
    keys = [r for r in records if r[2] == 2 and r[4] == 3]
    input_keys = [r for r in records if r[2] == 1 and r[4] == 3 and r[6] == 1]
    renders = [r for r in records if r[2] == 7]
    last_key_frame = next((r for r in renders if keys and r[0] > keys[-1][0]), None)
    pending = defaultdict(deque)
    key_delays = []
    for row in records:
        if row[2] == 1 and row[4] == 3 and row[5] >> 8 != 255:
            kind = 4 if row[6] == 0 else (5 if row[6] & 2 else 3)
            pending[(row[3], kind, row[5])].append(row[1])
        elif row[2] == 2 and row[4] in (3, 4, 5):
            ingress = pending[(row[3], row[4], row[5])]
            if ingress:
                key_delays.append((row[1] - ingress.popleft()) / 10)
    ordered = sorted(key_delays)
    delays = {name: ordered[min(len(ordered) - 1, (len(ordered) * pct + 99) // 100 - 1)]
              for name, pct in [('p50', 50), ('p95', 95), ('p99', 99), ('max', 100)]} if ordered else {}
    return {'frames': sum(c.get('render', 0) for c in stage['counters'].values()),
            'window_commits': sum(r[2] == 5 and r[4] == 6 for r in records),
            'deliveries_by_kind': dict(delivered),
            'service_calls': sum(v[0] for rows in stage['services'].values() for v in rows.values()),
            'service_seconds': sum(v[1] for rows in stage['services'].values() for v in rows.values()) / 1e7,
            'render_seconds': stage['render_seconds'],
            'frames_while_update_pending': stage['renders_during_update'],
            'first_input_to_last_key_frame_seconds':
                (last_key_frame[1] - input_keys[0][1]) / 1e7 if last_key_frame and input_keys else None,
            'action_wall_seconds': stage.get('action_seconds'),
            'phase_wall_seconds': stage['seconds'], 'overwritten': stage['overwritten'],
            'decoded_key_to_app_microseconds': delays, 'decoded_key_samples': len(key_delays),
            'max_masked_service_microseconds': max((r[5] / 10 for r in records if r[2] == 15), default=None),
            'frame_copy_microseconds': [r[4] / 10 for r in records if r[2] == 14]}


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--baseline', required=True, type=Path)
    parser.add_argument('--candidate', required=True, type=Path)
    parser.add_argument('--output', required=True, type=Path)
    args = parser.parse_args()
    before = json.loads((args.baseline / 'capture.json').read_text())
    after = json.loads((args.candidate / 'capture.json').read_text())
    baseline = {s['name']: s for s in before['stages']}
    report = {'baseline': str(args.baseline.resolve()), 'candidate': str(args.candidate.resolve()),
              'scope': 'Work counts and measured durations, not pass/fail or universal latency guarantees.',
              'limitations': ['Trace feature adds clock/counter overhead.',
                              'Candidate capture waits for quiet records; phase wall durations differ.',
                              'Frames during updates may legitimately paint the prior committed scene.',
                              'Background caret blinks and scheduling vary total frame counts.'],
              'stages': [{'name': s['name'], 'before': summarize(baseline[s['name']]),
                          'after': summarize(s)} for s in after['stages'] if s['name'] in baseline]}
    args.output.write_text(json.dumps(report, indent=2) + '\n')
