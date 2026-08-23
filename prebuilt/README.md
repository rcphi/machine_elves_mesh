# Prebuilt probe

`mesh-probe`, built statically. Committed on purpose.

The whole point of this tool is being handed to someone who should not need a
compiler, and a machine on a phone hotspot or an unfamiliar network can usually
reach a git remote and nothing else. So the binary travels with the source.

Static against musl, so it does not care what libraries the machine has, and it
depends on nothing outside the Rust standard library. Nothing to install.

```
git pull
./prebuilt/mesh-probe                      # one measurement, prints a summary
./prebuilt/mesh-probe --mapping-lifetime   # how long this router remembers
```

For unattended collection instead of a one-off reading:

```
sudo provision/setup.sh --label NAME --dist prebuilt
```

Rebuild and refresh it with `./release.sh`, then copy
`dist/mesh-probe` here. `SHA256SUMS` lets a machine confirm it received what
was sent — and lets two machines confirm they are running the same thing.

`jobs/` carries compiled jobs for the same reason. A machine that runs jobs
never compiles them — it receives WebAssembly and executes it — so a node on a
machine with no toolchain still needs the file, and a git remote is often the
only thing such a machine can reach.

The mesh node itself is deliberately *not* here: 33 MB is too much to keep in a
repository, and unlike the probe it is not something a person runs once to
answer a question. Build it with `./release.sh` and copy it.
