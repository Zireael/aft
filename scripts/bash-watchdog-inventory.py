#!/usr/bin/env python3
"""Read task artifacts and timestamps only; never open a live database or change storage."""
import collections
import json
import os
import pathlib
import time

base = pathlib.Path.home() / ".local/share/cortexkit/aft"
now = time.time()
counts = collections.Counter()
roots = collections.defaultdict(collections.Counter)
ages = collections.Counter()
folder_ages = collections.Counter()
old = collections.Counter()
undelivered = collections.Counter()
opencode_sessions = {}
opencode_other = {}
opens = reads = stats = 0

def is_dir(path):
    global stats
    stats += 1
    return pathlib.Path(path).is_dir()

def exists(path):
    global stats
    stats += 1
    return os.path.exists(path)
for namespace in base.iterdir():
    task_root = namespace / "bash-tasks"
    if not is_dir(task_root):
        continue
    for session in task_root.iterdir():
        if not is_dir(session):
            continue
        for folder in session.iterdir():
            if not folder.name.startswith("bash-") or not is_dir(folder):
                continue
            counts["folders"] += 1
            metadata = folder / "control/metadata.json"
            try:
                opens += 1
                with metadata.open() as stream:
                    reads += 1
                    task = json.load(stream)
                stats += 1
                modified = metadata.stat().st_mtime
                stats += 1
                folder_age = now - folder.stat().st_mtime
                folder_ages["<1h" if folder_age < 3600 else "1h-24h" if folder_age < 86400 else ">24h"] += 1
            except (OSError, ValueError):
                counts["unreadable"] += 1
                continue
            status = task.get("status", "unknown")
            counts[status] += 1
            age = now - modified
            ages["<1h" if age < 3600 else "1h-24h" if age < 86400 else ">24h"] += 1
            root = task.get("project_root") or task.get("workdir") or "unknown"
            roots[root][status] += 1
            if age >= 3600 and status not in ("running", "starting", "killing"):
                old[f"{namespace.name}:delivered={task.get('completion_delivered')}"] += 1
                if not task.get('completion_delivered'):
                    session_id = task.get('session_id', '')
                    kind = 'mason' if session_id.startswith('alfonso:bg_') else 'alfonso' if session_id.startswith('alfonso:') else 'ses_' if session_id.startswith('ses_') else 'test' if 'test' in session_id else 'other'
                    requester = (task.get('call_key') or {}).get('requester', 'unrecorded')
                    undelivered[f"storage={namespace.name};harness={task.get('harness', 'unrecorded')};session={kind};notify={task.get('notify_on_completion')};requester={requester}"] += 1
                    if namespace.name == 'opencode':
                        pattern = 'alfonso:bg_*' if session_id.startswith('alfonso:bg_') else 'alfonso:sidekick-*' if session_id.startswith('alfonso:sidekick-') else 'ses_*' if session_id.startswith('ses_') else session_id.split(':')[0] + ':*' if ':' in session_id else session_id.split('-')[0] + '-*'
                        group = opencode_sessions.setdefault(pattern, {'count': 0, 'oldest_unix_ms': task.get('finished_at') or task['started_at'], 'newest_unix_ms': 0})
                        group['count'] += 1
                        stamp = task.get('finished_at') or task['started_at']
                        group['oldest_unix_ms'] = min(group['oldest_unix_ms'], stamp)
                        group['newest_unix_ms'] = max(group['newest_unix_ms'], stamp)
                        if kind == 'other' and task.get('notify_on_completion'):
                            other = opencode_other.setdefault(pattern, {'count': 0, 'oldest_unix_ms': stamp, 'newest_unix_ms': 0})
                            other['count'] += 1
                            other['oldest_unix_ms'] = min(other['oldest_unix_ms'], stamp)
                            other['newest_unix_ms'] = max(other['newest_unix_ms'], stamp)
                old["workdir_present" if is_dir(task.get("workdir", "")) else "workdir_missing"] += 1
            if status == "running":
                counts["running_pipes" if task.get("mode") == "pipes" else "running_pty"] += 1
                pid = task.get("child_pid")
                try:
                    if not pid:
                        raise ProcessLookupError()
                    os.kill(pid, 0)
                    alive = True
                except ProcessLookupError:
                    alive = False
                except PermissionError:
                    alive = True
                counts["running_pid_alive" if alive else "running_pid_dead_or_absent"] += 1
                if alive:
                    roots[root]["pid_alive"] += 1
                opens += 1
                try:
                    with (folder / "io/exit").open() as stream:
                        reads += 1
                        marker = stream.read().strip()
                    counts["running_with_marker" if marker else "running_empty_marker"] += 1
                except OSError:
                    counts["running_marker_absent"] += 1
            if status not in ("running", "starting", "killing") and not task.get("completion_delivered"):
                finished = task.get("finished_at") or 0
                if finished and now - finished / 1000 > 7 * 86400:
                    if task.get("project_root") and not exists(task["project_root"]) and not exists(task.get("workdir", "")):
                        counts["abandoned_undelivered_over_7d"] += 1
print(json.dumps({"at_unix": now, "inventory_work": {"opens": opens, "reads": reads, "stats": stats}, "counts": dict(counts), "metadata_age": dict(ages), "folder_mtime_age": dict(folder_ages), "old_terminal": dict(old), "undelivered_breakdown": dict(undelivered), "opencode_undelivered_sessions": opencode_sessions, "opencode_other_notify_true": opencode_other, "roots": dict(sorted(roots.items()))}, indent=2))
