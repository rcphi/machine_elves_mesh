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

It prints an endpoint id. Then on yoinkerz, on the hotspot:

    ./iroh-probe connect <that-endpoint-id>

Both sides watch for a minute and print a verdict.

## Reading the result

Each side prints its own addresses, then what is actively carrying traffic.

- `ACTIVE ["Ip(...)"]` — direct. Hole punching worked where ours could not, and
  a mesh could form with nobody having touched a router.
- `ACTIVE ["Relay(...)"]` for the full minute — no direct path. iroh is no
  better placed here than we are, and adopting it would buy a relay dependency
  rather than remove one.

`also known: []` alongside a relay verdict means no direct address was ever a
candidate, so hole punching was never attempted rather than attempted and
failed. Check the `own addresses` lines: if both show the same public address,
the two machines are behind one NAT and the run tells you nothing.
