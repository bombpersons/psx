# A port in C

The smallest port psx checks that is not Rust: a sled on a hill, written in
C, with one scenario and one recording.
It shows the contract a port in any language keeps (psx guide --topic
setup, and --topic arithmetic for the numbers):

- **psx-port.toml** names the commands: `make` builds `game/build/sled`,
  and `game/build/sled --psx` speaks the port protocol on stdin and stdout
  (game/src/main.c).
- **The glossary** (glossary/src/lib.rs) is Rust, as in every port: psx
  reads it from source. The game sees only its names and types, over the
  protocol.
- **The arithmetic** (game/src/fixed.h) follows the guide's integer rules,
  and `make test VECTORS=DIR` runs it against `psx port vectors --out DIR`.

```sh
psx port vectors --out .psx/vectors && make -C game test VECTORS=../.psx/vectors
git init && git add -A && git commit -m sled && psx port check
```

The recording (scenarios/hill/push.jsonl) is synthetic: computed by the
sled's rules, not captured from a disc, so the example needs none. A
real port records its scenarios from the original with `psx scenario
record`, which needs the glossary's binding (glossary/binding/) as well.

psx's own tests check this example (crates/psx/tests/c_port.rs): it
passes, a port one unit out fails at the tick it parts, and a port that
does not build says why.
