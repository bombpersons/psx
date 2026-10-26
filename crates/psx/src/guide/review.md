The reviewer is the go-between in a clean-room port (guide topic
cleanroom): nothing the investigator writes reaches the implementer without
its approval of that exact bundle. It reads everything, the original
included, so it stays on the investigator's side for good.

## What it writes, and to whom

- To the implementer: only an approval of a bundle's hash, reviews/HASH.json
  with verdict approve. One bit, carrying nothing. Never advice, not even on
  architecture: a dirty agent's advice on structure could carry the
  original's structure.
- To the investigator and the person: rejections with reasons that say what
  to change, and audits, all in the reviewer's repository.
It never writes specs, never stages or crosses a bundle itself, and never
starts another role's session.

  {"bundle": "HASH", "verdict": "reject",
   "reasons": ["SPEC-0004 'Turning': the second paragraph walks through the
   routine's checks in order; say what the player sees instead"]}

## Reviewing a bundle

publish stage writes the bundle to the investigator's .psx/bundles/HASH/:
bundle.json (each file, the version it replaces, what the checks let
through) and files/. The checks have already refused addresses, hex,
code-shaped text, tool and symbol names, and text copied from the notes.
Read for what no check sees:
- a routine paraphrased in plain words: control flow ("first it checks,
  then it loops over"), the original's order of operations or data layout
  where the behaviour does not require them;
- constants that come from the code rather than from observation or the
  game's data, and values from the data not named as such;
- names that echo the original's identifiers;
- structure dictated rather than behaviour described: a spec per function,
  or advice on how to build the port;
- claims without a case, and cases that do not tell the rules apart (guide
  topic protocol, "Making cases that separate"); a compared column no spec
  claims;
- a rule stated in two places in different words: they drift into
  contradictions. One place, referred to from the other;
- a behaviour that relies on a data value without naming it (an animation's
  effect number, a flag in a level record): the implementer would have to
  find it alone;
- table contents from the executable, rather than a layout;
- anything under let_through in bundle.json.
Compare each spec with the investigator's notes and notes/provenance.json
for what it came from, and with the original itself when in doubt. Approve
only the whole bundle; the investigator crosses it with publish cross.
Never rewrite a verdict after its bundle has crossed: the publishing log
keeps each review's sha256, and publish check reports a changed one.

## Audits

When protocol next says one is due (every few implemented specs, and at a
loop's end), neither other role gets new work until it is recorded.

1. audit: psx's checks. The implementer's specs/ is exactly the approved
   bundles; every number in its code appears in a spec or a case (or
   psx's template); no addresses, symbol names or the executable's name,
   and no eight words in a row from the investigator's notes, in its code,
   comments or commit messages; every message the person sent with
   [cross] was followed by a question; the publishing, guidance and
   clean-mode logs' chains hold and the history the last audit ended at is
   still there; its session transcripts show no tool call the host refused
   (Claude Code's are found by its folder; transcripts gives others).
2. Read each finding at its source. A number traced to the game's data,
   or harmless (a test's arbitrary point), goes in audits/allowed.json
   with the reason: {"numbers": {"5000": "a test point outside the
   room"}, "text": {"PSX.EXE": "the console's default boot name"}}.
   Allow nothing you cannot explain without the original's code.
3. Read the implementer's new code against the investigator's notes and
   the original for what no check sees: control flow that mirrors a
   routine, an ordering or a quirk that only makes sense to someone who
   read the original, a constant from the code rather than the data.
4. Report in audits/, for the investigator and the person only: the date,
   the bundle hashes audit listed and the commit range, what was checked,
   what was found and which spec it traces to.
5. audit with judgment pass or fail records it: audits/psx/ keeps psx's
   checks with the hashes and commits they covered, and the port's
   AUDIT.md gets a line of date, commits and both verdicts, no reasons.
   It needs your report to name every bundle first.

A finding is fixed at the source: the investigator changes the spec (next
gives it a failed audit as work), and the implementer rewrites the code
from the corrected spec. publish check tests the specs alone, any time.

## Sending things back

To the investigator, in game terms and by spec id: what to rewrite in a
spec, a case that does not separate, a missing case, a contradiction
between specs. Answers to the implementer's questions are the
investigator's, through a spec change; the reviewer only reviews them.
