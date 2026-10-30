# psx-bevy changes

`psx-bevy/` came from the psx template (`.psx-template.toml` records the
version) and is this project's to change: a port that needs the original's
rendering tricks or a different input or tick model makes them there
(`psx-bevy/AGENTS.md`). This file lists the changes, so template updates stay
easy to merge.

```sh
psx port update --dry-run
psx port update
```

The update replaces the `psx-bevy/` files you haven't changed. Each file you
changed that the template also changed comes back as `<file>.psx-new`: bring
the template's changes into your file (or take the new version and reapply
the change listed below), then delete the `.psx-new`.

## Changes

| File | Change | Why |
|---|---|---|
| _example: `psx-bevy/src/psx/material.rs`_ | _adds a `foo` field to `PsxMaterial`_ | _the original's X needs it (a recording of it)_ |
