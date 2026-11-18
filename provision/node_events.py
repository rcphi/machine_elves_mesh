"""What a mesh node's events mean, in words, for the two operator views.

node-summary.py and node-watch.sh each kept their own list, and both fell
behind the node: neither said anything about speculation, and the summary
counted `effect` and `produced` events the node stopped writing long ago, so a
node working steadily reported "0 effects, 0 productions" (review finding 20).
One list, read by both, is one list to keep up.

Fields in braces come from the event; a missing one prints as "?". The steady
hum — `working`, `rtt`, `dialling`, `listening` — is deliberately absent: it
arrives constantly and would bury the line that matters.
"""

NOTABLE = {
    # Who this node is.
    "identity-created": "made a new identity — peers will not know it yet",
    "identity-loaded": "was itself again across the restart",
    "identity-unreadable": "IDENTITY FILE UNREADABLE — became a stranger",
    "identity-not-saved": "could not save its identity — a stranger after every restart",
    "started": "started up",
    # Who runs the job.
    "claimed": "took the job — nobody else was running it",
    "cold-start": "RESTARTED THE JOB FROM TICK 0 — no peer held a checkpoint; {held} things already existed",
    "job-owner": "saw {label} running the job",
    "yielded": "gave the job to {label}, which was further along",
    "speculating": "suspected {label} after {silent_ms} ms of silence, and started its job at tick {tick}",
    "speculation-abandoned": "dropped a suspicion of {label} ({cause}); {ticks_wasted} ticks discarded",
    "took-over": "TOOK OVER the job at tick {tick} from {previous}",
    "resumed": "ran its first tick after a takeover: {gap_ms} ms since the last state it heard",
    "checkpoint-incompatible": "{label} runs a different job — ignored, and not a candidate for this one",
    "effects-rejected": "REFUSED a tick's production: {why}",
    "job-failed": "the job failed",
    # Who else is here.
    "joined": "{label} appeared",
    "left": "{label} left, announced",
    "vanished": "{label} VANISHED without warning",
    "returned": "{label} came back after {away_ms} ms (restarted: {restarted})",
    "restarted-unnoticed": "{label} restarted inside one detection window",
    "redialling": "lost a peer — trying its address again",
    "connected": "connected to {addr}",
    "learned-peer": "heard about a peer at {addr}",
    "my-address": "learned its own address: {addr}",
    "silent-connection": "dropped a connection that never spoke",
    # Reachability.
    "reachable": "a member reached this node at {addr}: it can introduce others",
    "unreachable": "no member can reach this node at {addr} any more",
    "port-mapped": "the router opened {external}",
    "port-map-failed": "the router did not grant a mapping: {error}",
    "listen-failed": "could not listen on an address",
    "publish-failed": "could not send a {what}: {error}",
    # What exists.
    "ledger-loaded": "remembered {held} things that already existed",
    "ledger-repaired": "cut {bytes} bytes of an interrupted append off the ledger",
    "ledger-corrupt": "LEDGER DAMAGED at byte {at} — kept {kept} items, original moved to {aside}",
    "ledger-unreadable": "LEDGER UNREADABLE — the inventory started from nothing",
    "ledger-not-saved": "could not write the inventory down ({pending} items held to retry)",
    "ledger-saved-again": "wrote down {retried} items an earlier attempt could not",
    "ledger-comparing": "found {buckets} of 256 inventory buckets differing from {label}'s",
    "ledger-repairing": "sent {label} {items} items from {cells} differing cells of bucket {bucket}",
    # Who it remembers.
    "peer-book-loaded": "remembered {peers} peers ({skipped} unreadable lines skipped)",
    "peer-book-not-saved": "could not write the peer book: {error}",
}


def describe(event):
    """One event in words, or None if it is not worth a line of its own."""
    import collections

    template = NOTABLE.get(event.get("event"))
    if template is None:
        return None
    fields = collections.defaultdict(lambda: "?", {k: v for k, v in event.items() if v != ""})
    try:
        return template.format_map(fields)
    except (IndexError, ValueError):
        return event.get("event")
