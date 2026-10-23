# psx-bevy changes

The engine is built on [psx-bevy](https://github.com/bombpersons/psx-bevy), a
git dependency pinned to a tag in the root `Cargo.toml`. `psx template update`
moves the tag to the psx build's (`.psx-template.toml` records the version).

```sh
psx template update --dry-run
psx template update
```

When psx-bevy lacks a hook or option, change it in a checkout used through
`[patch]` as psx-bevy's `AGENTS.md` describes, and list the change here.
Once a psx-bevy tag has it, drop the `[patch]` and the row.

## Changes

| Change | Why | Where (branch, PR) | Released in |
|---|---|---|---|
| _example: adds a `foo` field to `PsxMaterial`_ | _the original's X needs it (`docs/evidence.md`)_ | _`my-port-foo` branch_ | _not yet_ |

## Workarounds

psx-bevy limits handled from `engine/` or `game/` without changing it, and
whether they should be raised upstream.

| Limit | Workaround | Raised upstream |
|---|---|---|
