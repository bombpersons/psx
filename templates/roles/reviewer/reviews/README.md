# Reviews

One verdict per bundle, `HASH.json`, for `psx publish cross`:

    {"bundle": "HASH", "verdict": "approve", "reasons": []}
    {"bundle": "HASH", "verdict": "reject", "reasons": ["..."]}

The implementer only ever learns that a hash was approved. Never rewrite
a verdict after its bundle has crossed: `psx publish check` records each
review's sha256 and reports a changed one.
