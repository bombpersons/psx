# Template edits

`psx-bevy/` comes from the psx template (`.psx-template.toml` records the
version). Edits to it are allowed under the rules in `psx-bevy/AGENTS.md`;
this file lists them so that `psx template update` stays easy to merge.

```sh
psx template update --dry-run
psx template update
```

Each edited file that the template also changed comes back as
`<file>.psx-new`. Take the new version, reapply the change listed below (or
drop the row if the template now does it), then delete the `.psx-new`.

## Local edits

| File | Change | Why | Raised upstream |
|---|---|---|---|
| _example: `psx-bevy/src/psx/material.rs`_ | _adds a `foo` field to `PsxMaterial`_ | _the original's X needs it (`docs/evidence.md`)_ | _no_ |

## Workarounds

Template limits handled from `engine/` or `game/` without editing it, and
whether they should be raised upstream.

| Limit | Workaround | Raised upstream |
|---|---|---|
