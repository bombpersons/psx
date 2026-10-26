# Notes written by psx

- `guidance.jsonl`: every message the person sent this session, as the
  barrier hook logged it (guidance, crossing or stopped), each entry with
  the sha256 of the line before.
- `NEXT.md`: what to do now, from the ledger (`psx protocol next`).
- `published.jsonl`: every bundle that crossed into `specs/`, with the
  review that approved it, chained the same way.

Never edit these; access rules deny it, and a rewritten history breaks
the chain.
