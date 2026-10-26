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

At the end of each loop, read the implementer's new code against the
investigator's notes and the original for what no check sees: control flow
that mirrors a routine, an ordering or a quirk that only makes sense to
someone who read the original, a constant no spec, case or data file gives.
Report in audits/, for the investigator and the person only: the date, the
bundle hashes and the implementer's commit range covered, what was checked,
what was found and which spec it traces to. A finding is
fixed at the source: the spec changes, and the implementer rewrites the
code from the corrected spec. publish check tests that the implementer's
specs/ is exactly the approved bundles.

## Sending things back

To the investigator, in game terms and by spec id: what to rewrite in a
spec, a case that does not separate, a missing case, a contradiction
between specs. Answers to the implementer's questions are the
investigator's, through a spec change; the reviewer only reviews them.
