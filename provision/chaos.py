#!/usr/bin/env python3
"""Takes this box's mesh node away at random, so takeovers can be timed.

    sudo ./chaos.py install          put the tool and its timer in place, disarmed
    sudo ./chaos.py arm 8            fire at random for the next eight hours
    sudo ./chaos.py disarm           stop now
    ./chaos.py status                armed or not, and what it has done

The timer calls `chaos.py fire`, which does nothing unless this node is running
the job and has a peer to hand it to. Killing a standby measures nothing; killing
a node with nobody to take over measures the restart, not the mesh.

**Runs on the box, not from somewhere else.** Remote control of these machines
is the problem being measured (docs/phase-0-plan.md, "The contamination rule").
Once armed, nothing needs to reach the box until the run is over.

**It disarms itself.** Arming takes a number of hours and the timer switches
itself off when they are up. A chaos timer forgotten on a friend's machine would
otherwise keep killing their mesh node indefinitely.

Three ways to take a node away, chosen at random each time:

  kill       SIGKILL. No goodbye. The case the thresholds are about.
  term       SIGTERM. The node announces its departure first — a different
             path through the code, kept as a separate distribution.
  partition  The node keeps running but none of its packets pass, in either
             direction, for 20–60 s. Then it returns without having restarted,
             so both sides may believe they own the job. The faults found in
             August all lived in that return.

systemd's Restart=always brings the node back after kill and term.
"""
import json
import os
import random
import shutil
import subprocess
import sys
import time

# Overridable so node/chaos-rig.sh can play two boxes on one machine. On a box,
# none of these are set and the defaults are the installed paths.
NODE_LOG = os.environ.get("MESH_NODE_LOG", "/var/log/mesh-node/events.jsonl")
CHAOS_LOG = os.environ.get("MESH_CHAOS_LOG", "/var/log/mesh-node/chaos.jsonl")
STATE_DIR = os.environ.get("MESH_CHAOS_STATE", "/var/lib/mesh-chaos")
NODE_CONFIG = "/etc/mesh-node/config"
NODE_UNIT = os.environ.get("MESH_NODE_UNIT", "mesh-node.service")
INSTALLED = "/usr/local/sbin/mesh-chaos"
UNITS = ("mesh-chaos.service", "mesh-chaos.timer")
# One table per node unit, so two rig nodes on one machine cannot lift each
# other's partition. On a box this is plain "mesh_chaos", which the service's
# ExecStopPost names.
NFT_TABLE = "mesh_chaos" if NODE_UNIT == "mesh-node.service" else \
    "mesh_chaos_" + "".join(c if c.isalnum() else "_" for c in NODE_UNIT.removesuffix(".service"))

# Weighted towards the unannounced loss, because that is what the thresholds
# govern and the distribution that needs the most samples.
ACTIONS = (("kill", 0.6), ("term", 0.2), ("partition", 0.2))
PARTITION_SECONDS = (20, 60)

# A node must have been settled this long before it is disturbed again. The
# previous disruption — a restart waits 10 s, then listens for three detection
# windows before claiming — must be over, or the next measurement starts from
# a mesh still recovering from the last one and times both at once.
SETTLED_SECONDS = int(os.environ.get("MESH_CHAOS_SETTLED_S", "60"))

# `working` is only ever written by the node advancing the job, so it proves
# ownership on its own. That matters after a log rotation, where the `claimed`
# or `took-over` that started the ownership may be in yesterday's file: on the
# night of 2026-09-15 riverboat held the job from 00:26 and its chaos read the
# rotated log, decided it was a standby, and fired nothing for five hours.
OWNING = {"claimed", "took-over", "working"}
NOT_OWNING = {"job-owner", "yielded", "job-failed"}


def now_ms():
    return int(time.time() * 1000)


def read_events(path, tail_bytes=2_000_000):
    """The current run's events: everything after the last `started`.

    Only the tail is read. The log outlives every restart and grows for weeks;
    the question is always about the process running now.
    """
    try:
        with open(path, "rb") as handle:
            handle.seek(0, os.SEEK_END)
            size = handle.tell()
            handle.seek(max(0, size - tail_bytes))
            raw = handle.read().decode("utf-8", errors="replace")
    except OSError:
        return []
    events = []
    for line in raw.splitlines():
        if not line.startswith("{"):
            continue
        try:
            events.append(json.loads(line))
        except json.JSONDecodeError:
            continue
    for index in range(len(events) - 1, -1, -1):
        if events[index].get("event") == "started":
            return events[index:]
    # No start in the tail. The log is rotated daily with copytruncate, so a run
    # that outlives a rotation leaves its start — and its joins, and its claim —
    # in yesterday's file. Reading only today's loses the peer and the
    # ownership, and a node that knows neither never fires.
    rotated = path + ".1"
    if os.path.exists(rotated):
        return read_events(rotated, tail_bytes) + events
    return events


def situation(events, now):
    """What this node is doing, from its own account of the current run.

    Returns (owns_job, peers_present, settled_for_seconds, last_tick).
    """
    owns = False
    peers = set()
    last_disturbance = None
    last_tick = None
    for event in events:
        kind = event.get("event")
        ts = event.get("ts_unix_ms", 0)
        if kind == "started":
            last_disturbance = ts
        elif kind in OWNING:
            owns = True
            last_disturbance = ts
        elif kind in NOT_OWNING:
            owns = False
            if kind == "yielded":
                last_disturbance = ts
        elif kind == "joined":
            peers.add(event.get("peer_id"))
            # A peer arriving can take the job away: after a partition both
            # sides ran it, and whoever is behind yields a moment later. Firing
            # in that moment kills a node its own log still calls the owner —
            # found in the third chaos-rig dry run, 160 ms after a partition
            # ended, in the same millisecond as the yield.
            last_disturbance = ts
        elif kind in ("vanished", "left"):
            peers.discard(event.get("peer_id"))
            last_disturbance = ts
        if "tick" in event:
            last_tick = event["tick"]
    settled = (now - last_disturbance) / 1000 if last_disturbance else 0
    return owns, len(peers), settled, last_tick


def decide(events, now, rng=random):
    """(action or None, reason)."""
    owns, peers, settled, _ = situation(events, now)
    if not events:
        return None, "no events from the node — is it running?"
    if not owns:
        return None, "not running the job"
    if peers == 0:
        return None, "no peer to take the job over"
    if settled < SETTLED_SECONDS:
        return None, f"settled only {settled:.0f}s"
    roll = rng.random()
    for action, weight in ACTIONS:
        if roll < weight:
            return action, "running the job with a peer present"
        roll -= weight
    return ACTIONS[0][0], "running the job with a peer present"


def armed_until():
    try:
        with open(os.path.join(STATE_DIR, "until")) as handle:
            return int(handle.read().strip())
    except (OSError, ValueError):
        return None


def label():
    if os.environ.get("MESH_NODE_LABEL"):
        return os.environ["MESH_NODE_LABEL"]
    try:
        with open(NODE_CONFIG) as handle:
            for line in handle:
                if line.startswith("LABEL="):
                    return line.split("=", 1)[1].strip().strip('"')
    except OSError:
        pass
    return os.uname().nodename


def record(**fields):
    entry = {"ts_unix_ms": now_ms(), "node": label(), **fields}
    line = json.dumps(entry)
    print(line)
    try:
        with open(CHAOS_LOG, "a") as handle:
            handle.write(line + "\n")
    except OSError as error:
        print(f"could not write {CHAOS_LOG}: {error}", file=sys.stderr)


def run(*command, check=True):
    return subprocess.run(command, check=check, capture_output=True, text=True)


def unblock():
    run("nft", "delete", "table", "inet", NFT_TABLE, check=False)


def partition(seconds):
    """Drops every packet belonging to the mesh node and nothing else.

    Matched by the node's control group rather than its port, for two reasons.
    Outbound TCP leaves from ephemeral ports a port rule would miss. And SSH
    keeps working, so a box being partitioned can still be looked at from the
    LAN — which does not cross the router being measured.

    Both directions, because a real outage is symmetric. Blocking only what
    the node sends would leave it hearing its peer, believing it still owns
    the job with a peer present, and never experiencing the partition at all.
    """
    group = f"system.slice/{NODE_UNIT}"
    rules = f"""
table inet {NFT_TABLE} {{
    chain input {{
        type filter hook input priority -10; policy accept;
        socket cgroupv2 level 2 "{group}" drop
    }}
    chain output {{
        type filter hook output priority -10; policy accept;
        socket cgroupv2 level 2 "{group}" drop
    }}
}}
"""
    unblock()
    subprocess.run(["nft", "-f", "-"], input=rules, check=True, capture_output=True, text=True)
    try:
        time.sleep(seconds)
    finally:
        # The unit's ExecStopPost removes the table as well, so a partition
        # interrupted here — the service stopped, the process killed — does not
        # leave the node cut off until someone notices.
        unblock()


def fire():
    until = armed_until()
    if until is None:
        record(action="skip", reason="not armed")
        return
    if now_ms() >= until:
        record(action="disarmed", reason="the armed period is over")
        disarm(quiet=True)
        return

    events = read_events(NODE_LOG)
    action, reason = decide(events, now_ms())
    _, _, _, tick = situation(events, now_ms())
    if action is None:
        record(action="skip", reason=reason)
        return

    if action == "kill":
        record(action="kill", reason=reason, tick=tick)
        run("systemctl", "kill", "--signal=SIGKILL", NODE_UNIT)
    elif action == "term":
        record(action="term", reason=reason, tick=tick)
        run("systemctl", "kill", "--signal=SIGTERM", NODE_UNIT)
    elif action == "partition":
        seconds = random.randint(*PARTITION_SECONDS)
        record(action="partition", reason=reason, tick=tick, seconds=seconds)
        partition(seconds)
        record(action="partition-ended", seconds=seconds)


def install():
    here = os.path.dirname(os.path.abspath(__file__))
    for unit in UNITS:
        shutil.copy(os.path.join(here, unit), f"/etc/systemd/system/{unit}")
    shutil.copy(os.path.abspath(__file__), INSTALLED)
    os.chmod(INSTALLED, 0o755)
    os.makedirs(STATE_DIR, exist_ok=True)
    run("systemctl", "daemon-reload")
    print(f"installed {INSTALLED} and {', '.join(UNITS)} — disarmed")
    print("arm with:  sudo mesh-chaos arm <hours>")


def arm(hours):
    if not 0 < hours <= 24:
        sys.exit("arm takes between 0 and 24 hours; a longer run is a new decision, not a default")
    os.makedirs(STATE_DIR, exist_ok=True)
    until = now_ms() + int(hours * 3600 * 1000)
    with open(os.path.join(STATE_DIR, "until"), "w") as handle:
        handle.write(str(until))
    run("systemctl", "enable", "--now", "mesh-chaos.timer")
    record(action="armed", hours=hours, until_unix_ms=until)


def disarm(quiet=False):
    try:
        os.remove(os.path.join(STATE_DIR, "until"))
    except OSError:
        pass
    # --no-block, because the timer's own service calls this, and waiting for a
    # stop that waits on this process would hang both.
    run("systemctl", "disable", "--now", "--no-block", "mesh-chaos.timer", check=False)
    unblock()
    if not quiet:
        record(action="disarmed", reason="by hand")


def status():
    until = armed_until()
    if until is None or now_ms() >= until:
        print("disarmed")
    else:
        print(f"armed for another {(until - now_ms()) / 3600000:.1f} h")
    counts = {}
    try:
        with open(CHAOS_LOG) as handle:
            for line in handle:
                try:
                    action = json.loads(line).get("action")
                except json.JSONDecodeError:
                    continue
                counts[action] = counts.get(action, 0) + 1
    except OSError:
        pass
    for action, count in sorted(counts.items()):
        print(f"  {action:<16} {count}")
    owns, peers, settled, tick = situation(read_events(NODE_LOG), now_ms())
    print(f"node: {'running the job' if owns else 'standing by'}, {peers} peer(s), "
          f"settled {settled:.0f}s, tick {tick}")


def main(argv):
    command = argv[1] if len(argv) > 1 else "status"
    needs_root = command in ("install", "arm", "disarm", "fire")
    if needs_root and os.geteuid() != 0:
        sys.exit(f"{command} needs root")
    if command == "install":
        install()
    elif command == "arm":
        arm(float(argv[2]) if len(argv) > 2 else 0)
    elif command == "disarm":
        disarm()
    elif command == "fire":
        fire()
    elif command == "status":
        status()
    else:
        sys.exit(__doc__)


if __name__ == "__main__":
    main(sys.argv)
