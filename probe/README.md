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

## What it deliberately does not test

- **Firewall filtering.** A machine may hold a global IPv6 address and still
  refuse inbound packets. Determining that needs a cooperating peer, which is
  Phase 0 proper.
- **Sustained behaviour.** Consumer routers drop idle mappings after a timeout
  that this probe is too short to observe.
- **Throughput or latency under load.**

## Known limits

Transaction IDs come from `RandomState` rather than a cryptographic source.
That is sufficient for matching a reply to its request in a diagnostic tool and
would not be sufficient anywhere in the product.
