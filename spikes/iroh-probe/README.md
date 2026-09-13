# Does iroh get a direct connection where ours could not?

Our own measurements found diamond's router filters by address **and** port, so
neither machine can accept an unsolicited packet. That is why the mesh needs one
member with a forwarded port, and why §11.6's invite is structural rather than
thematic. iroh claims roughly 90% direct connections in production. This answers
whether that holds on *these* machines.

The question is not whether it connects — a relay guarantees that. It is whether
the traffic ends up **direct**, because a relayed connection has reintroduced
the always-on third party we are trying to do without.

## Running it

**This test is only meaningful between two genuinely different networks.**

A first attempt ran diamond against the `lightning` VM and came back relay-only.
That result was worthless: iroh reported both machines' public address as
`204.210.210.200`, because lightning is a VM on the same home connection. Two
hosts behind one NAT need hairpinning to reach each other by public address, and
this router refuses it — the same reason yoinkerz cannot see diamond while on
the home wifi. Nothing was learned about hole punching.

So: **yoinkerz must be on the phone hotspot.** Check first that the two are
actually on different networks — the `own addresses` line each side prints must
show *different* public addresses.

On diamond:

    ./iroh-probe listen

It prints an endpoint id, and keeps listening after each connection — so a
connect started from the wrong network and stopped does not use it up. The id is
new every time the listener starts. Then on yoinkerz, on the hotspot:

    ./iroh-probe connect <that-endpoint-id>

Both sides watch for a minute and print a verdict.

**Port mapping is off unless `--portmap` is given**, on either side. iroh
otherwise asks the router for a forwarded port over UPnP/PCP/NAT-PMP, and
diamond's router is already known to grant one. A direct result would then
report a door the router opened — which the mesh already has — rather than hole
punching, which is the question.

## Reading the result

Each side prints its own addresses, then what is actively carrying traffic.

- `ACTIVE ["Ip(...)"]` — direct. Hole punching worked where ours could not, and
  a mesh could form with nobody having touched a router.
- `ACTIVE ["Relay(...)"]` for the full minute — no direct path. iroh is no
  better placed here than we are, and adopting it would buy a relay dependency
  rather than remove one.

The verdict is decided by the paths **active** at the end, never by `also
known`. A direct address tried and then abandoned for the relay shows up there,
and an earlier version counted it — calling a relay-only run direct.

`also known: []` alongside a relay verdict does **not** establish that hole
punching was never attempted. That was asserted here before it was checked
against iroh, and the output cannot tell the two apart: a path that was tried
and never validated may simply not be listed. Check the `own addresses` lines:
if both show the same public address, the two machines are behind one NAT and
the run tells you nothing.

## Result — 2026-09-12

**Relay only, both sides, for the full minute.** diamond on home broadband,
yoinkerz on the phone hotspot, port mapping off on both:

    diamond   local 0.0.0.0:58796   public 204.210.210.200:58796
    yoinkerz  local 0.0.0.0:45777   public 69.223.231.203:1837

    [  0.0s] ACTIVE ["Relay(https://use1-1.relay.n0.iroh.link./)"]  (also known: [])
    VERDICT: relay only

**It is the result our own probe predicted, not a failure peculiar to iroh.**
diamond preserves its port but filters by address and port, so it admits only
packets from the exact address and port it has already sent to. yoinkerz's
carrier rewrites the port (45777 became 1837) and chooses a new one per
destination. diamond can never send to the port yoinkerz's packets will arrive
from, because that port does not exist until yoinkerz sends — the same dead end
`docs/phase-0-plan.md` reached on 2026-08-22. No hole-punching technique crosses
it; iroh's ~90% is across a population, and this pair is in the remainder.

**So iroh does not remove the need for a reachable member here.** Adopting it
for this pair would buy a relay dependency, not remove one. The forwarded port
stays.

**What this does not show.** One pair is not a sample: a friend's router more
permissive than diamond's could punch fine against the same hotspot.

**The lead it leaves.** yoinkerz holds global IPv6 addresses on the hotspot;
diamond has only link-local, and `curl -6` fails — IPv6 is off somewhere between
diamond and the ISP, most likely in the router. Home IPv6 has no address
translation, which usually makes hole punching trivial. Worth enabling and
rerunning before concluding anything about friends who have it.
