# Engine model

What the game treats as **data** (the engine interprets it) and what as
**behaviour** (per-type rules in `game/`), as the specs and the data files
show it. Keep it current: it defines the engine's scope. For each answer,
cite the specs and data formats (`FMT-NNNN`) behind it.

## World

What is the world made of (rooms and portals, a tile grid, a heightmap,
free polygons)? How are collision and visibility derived from it?

_Not yet established._

## Movement

How do characters move: driven by animation data, or by rules per tick?

_Not yet established._

## Objects

How are objects defined: which fields come from the data files, and which
behaviour the specs give per type?

_Not yet established._

## Level scripting

How do levels script events: trigger tables in the data, or rules per
level in the specs?

_Not yet established._

## Frame

How is a tick run: update order, tick rate (`REGION`, `FIELDS_PER_TICK`),
what happens per field and what per tick? `engine::schedule` mirrors the
answer.

_Not yet established._
