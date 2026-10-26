# Audits

One report per audit: the date, the bundle hashes (in full) and the
implementer's commit range it covered, what was checked and what was
found, and which spec each finding traces to. `psx audit` with your
judgment records it once a report here names every bundle it covers:

- `psx/`: psx's records, its checks with the hashes and commits each
  covered. `psx protocol next` counts a bundle audited when a record with
  a judgment names it;
- `allowed.json`: numbers and names psx's checks flag that you traced
  and let through, each with its reason,
  `{"numbers": {"5000": "a test point outside the room"}, "text": {}}`.

Reports stay here; the port's AUDIT.md gets the date, the commits and the
verdicts only.
