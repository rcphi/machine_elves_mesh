# Connection probe

Reports what one internet connection can and cannot do. This is the first
deliverable of Phase 0 (`../docs/phase-0-plan.md`) and it exists to answer a
question cheaply that would otherwise be answered expensively: **can these
machines reach each other at all, without renting a server?**

## What it does

Sends STUN binding requests to four public servers run by four different
operators, and compares what each one reports. From that it determines:

- The address the internet sees, versus the machine's own address.
- **Whether the same external port is used for every destination.** This is the
  measurement that matters, and it is why one socket is reused for all four
  queries — a fresh socket per server would measure nothing. Consistent ports
  mean hole punching can work; varying ports mean it usually will not.
- **Carrier-grade NAT**, detected unambiguously when the "public" address is
  itself inside the ISP's shared range (`100.64.0.0/10`).
- **Whether IPv6 works.** This is the outcome that would make the whole NAT
  question moot, since two peers with working IPv6 can address each other
  directly with no traversal and no relay.

It uploads nothing. The report is printed locally for the operator to send back
by hand.

## Running it

Needs Rust (<https://rustup.rs>), then:

```
cargo run --release
```

It takes under a minute and prints a summary line to send back, of the form:

```
PROBE v0.1 | ipv4=nat-endpoint-independent | ipv6=none | relay-capable=no
```

## Reading the result

| `ipv4=` | Meaning |
|---|---|
| `public` | No translation. Can accept incoming connections and can carry relay duty. |
| `nat-endpoint-independent` | Behind NAT, but predictable. Hole punching should work. |
| `nat-address-dependent` | Behind NAT that varies the external port per destination. Hole punching unreliable; needs a relay. |
| `cgnat` | Carrier-grade NAT. No incoming connections on IPv4 at all. |
| `double-nat` | Two or more layers of translation. Treat as `cgnat`. |
| `unreachable` | No STUN server answered — outbound UDP is likely blocked. |

**`relay-capable=yes` on at least one volunteer is what Phase 0 needs.** With no
rented infrastructure, a peer that cannot be reached directly needs a peer that
can, and that has to be one of the participants.

If every volunteer reports `relay-capable=no` and `ipv6=none`, the stop
criterion in `../docs/phase-0-plan.md` has been met on day one. That is a
genuine finding rather than a failure, and it arrives for the cost of one
afternoon instead of one year.

## Mapping lifetime

```
cargo run --release -- --mapping-lifetime
```

A separate measurement answering a separate question: **how long does this
router remember a connection nobody is using?**

When a machine sends a packet out, the router rewrites it to its own address
and remembers the pairing so replies can find their way back. That memory
expires. When it does, a peer-to-peer connection dies **silently** — the other
side's packets arrive at a port the router no longer recognises and are
discarded, with no error and no notification.

The mesh therefore has to send keepalives: small meaningless packets whose only
job is to remind the router that a connection still exists. The interval is a
real tradeoff — too long and connections die, too short and every peer pair in
the mesh is waking machines and burning battery for nothing.

**The method:** learn the external port, send absolutely nothing for a set
interval, then ask the same server again through the same socket. An unchanged
port means the mapping survived.

Two details that make the answer trustworthy:

- **The same STUN server is used for both queries.** Under address-dependent
  mapping a different server sees a different port anyway, so comparing across
  servers would report expiry every single time.
- **A changed public address is reported as inconclusive rather than expired.**
  If the ISP moves the connection mid-test, the port changes for a reason that
  says nothing about the router.

**Each run tests exactly one idle interval**, chosen at random from a list
running from 15 seconds to 10 minutes. This run might stay silent for 90
seconds and the next for 240, and over days every interval gets tried many
times.

The alternative would be to narrow in on the answer within a single run, trying
a long interval and then a shorter one. That is faster in principle and worse in
practice: it takes many minutes and a failure partway through wastes the whole
attempt. One interval per run means a failed run costs one data point.

The list deliberately includes values above and below 120 seconds, which is the
minimum the relevant standard (RFC 4787) asks routers to provide. Real routers
range from about 30 seconds to many minutes.

`summarise.py` combines the results into a range — "forgets somewhere between
60s and 90s" — and recommends a keepalive interval at half the shortest
confirmed-safe value, **taken across machines rather than averaged.** A peer
whose mapping has expired is unreachable no matter how patient the others are,
so the mesh is governed by the worst router among the players.

## What it deliberately does not test

- **Firewall filtering.** A machine may hold a global IPv6 address and still
  refuse inbound packets. Determining that needs a cooperating peer, which is
  Phase 0 proper.
- **Throughput or latency under load.**
- **Whether a peer can actually be reached.** Everything here is inferred from
  what a third party observes, never from a successful peer connection.

## Known limits

Transaction IDs come from `RandomState` rather than a cryptographic source.
That is sufficient for matching a reply to its request in a diagnostic tool and
would not be sufficient anywhere in the product.
