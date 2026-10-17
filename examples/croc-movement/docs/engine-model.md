# Engine model

What the original treats as **data** (the engine interprets it) and what as
**code** (per-type controllers in `game/`), worked out from evidence before
writing gameplay code. Keep it current: it defines the engine's scope.
For each answer, cite the evidence (addresses, tables, traces, `psx` runs).

## World

What is the world made of (rooms and portals, a tile grid, BSP, a heightmap,
free polygon soup)? How are collision and visibility derived from it?

_Not yet established._

## Movement

How do characters move: animation-driven (root motion, state machines in
data) or code-driven physics?

_Not yet established._

## Objects

How are objects defined: a type table with a control function per type?
Which fields come from tables and which are hardcoded?

_Not yet established._

## Level scripting

How do levels script events: trigger tables, bytecode, hardcoded per level?

_Not yet established._

## Frame

How is a frame run: update order, tick rate (`REGION`, `FIELDS_PER_TICK`),
which work happens per field and which per game tick? `engine::schedule`
mirrors the answer.

_Not yet established._
