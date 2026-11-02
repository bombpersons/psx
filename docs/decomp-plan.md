# Plan: the original's code, named, rewritten and checked

Agents port from the original's code, but read it as numbers: Croc's
agent keeps the object's layout in a comment block in `docs/walls.md`,
while each decompile still prints `local_150[local_130 * 2 + 0x13]`. They
also port from Ghidra's decompile, which is often wrong in small ways (a
signed compare read as unsigned, a write through an aliased pointer
dropped), and from their own reading of it. A mistake shows up only when a
feature's recordings fail, many functions and frames later.

This plan gives agents three things, each building on the one before:

1. **Names.** Files the agent edits like any other, naming functions,
   globals, struct fields, parameters and locals. Every decompile, disasm,
   xref, trace and RAM view applies them without being asked. Debug files
   left on the disc fill in what they can.
2. **Checked C.** The agent rewrites a function in C. psx builds it, and
   during a recording runs the original and the agent's version from the
   same moment and compares what each did.
3. **The game running on it.** Every function whose C passes replaces the
   original while the game runs, as re3 and OpenRCT2 did with a DLL. This
   catches what single calls cannot: drift over many frames, and functions
   that are right alone but wrong together.

Under all three, psx works out how the game was built (the SDK, the
compiler and its options, hand-written code) and sets Ghidra up for it,
so the decompile shows less of the compiler and more of the game.

None of this needs the original compiler or matching bytes: behaviour is
what is checked. The C is not the port either. It is the agent's reading
of the original, proven by running it, and the port's Rust is written
from it.

## The trial (2026-11-02)

On a scratch copy of Croc's Ghidra project, only what `docs/walls.md`
already said was applied to 0x80031F64 (the world collision pass): the
object's fields, the area's placements, the floor/ceiling result, and the
names of 9 helpers, 3 globals and 12 locals.

| | before | after |
|---|---|---|
| raw `+ 0x..` offsets | 229 | 58 |
| pointer casts | 474 | 240 |
| unnamed `FUN_` calls | 120 | 0 |
| `->` field accesses | 0 | 221 |

What it took:

- **The function's parameters had to be saved to the database first**
  (`HighFunctionDBUtil.commitParamsToDatabase`); Ghidra had only the
  decompiler's inference.
- **Locals were renamed and retyped** through `updateDBVariable`.
- **A pointer into the middle of a struct needs its own type.** The
  compiler kept `&obj->floor_height` and indexed from there, so Ghidra
  needs an offset pointer (`PointerTypedefBuilder`, DEFAULT,
  `componentOffset(0x38)`), printed as `ADJ(p)->probe_hint[i]`. Without
  it, a third of the accesses stayed arithmetic.
- **Globals in BSS took labels but not types**, since that block holds no
  bytes.

## What the decompile shows now

The decompiles the agents have already asked for (Croc 63, Gran Turismo 2
342), counted:

| | Croc | GT2 | what it is |
|---|---|---|---|
| `unaff_gp` | 682 | 0 | gp unknown: code decompiled from RAM without it |
| `unaff_s*`, `in_*` | 26 | 175 | registers used without being set: wrong function bounds, or hand-written code |
| `setCopReg`, `getCopReg`, `copFunction` | 324 | 33 | GTE instructions, which Ghidra's MIPS knows only as coprocessor 2 |
| `trap(0x1c00)`, `trap(0x1800)` | 130 | 18 | the divide-by-zero and overflow checks GCC's `div` adds, 6 lines each |
| `CONCAT` | 71 | 6 | byte and halfword pieces glued together |

Croc's 0x80031F64 alone has 56 traps in 1278 lines. All of these come
from how the code was built, and knowing it removes them:
`copFunction(2,0x1400006)` is the GTE's `NCLIP`, and gp is set by the
startup code and kept in every overlay.

## The interface

Two directories the agent writes with its own file tools, and four
commands. Wherever psx takes a function or an address, it now also takes
a name: `world_collide`, `0x80031F64`, `RACE.OVL:race_update`,
`croc+0x370`.

| | what it is |
|---|---|
| `names/*.names` | the agent's names, committed |
| `decomp/*.c` | the agent's C, committed; a function named as in `names/` replaces that original |
| `code decompile FN` | the function with every name applied; the agent's C first when it has one, with its last check |
| `code names [QUERY]` | where a name is defined and what it resolves to; with no query, the files' errors and what is most worth naming next |
| `code check [FN...]` | builds `decomp/` and compares each function with the original over recordings |
| `--decomp` on `run`, `scenario check`, `port check` | the game runs with every passing function replaced |

`code annotate` goes. `code symbols` becomes what the disc says about how
the game was built: the PsyQ libraries it names now, plus the compiler and
its options, gp for each module, the functions written by hand, and the
debug files left on the disc, imported to `.psx/names/`.

### Names files

Agents already write what they learn one fact per line (walls.md's
`obj+0x38  i32 floor height (out)`; the glossary binding's
`const CROC: u32 = 0x8007_528C;` with the evidence in a comment). The
names files keep that shape and read like the C the decompiler prints:

```
// Croc's object; the word at 0x8007528C points at Croc's
struct Object 0x988 {
  0x038 int floor_height        // floor under the body, out of world_collide
  0x03E u8 ground_kind
  0x040 ProbeOffset probe_offset[6]
  0x076 u16 wall_bits           // zeroed at 0x80031FCC
  0x084 ProbeHint probe_hint[6]
  0x370 int x
}

global 0x8007528C Object *croc
global 0x80074D08 int speed_x?    // gp+0x1D0

function 0x80031F64 void world_collide(Object *obj, int unk, Area *area)
  local_150 Object *+0x38 obj_38
  local_230 int p_x
function 0x800306B0 floor_find
function 0x80100000 in RACE.OVL race_update
```

- **One fact, one line, in any order.** Struct fields are written with
  their offsets, since agents know offsets and not the original's padding.
  Parts nobody knows are left out; psx sorts the fields and reports
  overlaps.
- **Types read as C, type before name:** Ghidra's base types plus `i8` to
  `u32`, pointers, `T *+0x38` (a pointer into a struct's middle), arrays,
  structs, unions and typedefs.
- **`//` comments hold the evidence.** A function's comment is printed
  above its decompile and a field's beside its declaration.
- **A trailing `?` marks a guess**, and every view keeps it, so a doubtful
  name never reads as a known one. psx sets no rules on naming.
- **Functions are keyed by address**, and by module for overlays, named
  as `code inventory` names them. A function needs only an address and a
  name; a signature, locals and a comment are optional.
- **Locals are keyed by the decompiler's name** (`local_230`, stored as
  its stack offset) or by stack offset or register (`sp+0x230`, `s0`),
  the form debug files use.

### What the agent sees

A decompile, after names, with what is still unnamed below it:

```
// world_collide: the world collision pass (walls.md)
void world_collide(Object *obj, int unk, Area *area)
{
  ...
  if (obj_38->probe_hint[probe].floor_piece < 0) {
  ...
}
unnamed: FUN_80030a10 (4 calls), DAT_80075160 (3), Object+0x7e (2)
names: names/walls.names:14: unknown type "Probehint" (ProbeHint?)
```

A check:

```
world_collide   12 of 14 cases pass   (walls_slide, ledge_drop; HEAD 3f2a1c0)
  case 9  tick 212  obj=croc  area=0x800A3110
    obj->floor_height   original 0x1200  yours 0x1180
    obj->wall_bits      original 0x0004  yours 0x0000
    state: .psx/checks/world_collide/9.state
  branches reached: 41 of 47 (not: 0x800320A8, 0x80032114, ...)
```

A run with `--decomp` says which functions were replaced and the share
of the game's instructions that ran in the agent's C.

## Decisions

- **The agent's files live in the project, committed:** `names/` and
  `decomp/`, arranged however the agent likes. Like the port's code they
  are the agent's own writing; the GitHub example ships none of Croc's.
  Names psx imports from the disc (debug files, PsyQ signatures) are
  derived from disc data, so they go in `.psx/names/` in the same format,
  rebuilt rather than committed. The agent's names win; two of the
  agent's files naming one thing is an error naming both lines.
- **The agent edits with its own file tools.** No psx call writes names or
  C. Feedback comes from the next decompile or check: what did not parse
  (with what was expected) and what did not apply, by file and line.
- **Nothing in the files is machine-written; names bind by content.** The
  inventory knows each function of the executables and overlays by the
  sha256 of its bytes. A function's name applies where the analysed code at
  its address hashes to the function's, so overlays loaded at one address
  are told apart by what they hold, and a name for code that is not there
  (another overlay, or the executable under an overlay) is left out
  quietly. `in MODULE` picks the overlay or executable by its file; a name
  without it is the executable's, and is reported when only overlays have
  a function there and more than one does. A module's globals apply where
  its code is loaded. Later phases use the same check before replacing a
  function with its C.
- **The build is detected from the code, not assumed.** The PsyQ library
  version (from signatures) dates the SDK. `code symbols` adds what the
  code shows: the gp its startup code sets (small data addressed from gp,
  GCC's `-G`) and how often code uses it, GCC's division checks (`break 7`,
  `break 6`) against the divisions, stack frames and frame pointers, the
  share of loads followed by a `nop` (near half when the code was not
  scheduled), and the library functions that write gp, k0 or k1, which
  compiled C never does. Game code is not checked for hand-written
  functions there, since its words cannot be told from data without
  analysis; the decompile covers it instead (below). On the three discs:
  Croc (SDK 4.0) sets gp 0x80074B38 with 2726 gp accesses, 79 zero
  checks in 285 divisions, 47% load nops, and `_patch_gte` hand-written;
  GT2 (4.7) has no gp, 9 checks in 253 divisions and 13% load nops; TRC
  (4.7) sets gp 0x000A2814, 1 check in 115 divisions, 24% load nops. GT2
  and TRC were built with the checks off and better scheduling than Croc.
- **Ghidra is set up from what was found.** The language is
  `MIPS:LE:32:psx` (tools/ghidra/language): MIPS with the GTE's
  registers and commands by name (`gte_set_VXY0(…)`, `gte_RTPS(1,0)`,
  `gte_get_SXY2()`) instead of coprocessor 2's numbers, and GCC's
  division-check breaks as no-ops. psx builds it once per Ghidra
  installation from that installation's own MIPS sources with its sleigh
  (in `.psx/ghidra/language/`) and installs it in each attempt's settings
  directory. gp for the EXE comes from the header, else from its startup
  code (folded to KSEG0); a RAM snapshot's from the run's registers, which
  is right for GT2 (gp 0 at runtime). A decompile that reads registers no
  path sets (`unaff_`, `in_`) says why in a `note`: its start does not
  follow the end of a function (a seed inside a function), or the
  function reads values on purpose or was written by hand. Still to do:
  gp per overlay, and the compiler's struct layout rules for the names'
  structs.
- **Applying names never saves a Ghidra revision.** The query applies
  them in a transaction on the read-only project and decompiles, and the
  changes are discarded. The query code moves into its own Ghidra script,
  so later changes to it do not re-analyse every project.
- **The C's header comes from the names.** psx writes
  `.psx/decomp/names.h` with every struct (padded to its offsets, with
  static asserts), global and signature, and a linker script placing
  globals and the original functions at their addresses. The agent's C
  calls any original function by name, so callees never need decompiling
  first.
- **Any MIPS GCC builds it**, at `-O2 -G0`. psx looks for
  `mipsel-none-elf-gcc` or `mipsel-linux-gnu-gcc`; `psx context` says
  which it found and the setup guide says how to install one.
- **The build is loaded where the game cannot see it:** an address range
  real hardware leaves unmapped, which psx-core maps only when C is in
  use. psx-core sends a call to the build when the CPU reaches the
  original's entry, instead of patching the game's bytes, so an overlay
  loading over the code does not undo it. The game's RAM and layout are
  untouched.
- **A check compares effects, not instructions.** At a call psx keeps the
  machine's state, runs the original to its return and records its
  effects, restores the state, runs the build, records its effects,
  compares, and goes on from the original's result. Effects are what a
  caller could see: RAM bytes left changed, the return registers, `sp`
  and the saved registers, the GTE's registers, hardware register writes
  in order, and the functions called with their arguments. Writes an
  interrupt handler makes meanwhile are left out; time is not compared.
- **Hand-written functions can still be rewritten in C**, but one that
  does not follow the calling convention cannot replace the original; the
  check reports the registers it relies on.
- **A check's cases are calls spread over the recordings given** (by
  default those of the features whose scenarios reach the function, up to
  a set number). A failed case is saved as a state at the call, so the
  agent can `run --state` it or check again without replaying.
- **`--decomp` replaces only functions whose last check passed** on the
  current C and names; `--decomp all` or a list replaces others too.

## Steps

One commit per step; `cargo test --workspace` and clippy pass after each.
Each phase is useful on its own, and the measure at its end decides
whether the next is worth building.

### Phase 1: names, and the build

1. **The parser.** It reads `names/` and `.psx/names/` into one model:
   types, globals, functions with signatures and locals, and comments.
   Errors give file, line and what was expected; the checker reports
   overlapping fields, duplicates and unknown types.

   *Done when* unit tests cover each kind, including the offset pointer,
   a guess, an overlay function, an overlapping field and a duplicate
   across two files.

   *Done (2026-11-02):* `psx_symbols::names`, with 14 unit tests.

2. **How the game was built, and Ghidra set up for it.** `code symbols` reports the
   SDK, compiler and options with their evidence, gp per module, and the
   hand-written functions. The setup script uses them: gp everywhere, the
   GTE by name, the division checks folded, hand-written functions marked.

   *Done when* on Croc's and GT2's earlier decompiles, redone on scratch
   copies, `unaff_gp`, `trap(0x1c00)`, `trap(0x1800)` and coprocessor 2's
   generic calls are gone, every remaining `unaff_`/`in_` is in a function
   reported as hand-written or wrongly bounded, and the three discs report
   a compiler and SDK that agree with each other.

   *Done (2026-11-02), with one change:* psx decompiles with its own
   language, `MIPS:LE:32:psx`, built from tools/ghidra/language on first
   use, and takes gp from the startup code when the header has none.
   Croc's 55 earlier decompiles, redone: `unaff_gp` 554 to 0, traps 73 to
   0, coprocessor 2's generic calls 324 to 0, unset registers 580 to 0.
   GT2's 107: traps 9 to 0, the generic calls 31 to 0, unset registers
   73 to 67, all in five functions that carry a note (three start inside
   another function). Unset
   registers have a third cause besides the two expected: ordinary C
   reading a value no path sets (GT2's 0x8007A938), so the decompile's
   `note` says which of two things it is, by whether the function starts
   right after another one ends; "hand-written" is reported only for
   matched library code (Croc's `_patch_gte`). The SDKs (4.0, 4.7, 4.7)
   agree with each build's habits; gp per overlay is still to do.

3. **Decompiles apply them.** The query request carries the model, and a
   new query script saves the decompiler's parameters where none are
   stored, applies types, names, signatures, global labels (and types
   where the memory holds bytes) and locals, and decompiles with the
   comments. The cache is keyed by the names' hash too. The result lists
   errors and names that did not apply.

   *Done when* Croc's 0x80031F64, with a names file written from walls.md
   on a scratch copy, decompiles as the trial's did (no more than 58 raw
   offsets, every call named), and a planted typo is reported by line.

   *Done (2026-11-02):* the query script applies the names in its
   read-only session, so nothing stays in the Ghidra project, and the
   result's `names` counts what applied and lists what did not. On scratch
   Croc, walls.names gives 0x80031F64 64 raw offsets (the trial's 67 by the
   same count), no `FUN_`, 10 functions, 14 locals, 7 types and 3 globals
   applied; a planted `Probehint` is reported once, at its line. Ghidra
   prints names as identifiers, so a guess's `?` is put back by psx and the
   offset pointer reads `Object_ptr_0x38` (with `ADJ(obj_38)->field`); a
   typed global past the EXE's image gets memory for the query alone. Names
   bind by content against the inventory (above): on GT2, three names `in
   GT2.OVL` apply in a RAM analysis with the overlay loaded and not in the
   executable's, and the executable's name at an address the overlay shares
   applies only in the executable's; `in` with a module the inventory has
   no function of at that address is reported by line. Without an
   inventory, `in` matches the analysed executable's file name.

4. **Names everywhere.** Function and address arguments take names
   (`world_collide`, `croc+0x370`, `RACE.OVL:race_update`). `code disasm`,
   `code xrefs`, `code functions`, `code inventory`, `code coverage`,
   run's traces, samples and watches, and `ram view` show names by
   default; `--symbols` still adds files. `code annotate`, `annotations`
   in manifests and `export.symbols.json` go.

   *Done when* a traced run on scratch Croc prints `world_collide` with
   no `--symbols` given, and nothing refers to the old annotate.

5. **What to name next.** The decompile lists unnamed callees, `DAT_`
   globals and offsets on typed pointers that fall in no field, by how
   often each appears. `code names` with no query does the same over the
   functions the port's recordings reach, weighted by how often they run;
   with a query it gives the definition's file and line and what it
   resolves to.

   *Done when* 0x80031F64's decompile lists the polygon record's offsets.

6. **Coverage on the C, and the gaps grouped.** Finding why a branch no
   recording took cannot be reached is where Croc's agent spent much of
   its time: 183 wont_reach entries, written one branch at a time, with
   22 scripts of its own over coverage's missed ways and its list.
   They have 48 distinct causes; one, analog stick mode
   (0x8007529C == 2), answers 37 of them. So:
   - The decompile keeps each line's instruction addresses beside its
     pseudocode. `code coverage` with a function gives its C with each
     line's runs and each condition's ways seen (never true, never
     false), in names once step 3 applies them. Ghidra may print a
     condition negated with its branches swapped; the mark follows the
     printed condition.
   - GCC's division checks (`break 7`, `break 6` and the branches to
     them) are no gaps: 104 of Croc's 2438 blocks never run and 160 of
     its 1931 ways never taken.
   - A way never taken answers for the code only it reaches (its
     dominance in the function's graph, exact in compiled code), so the
     gap list gives that way once, not every branch behind it.
   - The ways still unanswered are grouped by what they test: the same
     memory and comparison, or the same C condition. port next gives a
     feature's open branches as those groups, with how often each
     reached, and wont_reach takes a condition (`0x8007529C == 2`) that
     answers for every branch testing it, checked when the port is.
   - The values a group tests, as the recordings saw them, sampled at
     the branch in a traced run (`0x8007529C` was 0 in all 412 runs): the
     evidence a reason needs, without a session of `ram view`.

   *Done when* on scratch Croc the open branches of F-0001 come as groups
   whose count is near its 48 causes, its analog stick entries fold into
   one, and 0x80031F64's coverage reads as marked C.

7. **The disc's own names.** `code symbols` scans the whole disc for
   debug leftovers: files by name (`.SYM`, `.MAP`, `.ELF`, `.CPE`,
   sources) and by content (SN SYM's `MND` header, ELF headers, psylink
   map headers) inside every file, archives included, and in sectors no
   file owns; strings that look like source paths; `GCC:` compiler
   strings beside the PsyQ version it already gives. A debug file that
   describes this EXE (most of its functions start at function starts)
   is imported to `.psx/names/<source>.names`: SN SYM with its types,
   locals and line numbers, MAP names, ELF symbols and DWARF types. PsyQ
   signature names move there too. The SYM layout is checked against a
   public parser (for example ghidra_psx_ldr).

   *Done when* the three discs report no debug files, and test images
   with planted files are imported, matching and not matching.

8. **The flow, and a measure.** `port init` runs the scan; a found debug
   file gives a Read item in `port next`. The guide's code topic shows the
   format with one example and sets no naming rules; the template's
   AGENTS.md mentions `names/`. Then one agent session on a scratch copy
   of a port against its earlier logs: decompiles per function, time per
   feature, calls and tokens per wont_reach answered, and whether the
   agent moved its field maps into `names/`.

   *Done when* the numbers are in front of the person.

### Phase 2: checked C

9. **The header and the build.** From the names model, the header with
   padded structs and offset static asserts, and the linker script. `code
   check FN` compiles `decomp/` with it, links at the hidden range and
   reports compiler errors by file and line.

   *Done when* the scratch Croc names compile into a header with every
   offset asserted, and a C version of a small Croc helper builds.

10. **One call, compared, in psx-core.** Keep and restore a machine state
   in memory, map the hidden range, send one call to the build, run to the
   return (the return address reached with `sp` restored, or a step
   limit) and collect the effects.

   *Done when* tests on a test EXE pass an equivalent function and report
   a wrong byte, a wrong return value, a missing call, a clobbered saved
   register and a loop that never returns.

11. **Checks over recordings.** `code check` replays the recordings
   (`--scenario`, `--recording`, `--state` choose others), runs up to
   `--cases` cases, and reports as shown above: cases passed, each
   failure's first differences through the names, the call's arguments,
   its saved state, and the original's branches the cases reached. With no
   function it checks every function in `decomp/` whose C or names changed
   since its last check.

   *Done when* a correct Croc helper passes over its recordings and one
   with a flipped sign fails with the field named.

12. **The decompile shows the C.** `code decompile` on a function with C
    returns it with its last check (cases, result, commit, recordings),
    Ghidra's below. `code inventory`, `port status` and the Stats tab
    count functions named, in C, and passing.

    *Done when* 0x80031F64 decompiles to the agent's C once it passes.

### Phase 3: the game running on it

13. **Replacement for whole runs.** psx-core sends every call of each
    replaced function to the build for the whole run, overlays only while
    their bytes match. `--decomp` on `run`, `scenario check` and `port
    check` turns it on. The result names the functions replaced and the
    share of instructions run in the C, and a recording that diverges
    with the C but not without it says so, with the first tick it
    differs.

    *Done when* a scenario on scratch Croc checks the same with
    world_collide replaced as without, and a planted mistake in its C
    makes it diverge.

14. **The person sees it.** psx web plays the original with the agent's C
    (a toggle beside each recording and in play), draws a check's card in
    the live log (cases as a strip of passes and failures, the first
    difference), and gains a Code view: functions with their names, C and
    check state, the decompile, and the share of the game in the C over
    time. `port next` gives a Fix item when a function that passed fails
    after a change to its C or names.

    *Done when* the view shows scratch Croc's checked functions and a
    recording plays with them replaced.

15. **Measure it.** One agent session on a scratch copy of Croc with all
    of the above: functions checked, cases failed before passing (each a
    mistake caught before the port), and whether the feature's recordings
    then pass in fewer tries than in its earlier logs.

    *Done when* the numbers are in front of the person.

## Not in this plan

- Building with the original compiler (PsyQ's GCC, once identified) to
  measure how close a function comes to matching.
- Porting by the C itself: replacing every function, then building the C
  natively with a stand-in for the PS1's libraries (GPU, GTE, CD, SPU).
  It gives a faithful PC build mechanically, but with the original's
  structure rather than a Bevy game's.
- Using a check's cases as tests for the port's Rust: they hold inputs
  and effects in the original's memory layout, which the port does not
  share.
- Generating the C from Ghidra's decompile as a starting point that
  builds.
- Generating a port's glossary consts from `names/`, so a fact is not
  kept twice; phase 1's measure shows whether it is needed.
- Splitting the EXE back into its source files, or matching names from
  another build of the game (a prototype's SYM) by function bytes.
