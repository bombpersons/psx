# Plan: how `protocol next` and `port run` decide who works

`psx port run` starts one agent session at a time, for the role whose
first item of work does not wait on another role. The items come from
`protocol next`'s ledger (`protocol.rs`, `ledger()`). This plan
restructures both so that every item says when it is due, what fact
marks it done and what it waits on. It also fixes the loops the GT2 port
ran into. Nothing in the protocol's roles, files or barrier changes.

## What it does now

`ledger()` (about 700 lines) reads the three repositories and builds three
lists of JSON items (`{"do": "audit_findings", "what": "...", ...}`):

- **Priority** is the order the code pushes items in.
- **Gates** come in three forms:
  - a waiting item placed in the list (`awaiting_review`, `hold`);
  - a waiting item pushed first, followed by `retain` calls that cut the
    list (`wait_for_audit`);
  - an item pushed after the idle item, where it can never be first
    (`read_changes`).
- **Waiting** is decided by name: `waits()` treats `wait_for*`,
  `nothing`, `awaiting_review` and `hold` as waiting.
- **Port run** matches the literal `"cross"` to cross a bundle itself.

`port run` loops:

1. Recompute the lists.
2. Commit any crossings the implementer's repo lacks.
3. Cross an approved bundle.
4. Rotate through the roles (investigator, reviewer, implementer, starting
   after the last one run) to the first whose first item does not wait,
   and run one session for it.

It stops on a failed session, on a session that changed nothing, or at
`max_runs`. The loop itself is sound. The problems are all in how items
are made.

## What goes wrong

1. **Items with no done-check.** `audit_findings` is due while the last
   audit failed, and nothing the investigator does clears it until a new
   audit is recorded. The investigator stages a "clarification" and it
   crosses. Because the specs are already cited, an audit is due
   straight away. The audit fails on the same findings, and the round
   starts again. GT2 went four rounds at about 3M input tokens each, and
   port run's "changed nothing" stop never fires because every round
   commits.
2. **"Implemented" ignores versions.** A spec counts as implemented when
   the code mentions its id, and verified when its cases have passing
   verdicts newer than the case files' mtimes. A crossing that changes a
   spec's text, and not its cases, leaves the spec implemented and
   verified. So:
   - an audit is due before the implementer has seen the change;
   - `read_changes` sits behind `wait_for_specs` and never wakes the
     implementer;
   - an old heldout failure can send the investigator to `thin_specs`
     before the port has been rebuilt against the spec it just changed.
3. **Gates in the wrong order.**
   - During an audit the implementer is meant to keep recording verdicts
     (`retain` keeps `run_cases`), but `wait_for_audit` is first, so it
     never does.
   - A failing case whose spec has an open question from the implementer
     still gives `fix` first, every rotation, while the question waits.
4. **The audit's pass/fail is ambiguous.** "Checks fail, reviewer pass"
   is a state the protocol cannot act on:
   - the copied-text check ignores `audits/allowed.json` in code files
     (only commit messages honour it), so the reviewer's allowances
     never take;
   - findings the investigator cannot fix (broken log chains, unsandboxed
     heldout runs, refused tool calls) still go to the investigator as
     "change the specs".
5. **Times from mtimes.** A case's "last changed" is its file mtime. A
   fresh clone, a checkout or a crossing that rewrites a file moves it,
   which invalidates verdicts that are still good.
6. **One function does everything.** Gathering facts, gating and prose all
   live in `ledger()`, so no rule can be tested on its own.

## The decision: when has the implementer had its turn?

There were two ways to tell that the implementer has caught up with a
crossing: a new implementer commit, or a new case result. **The answer is
results, and once verdicts are versioned it needs no rule of its own.**

A commit is the wrong signal both ways:

- **Not necessary:** a clarification may need no code at all.
- **Not sufficient:** new code with no case run proves nothing.

What the protocol needs after a spec changes is to know whether the port
still meets the spec as it now reads. Only a verdict says that. So:

> **A verdict counts only if it is newer than what it judges:** the case,
> and the spec the case claims. "Newer" is measured against the time each
> crossed in the publishing log, never an mtime.

(The port's code is also judged, but tying verdicts to the code would
make every commit invalidate every verdict. Today verdicts are not tied
to the code, and this plan leaves that alone.)

Everything else follows from that one rule:

- **The stage drops.** A crossing that changes SPEC-0002 makes its verdicts
  old, so SPEC-0002 falls from verified back to implemented until its cases
  run again. That is the truth: nobody has checked the port against the
  new text.
- **The implementer gets one item:** `verify`. It lists the specs whose
  verdicts are old, names `notes/changes/BUNDLE.md` for those whose text
  changed, and says to bring the port in line and run their cases. It
  replaces both `read_changes` and `run_cases`.
  - **Done when:** fresh verdicts exist, whether or not code changed.
- **Implemented for audit** means cited and verified at the current version.
  An audit is therefore never due between a crossing and the
  implementer's turn. No turn-tracking is needed.
- **`thin_specs`** needs a verified spec with a failing heldout verdict.
  Both now have to be fresh, so it waits for the rebuild too. Heldout
  verdicts get the same rule, using their times in `notes/oracle.jsonl`.
- **Format specs' tests** (`round_trip`) and corpus verdicts get the same
  rule. Today they have no freshness check at all.

The audit's subject changes the same way. An audit reviews the
implementer's code, so it is due when there is code no audit has seen:

- code files (`code_files`) changed between the last audit's
  `implementer_commits.to` and HEAD;
- **and** either `audit_every` specs have become verified since the last
  audit, or the implementer has nothing left to do (today's "loop
  ended").

A crossing alone never makes an audit due, and an implementer with
nothing to change never holds one up. One exception: after a **failed**
audit, the next audit is also due once the investigator's correction has
crossed and its specs are verified again, even if no code changed. A leak
the specs no longer give can still sit in the code, and only a new audit
clears the failure. The audit record keeps listing the bundles that
crossed in its range, for the report. Counting moves from bundles to the
commit range.

## The design

### 1. Facts, gathered once

A `Facts` struct holds everything read from the three repositories and
decides nothing. It holds:

- specs and their states;
- each file's last crossing time (from the publishing log);
- verdicts with their times;
- questions and choices;
- the pending bundle and its verdict;
- audit records;
- the implementer's code changes since the last audit;
- inventory, coverage and emulator staleness.

The ledger JSON and NEXT.md are built from `Facts`. Tests build `Facts`
by hand.

### 2. Typed items

```rust
enum Item {
    // investigator
    RebuildStates { chains }, CheckCases { cases }, AuditFindings { audit },
    Revise { bundle, reasons }, Cross { bundle }, RecordAgain { runs },
    UnpackOverlays { regions }, Inventory { .. }, Coverage { .. }, Corpus { runs },
    AnswerQuestions { questions }, AnswerChoices { choices }, ThinSpecs { specs },
    TriageCases { cases }, RecordCases { specs }, Stage { files },
    SpecifyFormats { kinds }, Specify { functions }, Classify { count },
    // implementer
    Fix { failing }, Implement { specs }, Verify { specs, changes }, Idle,
    // reviewer
    Review { bundle }, Audit { bundles, specs, commits },
    // waiting, for any role
    Wait { on: Waiting },
}
enum Waiting { Review(bundle), Audit, SpecsAhead(specs), Answer(questions), Specs, Nothing }
```

Each item has these methods:

- `kind()`: today's `do` string, so the JSON the agents read is unchanged;
- `what()`: the prose;
- `guide()`: its topics;
- `waits()`;
- `automatic()`: true for `Cross`, which port run does itself;
- `key()`: the event it answers (an audit time, a bundle hash, question
  ids), for items that answer one event.

`waits()` in port run and the `"cross"` match go through these methods.

### 3. One ordered list per role, gates as items

There are no `retain` calls. A role's list is its items in a fixed
priority order (written once, per role, as a table in code), with each
gate a `Wait` item at its place. A session starts only for an actionable
first item. Later items stay in the list so the agent sees what is
coming.

| Role | Order |
|---|---|
| investigator | RebuildStates, CheckCases, **Wait(Audit)**, AuditFindings, Revise / Cross, RecordAgain / UnpackOverlays, Inventory / Coverage / Corpus, AnswerQuestions, AnswerChoices, ThinSpecs, TriageCases, RecordCases, Stage, **Wait(Review)**, **Wait(SpecsAhead)**, SpecifyFormats, Specify, Classify |
| implementer | Verify, **Wait(Audit)**, Fix, Implement, **Wait(Answer)** for a failing spec with an open question, Idle |
| reviewer | Review, Audit, Wait(Nothing) |

These are today's orders, with three changes:

- the implementer's verdicts come before the audit gate, as the
  `retain` comment intended;
- `fix` waits on an open question about its spec;
- `read_changes` and `run_cases` become `Verify`.

### 4. Every item says when it is done

The guide gets this table, and a test checks that every `Item` kind is
in it.

| Item | Due when | Done when |
|---|---|---|
| RebuildStates | a start-state chain was made on another emulator version | rebuilt on this one |
| CheckCases | a case has no check on this emulator | checked |
| AuditFindings | the last audit failed with findings that trace to specs | a bundle staged after that audit's time |
| Revise | the pending bundle was rejected | a newer bundle staged |
| Cross (automatic) | the pending bundle was approved | crossed |
| RecordAgain, UnpackOverlays, Inventory, Coverage, Corpus | the inventory or coverage facts that trigger them | those files change (already state-based) |
| AnswerQuestions / AnswerChoices | open | an answer file is written |
| ThinSpecs | a fresh-verified spec has a fresh failing heldout verdict | the spec changes (its verdicts go stale) |
| TriageCases | a case differs on this emulator | recorded again |
| RecordCases | a spec lacks its cases | they exist |
| Stage | spec files differ from what crossed, unstaged | staged |
| Verify | a crossed, cited spec has a verdict older than its case or spec | fresh verdicts |
| Fix | a fresh failing verdict, and no open question on the spec | a fresh pass, or a question |
| Implement | a crossed spec not cited | cited |
| Review | a pending bundle without a verdict | a review is written |
| Audit | see the decision above | an audit is recorded |

### 5. The audit's results route by who can act

- **Content checks** are numbers, addresses and symbols, and copied text.
  The reviewer either allows a finding in `audits/allowed.json` (now
  honoured for copied text in code files too, as it is for names, wherever
  the text appears) or
  fails the audit.
  - `psx audit --judgment pass` refuses while any content finding is
    unallowed, so "checks fail, reviewer pass" cannot be recorded for
    them.
  - A failed judgment goes to the investigator (`AuditFindings`).
- **Process checks** are `logs_intact`, `clean_mode`, `refused_tool_calls`
  and `specs_are_approved_bundles`.
  - When one fails, port run stops and says why. They need the person,
    not a spec change.
  - `crossings_followed_by_questions` goes to the implementer as an item
    (write the question).

### 6. A loop guard in port run

`runs.jsonl` records each session's item kind and `key()`. If port run
is about to start the same role on the same keyed item it just ran, and
that item's done-check still does not hold, it stops: "the investigator
did audit_findings for 20261028T225732 twice and it is still due". With
done-checks this should never fire. If it does, it means a psx bug,
caught after one wasted session rather than four. Unkeyed items
(`Specify`, `Classify`) legitimately take several sessions and are not
guarded. The "changed nothing" stop still covers them.

## Steps

Each step is its own commit, and the larger ones are their own PR.

1. **Scenario tests first.** Build a small three-repo fixture (tempdir)
   and pin today's lists for these scenarios:
   - nothing crossed;
   - a bundle pending;
   - a bundle approved;
   - a crossing of cited specs;
   - an audit due;
   - a failed audit;
   - an open question with a failing case;
   - specs ahead.

   Later steps change these expectations deliberately, one at a time.
2. **`Facts`** out of `ledger()`, with no behaviour change. Crossing times
   come from the publishing log here, and the mtime use goes.
3. **Typed `Item`s** and the per-role order table, with no behaviour
   change. Port run and `waits()` use the methods.
4. **Gates as items:** remove the `retain` calls. The implementer's
   verdicts go before the audit gate, and `fix` waits on its open
   question.
5. **Versioned verdicts and `Verify`:** a verdict counts only if newer
   than its case and spec (cases, tests, heldout, corpus).
   - "Implemented for audit" means verified at the current version.
   - `read_changes` and `run_cases` merge into `Verify`.
6. **The audit:**
   - due by unaudited code, as in the decision above;
   - `--judgment pass` refused with unallowed content findings;
   - process findings stop port run;
   - `AuditFindings` done when staged after the audit.
7. **Port run's loop guard** and the item key in `runs.jsonl`.
8. **Docs:** guide topics protocol (the item table), review (allowances
   and routing), barrier (`Verify` replaces `read_changes`), the role
   templates where they name items, and protocol-plan.md's audit
   paragraph.

## What changes for a running port

- **Old verdicts:**
  - after step 5, verdicts older than a spec's last crossing stop
    counting, so the implementer gets `Verify` for those specs;
  - for GT2 that is SPEC-0001 to 0003, since the clarification bundles
    crossed after its last case run;
  - that is the correct outcome, and costs one implementer session.
- **Audit records:** they already hold `implementer_commits.to`, so
  counting by commit range reads the existing records. No migration is
  needed.
- **Agent prompts:** the `do` strings stay the same, apart from `verify`
  replacing `read_changes` and `run_cases`. The implementer's NEXT.md
  shows the new wording.

## Decisions

1. ~~Allowances everywhere or per file?~~ Settled: everywhere, as names
   are. Text that is harmless in one file is harmless in another.
2. ~~Stop port run on a failed process check?~~ Settled: stop. Each can
   mean work after it is thrown away, and letting other roles go on only
   adds to that. The person fixes or decides, then starts port run again;
   its first step is a new audit, which passes and work goes on, or fails
   and stops again.
