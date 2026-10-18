#!/usr/bin/env python3
"""Replay measured master-push arrivals; fixed service time, no predicted test verdicts."""

import argparse
import json
from pathlib import Path


def replay(arrivals, duration, cancel):
    started = completed = superseded = 0
    busy = 0
    current = pending = None
    finish = 0
    for arrival in arrivals:
        # Complete work before processing a simultaneous new arrival.
        while current is not None and finish <= arrival:
            completed += 1
            busy += duration
            current = pending
            pending = None
            if current is not None:
                started += 1
                finish += duration
        if current is None:
            current = arrival
            finish = arrival + duration
            started += 1
        elif cancel:
            busy += arrival - (finish - duration)
            superseded += 1
            started += 1
            current = arrival
            finish = arrival + duration
        else:
            superseded += pending is not None
            pending = arrival
    if current is not None:
        completed += 1
        busy += duration
    if pending is not None:
        started += 1
        completed += 1
        busy += duration
    return dict(started=started, completed=completed, superseded=superseded,
                busy_minutes=round(busy / 60, 2))


def check():
    assert replay([], 10, False)['started'] == 0
    assert replay([0, 1, 2, 50], 10, True) == dict(started=4, completed=2, superseded=2, busy_minutes=0.37)
    assert replay([0, 1, 2, 50], 10, False) == dict(started=3, completed=3, superseded=1, busy_minutes=0.5)
    assert replay([0, 10], 10, True)['completed'] == 2
    assert replay([0, 1, 2], 10, False)['completed'] == 2


if __name__ == '__main__':
    check()
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('arrivals', type=Path)
    args = parser.parse_args()
    data = json.loads(args.arrivals.read_text())
    times = sorted(row[1] for row in data['arrivals'])
    for minutes in (20, 30, 60):
        print(json.dumps(dict(service_minutes=minutes,
                              cancel_in_progress=replay(times, minutes * 60, True),
                              finish_then_latest=replay(times, minutes * 60, False))))
