# Relaying and hole punching — design

2026-11-18. For the Phase 0 mesh node (`node/`). Follows Run F (`docs/phase-0-plan.md`, "Run F: three connections, measured").

**Approved by the operator, 2026-11-18.**

## Why

Run F put three machines on three connections, and only `riverboat` was reachable.
- **The two unreachable machines never connected to each other.** Both of their routers give one public port to every destination, and both nodes dialled the other's correct address for over two hours. Packet captures on both ends show every packet sent and none received.
- **They reached each other only through riverboat**, because gossip forwards messages. When riverboat left, the mesh split, and both halves ran the job for 57 minutes.
- **It healed 1.3 s after riverboat returned.**
- **Retrying the dead address every 2 s cost about 13 MB/h per address.** That was two-thirds of the laptop's hotspot data.

The operator's aim, in their words: "the network, and the in-game work that relies on it, should be as resilient and self-healing as reasonably possible."

Run F also showed what a relay alone cannot do. **A relay disappears with the machine that hosts it.** With a single reachable member, only a direct link survives that member leaving, and the direct link is what failed. This design therefore does two things:
- it makes **more members reachable**, so there is more than one machine to pass traffic through;
- it adds **coordinated punching**, so that unreachable pairs gain direct links where their routers allow.

What each job does when the mesh splits anyway was decided in the same conversation. It is the next design, not this one: see "Decided for the next design".

## Goals

1. **More reachable members.** A node asks its router for a port by UPnP as well as NAT-PMP. Today it speaks only NAT-PMP, which the friend's router ignores.
2. **Every reachable member offers introductions.** It runs libp2p's relay with the default limits, which are long enough to coordinate a punch and too short to carry the mesh's traffic. An unreachable member holds introductions on up to three reachable members, so no single one is critical. *(Three: the operator's call, 2026-11-18.)*
3. **Unreachable pairs try a coordinated punch.** They use libp2p's DCUtR, run over an introduction. A punch that works gives a direct link that survives the introducer leaving. A punch that fails costs redundancy, not connectivity, because gossip keeps forwarding through the reachable members.
4. **Healing across restarts.** A node remembers its peers on disk and uses them to get back in.
5. **Cheap retries.** An address that never answers costs well under 1 MB/h, instead of 13 MB/h.
6. **Everything observable.** Every new mechanism writes events, and the run summary reports how links were made.
7. **Nothing that works today gets worse.** That covers detection, resume, rejoin after a restart, transitive discovery, the existing rigs, and today's job behaviour, split included.

## Non-goals

- **Jobs that must not run twice.** This is the second project, designed next.
- **Relayed connections as permanent links**, with the relay's limits lifted (approach B below). Gossip already does that forwarding.
- **Replacing libp2p** (approach C).
- Scaling past a full mesh to §11.6's O(log n) overlay. Deferred by the plan.
- The WireGuard substrate §11.6 names, and IPv6 bridging.
- Accounting for, or rewarding, relay work (§10.3's fifth lever). A reachable member choosing not to relay. In Phase 0 every reachable member offers.

## Anti-goals: failures even if it works

- **Any server that is not a member:** no public STUN servers in the node, and no hosted relay or rendezvous. A node learns whether it is reachable only from members. UPnP and NAT-PMP talk only to the member's own router.
- Management traffic over the mesh, or a fallback route that succeeds quietly.
- **Dialling that costs more data than today.** The hotspot is the case that pays.
- **A slower takeover, or slower rejoining after a restart.** Either would be paying for redundancy with the property the thresholds measure.

## Constraints

From `CLAUDE.md` and the plan:
- No rented infrastructure.
- The management channel stays off the measured path.
- The probe stays std-only. It is not touched here.
- The thresholds fixed on 2026-08-21 still judge every takeover: 2–3 s detection and 500 ms resume, at p95.
- Results are an upper bound.
- Rust via rustup, static musl builds, and tests with every behaviour.
- libp2p 0.56, already in use. The `relay`, `dcutr`, `autonat` and `upnp` features are part of it and were fetched and read on 2026-11-18. The relay's defaults are 2 minutes and 128 KiB per relayed connection, 16 connections at once and 4 per peer. Neither the swarm nor gossipsub gives a relayed connection special treatment.

## Approaches

- **A. Gossip keeps relaying, and libp2p supplies the rest** — *chosen.* The relay is used only to introduce two unreachable members for a punch. On top of it: UPnP alongside NAT-PMP, reachability learned from members, remembered peers, and a dial backoff. It fixes what Run F broke, with the smallest change to what already works.
- **B. Full relayed connections**, with the limits lifted, as permanent links beside gossip. This duplicates forwarding that gossip already does. It pays off only in meshes too large for everyone to connect to everyone, which Phase 0 does not reach.
- **C. Switch to iroh**, a library built around punching with a relay fallback, modelled on Tailscale. Rejected for three reasons:
  - its relays are separate server programs, and its default address lookup uses its maker's servers, which would have to be switched off;
  - it means rewriting the network layer of a spike meant to be discarded;
  - Run F's pair sent to each other's correct addresses without pause, and better punching tactics are not known to beat that.

The operator chose A on 2026-11-18.

## The design

### Reachability

- **Asking the router.** A node asks for a port by NAT-PMP as now, and by UPnP as well, using libp2p's `upnp` behaviour. Whichever router answers grants the mapping, and both outcomes are reported as events. NAT-PMP keeps its current renewal. UPnP uses the behaviour's own.
- **Knowing it is reachable.** A node counts itself reachable only once a member has made a successful connection to it at its external address. That happens either because the member dialled the announced address, or through libp2p's `autonat` dial-back, with members as the only servers it asks. A mapping that was granted but never connected through does not count. *(The operator's call, 2026-11-18.)*
- **Found in the NAT rig, 2026-11-18: AutoNAT alone cannot confirm the member everyone dials.** Its v2 server answers only on connections it received, so a node is tested only by peers it dialled. A reachable member is the one that dials nobody, so it was never tested and never introduced anyone. A node therefore also counts itself reachable when a member connects to it **from an address it has not dialled in the last 10 minutes**.
  - **Why that proves it:** both of Run F's routers admit only replies, so only an open door lets such a connection in. A punched link or a blind dial met halfway arrives from an address the node had just sent to, so it never counts.
  - **The address registered:** the one the member reports seeing, from identify. libp2p's relay grants introductions only once it knows such an address.
  - **AutoNAT stays** for the other case: a node behind a mapping that dials out, as the rig's UPnP member does.
  - **What never counts:** an address through an introducer never makes a node reachable, whichever end of the connection it is seen from.
- **What a confirmed node does:** it says so in its heartbeat, and it serves introductions.
- **Found 2026-11-18: a host that keeps away from its household cannot use multicast.** diamond's `isolate-lan` rejects all outgoing multicast, so that it cannot talk to the friend's other devices, and UPnP's discovery step is multicast. The gateway itself is exempt, because DHCP and NAT-PMP need it. So the node asks the gateway directly first (unicast), then by multicast, which is how most routers are found.
  - **Multicast is used because players' machines should need no router setup.** In the operator's words: "I want this game to work on as many computers as possible with the least router configuration done by the user. Let's take advantage of the probability of some users having UPnP enabled." Where a host blocks multicast, as diamond does, that attempt fails harmlessly and is reported. `isolate-lan` is unchanged, because the friend's router does not answer UPnP even when multicast is let out.
  - libp2p's `upnp` behaviour may search by multicast only. If so, the builder uses `igd-next` directly, pointed at the gateway.
  - **Checked 2026-11-18: the friend's router does not answer UPnP.** `provision/upnp-check.py` was run on diamond with two temporary openings in place: SSDP multicast let out, and UDP from the gateway let in. The router answered neither the direct query nor the multicast one, so it lacks UPnP or has it switched off. It also ignores NAT-PMP.
  - **diamond therefore stays unreachable.** In Run F's topology, only a coordinated punch can keep diamond and yoinkerz together once riverboat has gone.
  - **If the friend turns UPnP on in the router's settings**, the same script answers whether diamond could then become a second reachable member. The node would also need SSDP let out through `isolate-lan` permanently, which touches the household, so that is the operator's decision.

### Introductions and punching

**An introduction** is how two members that cannot reach each other still meet. Both can reach a third member, which passes each of them the other's current address and tells them to send now. Both then send at the same moment, and each router lets the other's packets in because its own machine has just sent to that address. The introducer only arranges the meeting. It lasts a couple of minutes at most, and carries none of their traffic afterwards. That is libp2p's relay, used with its short default limits.

- **Who runs what.** Every node runs the relay server with the default limits, and the relay client. Only members whose heartbeat says they are confirmed reachable are *asked* to introduce. *(Changed while planning, 2026-11-18: the first version ran the server only on confirmed-reachable members. libp2p fixes a node's behaviours when the swarm is built, while reachability changes as it runs, so the choice moved to who is asked.)*
- **Reservations.** A node that is not confirmed reachable keeps reservations on up to three reachable members, or on all of them if there are fewer, and renews them.
- **Announcing.** It announces the addresses it can be reached at through those relays, using the existing announcement (`Observed`, every 30 s), extended to carry relay addresses.
- **Punching.** When a node has no direct link to a member, and neither of them is confirmed reachable, it dials that member through a relay. DCUtR then times both sides' sends.
  - **If the punch works,** the pair has a direct connection, and gossip uses it like any other.
  - **If it fails,** the attempt is retried with a backoff while no direct link exists: about 1 minute, doubling to a cap of about 30 minutes. Each attempt is bounded by the relay's 128 KiB. If gossip happens to carry some traffic over the relayed connection meanwhile, that only uses up the same budget sooner.
- **Blind dialling of observed addresses stays, under the backoff below.** Transitive discovery depends on it: a node that knows one address hears of the rest and dials them. So do the loopback and LAN rigs, where every observed address answers. What changes is the rate. Run F's cost came from dialling every 2 s forever, not from dialling at all.

### Remembered peers

- **The peer book.** A per-mesh file beside the ledger. For each peer it records:
  - its label;
  - the addresses that reached it by being dialled;
  - whether it was confirmed reachable;
  - when it was last heard.

  It is written atomically when it changes.
- **On start,** a node dials its configured peers and every remembered confirmed-reachable address. The plan's rule holds: remember the peers you can dial, and the ones you cannot dial will remember you.

### Dial backoff

Each address keeps its own schedule.
- **Retries start at 2 s,** as now. A restart is still redialled within seconds, and that path must not get slower.
- **They then double, to a cap of about 5 minutes** for remembered and observed addresses, and about 60 s for configured ones, which are the way in.
- **A schedule resets** when the address is first learned, or when a direct connection to its peer is lost. Hearing a peer through a forwarder must not reset it: that is Run F's case, where diamond heard yoinkerz every second through riverboat. A periodic re-announcement must not reset it either. *(Corrected 2026-11-18, while planning: the first version said "or when its peer is heard from in any other way", which would have restored Run F's cost.)*

**The requirement, whatever the numbers:** rejoining after a restart is no slower than today, and an address that never answers costs under 1 MB/h.

### Jobs during a split

**Unchanged.** Every job behaves as it does today: speculation, takeover on silence, both sides running during a split, and a yield by tick when the sides rejoin. The next design changes this for jobs that must not run twice.

### Observability

- **New events,** each with an entry in `provision/node_events.py`:
  - a port granted or refused, by each method;
  - reachability confirmed or lost;
  - a reservation held or lost;
  - a punch tried, succeeded or failed;
  - a link made, and whether it is direct or relayed;
  - the peer book loaded or saved;
  - a dial schedule backed off.
- **`takeover-summary.py`, or a sibling tool, reports per run:**
  - how many pairs had a direct link;
  - how many punches were tried and how many worked;
  - how long each pair depended on a forwarder;
  - how many split-brain intervals there were, and how long each lasted. Run F's 57 minutes is the baseline.

### Validation

1. **Unit tests:** the backoff schedule, the peer book across a restart, choosing which relays to reserve on, and deciding when to try a punch.
2. **The existing rigs still pass:** `rig.sh`, `migration-rig.sh`, `chaos-rig.sh`, and the transitive-discovery check.
3. **A namespace rig on lightning,** modelled on `probe/inbound-rig.sh`: one public member and two members behind Linux NATs. It runs two scenarios:
   - **Port-preserving NATs,** where the punch should succeed. Pass: the pair gains a direct link, and **killing the public member does not split the mesh.**
   - **One NAT with randomised ports** (`masquerade random,fully-random`), where the punch must fail. Pass: the pair stays in touch through the public member while it is up, retries stay cheap, and the split and heal behave as in Run F.

   **UPnP** is tested in the rig if `miniupnpd` can run in the NAT namespace. Otherwise it is tested against a real router.
4. **Run G:** Run F's three boxes, running the new node, with predictions fixed before the run as for every run.
   - **What it can and cannot show:** the friend's router answers neither NAT-PMP nor UPnP (checked 2026-11-18), so riverboat is still the only reachable member. Run G therefore tests whether a *coordinated* punch connects diamond and yoinkerz where Run F's blind simultaneous dialling did not.
   - **If the punch fails,** losing riverboat still splits the mesh as in Run F. What Run G then measures is the rest: retries that cost little, introductions renewed and not churned, and a split that still heals.
   - **Avoiding the split altogether** needs a second reachable member. In this topology that is the friend turning UPnP on, or a fourth machine on a connection whose router grants mappings.

## Decided for the next design: jobs that must not run twice

**The operator's decisions, 2026-11-18:**
- **The rule is chosen per job,** by whether the job can repeat. Jobs that can repeat keep running on every side of a split, as now. Jobs that cannot run only on a side that sees a majority of their holders. In their words: "I would rather the game pause or lose partial service over the game's logic breaking."
- **Computing work twice during a brief disconnect is fine** — "ignore my 'never duplicated' comment". What must happen only once is a result *counting*. Strict jobs follow §9.3 as written: "Quorum consensus before commit."
- **It is built on an established consensus library,** not hand-written.

**What reading this design as its builder found it requires.** It is more than a pause rule, and it is why this is a second project.
- **A result that counts once needs a majority before it counts.** An owner cut off from the majority can compute a few ticks, about one detection window's worth, before it can know to stop. If its successor resumes from an older checkpoint, those ticks happen twice. Ruling that out means a tick's results count only once a majority of the job's holders holds the checkpoint they came from. That is §9.3's Strict tier as written: "Quorum consensus before commit."
- **That brings the usual parts of a consensus protocol:**
  - ownership terms, so that an old owner cannot gather a majority once a successor has started;
  - a successor that resumes from the newest checkpoint any member of its majority holds;
  - holder-set changes, joins and departures, decided by a majority the same way. Membership change is where majority designs usually break.
- **Built on a library,** as the operator decided. Which library, and how it carries its messages over the mesh, is that design's first question.

**Consequences already known, and accepted by the operator's preference:**
- A strict job never speculates, so its takeover takes about a detection window, roughly 2.5–3 s, where a repeatable job's gap is about 1 s.
- In a mesh of two, a strict job hands over on a clean shutdown and **pauses on any crash** until the machine returns.
- A holder that dies and never returns keeps the job paused until a person removes it. Silence never removes one.
- In Run F's three-way split, every side would have paused.

**One part decided already:** a job declares its kind inside its own file, in a WebAssembly custom section. The job's identity is the hash of that file, so every holder of a job agrees on its kind, and a job without the section behaves as today.

## What is left to the builder

- Wiring the libp2p features into the existing swarm, including the relay client transport alongside QUIC and TCP.
- AutoNAT v1 or v2.
- The exact backoff numbers, within the requirements above.
- How relay addresses ride in the existing announcement.
- Event names.
- The rig's construction.
- Whether a punch over QUIC works in libp2p 0.56 as documented. The rig is where that is found out, before any box.
