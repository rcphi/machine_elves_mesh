#!/usr/bin/env bash
#
# Watches a node as it works, in plain language.
#
#   sudo ./node-watch.sh
#
# For the moments when something is being deliberately broken and the question
# is what the node makes of it. node-summary.py answers "what happened"; this
# answers "what is happening".
set -euo pipefail

LOG="${1:-/var/log/mesh-node/events.jsonl}"
[[ -r "$LOG" ]] || { echo "cannot read $LOG (try sudo)" >&2; exit 1; }

echo "watching $LOG — ctrl-c to stop"
echo

# The steady hum of working is filtered out. Effects and heartbeats arrive
# several times a second and would bury the one line that matters.
HERE="$(cd "$(dirname "$0")" && pwd)"
# The descriptions live in node_events.py, shared with node-summary.py, so the
# two views cannot drift apart again (review finding 20).
tail -n 0 -F "$LOG" 2>/dev/null | MESH_PROVISION="$HERE" python3 -u -c '
import json, os, signal, sys, time
sys.path.insert(0, os.environ["MESH_PROVISION"])
from node_events import describe

# Ctrl-C is how this is meant to end. A traceback makes an ordinary exit look
# like a fault.
signal.signal(signal.SIGINT, lambda *_: sys.exit(0))

for line in sys.stdin:
    line = line.strip()
    if not line.startswith("{"):
        continue
    try:
        event = json.loads(line)
    except json.JSONDecodeError:
        continue
    said = describe(event)
    if said is None:
        continue
    clock = time.strftime("%H:%M:%S", time.localtime(event.get("ts_unix_ms", 0) / 1000))
    print(f"{clock}  {said}")
'
