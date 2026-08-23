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
tail -n 0 -F "$LOG" 2>/dev/null | python3 -u -c '
import json, sys, time

SAY = {
    "started":        "started up",
    "claimed":        "TOOK THE JOB — nobody else was running it",
    "job-owner":      "{label} is running the job",
    "yielded":        "gave the job to {label} — it is further along",
    "took-over":      "TOOK OVER at tick {tick}",
    "joined":         "{label} appeared",
    "left":           "{label} left, announced",
    "vanished":       "{label} VANISHED — no goodbye",
    "redialling":     "lost a peer, trying its address again",
    "connected":      "connected: {addr}",
    "my-address":     "this node is at {addr}, says {observed_by}",
    "port-mapped":    "router opened {external}",
    "port-map-failed":"router refused a mapping",
    "job-failed":     "JOB FAILED",
    "listen-failed":  "could not listen",
    "learned-peer":   "heard about a peer at {addr}",
}

for line in sys.stdin:
    line = line.strip()
    if not line.startswith("{"):
        continue
    try:
        event = json.loads(line)
    except json.JSONDecodeError:
        continue
    kind = event.get("event")
    if kind not in SAY:
        continue
    fields = {k: event.get(k, "?") for k in
              ("label", "tick", "addr", "external", "observed_by")}
    clock = time.strftime("%H:%M:%S", time.localtime(event.get("ts_unix_ms", 0) / 1000))
    print(f"{clock}  {SAY[kind].format(**fields)}")
'
