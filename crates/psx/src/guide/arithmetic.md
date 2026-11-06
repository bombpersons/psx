## The original's integer arithmetic

The comparison is exact, so the port computes what the original's
compiled code computed. psx checks only the numbers, not how they were
made. The Rust template's vendor/ has psx-fixed (its `Exact` mode) and
psx-gte, which keep these rules; whatever computes them, `psx port
vectors` tests it.

A number is a raw word of a stated width (i8, u8, i16, u16, i32, u32) and
fraction bits F (0 to 31): the real value is raw / 2^F. Arithmetic is on
32-bit `int`, or on `unsigned int` when an operand is a u32 (narrower
words are promoted to int first); each result is kept to the word's width
as a store keeps it: its low bits, reinterpreted with the word's sign
(`wrap`). In what follows `a` and `b` are raw words, `a` of format (W, F)
and `b` of (W2, G); results are of `a`'s format unless said.

- `add`, `sub`, `neg`: wrap(a + b), wrap(a - b), wrap(-a).
- `mul`: the product's low 32 bits (`mult; mflo`) shifted right by G:
  arithmetically for int, logically for unsigned; then wrap.
- `mul_wide`: the whole 64-bit product shifted right arithmetically by G,
  then wrap (`mfhi:mflo`).
- `div`: n = (a << G) kept to 32 bits, then n / b truncating toward zero,
  then wrap. A zero divisor gives the R3000's results: -1 when n ≥ 0 and 1
  when n < 0 (int), 0xFFFFFFFF (unsigned). INT_MIN / -1 is INT_MIN.
- `mul_int`, `div_int`: `mul` and `div` with an int n and G = 0.
- `rescale` to (W2, G): a << (G - F) when G ≥ F, else a >> (F - G)
  (arithmetic), in 64 bits, then wrap to W2.
- `div_pow2_floor(k)`: a >> (k & 31) arithmetic (rounds toward minus
  infinity). `div_pow2_trunc(k)`: a / 2^(k & 31) truncating toward zero.
  The R3000's variable shifts use the count's low 5 bits.
- `settle_to_zero`: a unchanged (the truncation already settled it).
- `snap_to_unit`: a & ~(2^F - 1). `floor_int`: a >> F. `trunc_int`:
  a / 2^F truncating. Both give an int.
- `abs`: wrap(|a|): the most negative value stays itself.
- Angles are a fraction of a turn with F bits (12: 4096 a turn).
  `normalized`: a & (2^F - 1). `normalized_signed`: the low F bits sign
  extended.
- `sin_quarter` (F = 12): with the 1025-entry SIN_QUARTER table (q = 1024)
  and a' = a mod 4096 (always non-negative): a' < 1024: T[a'];
  < 2048: T[2048 - a']; < 3072: -T[a' - 2048]; else -T[4096 - a']. The
  result is 1.3.12. `cos_quarter`: `sin_quarter` of wrap(a + 1024).

Where integers do not wrap on overflow, or a right shift of a negative
number is not arithmetic, compute in unsigned 32 bits and convert.

## The GTE

The GTE is psx-spx's "Geometry Transformation Engine" (problemkaputt.de/
psx-spx.htm), register for register. Where it leaves room, psx-gte pins:

- MAC1-3 accumulate in 44 bits signed: each step's sum is checked against
  ±2^43 (FLAG bits 30-25 for over and under) and continues wrapped to 44
  bits; `sf` (bit 19) shifts the result right by 12 arithmetically before
  it is kept to 32 bits. MAC0 is checked against 32 bits (bits 16, 15).
- IR1-3 saturate to -0x8000..0x7FFF, or 0..0x7FFF with `lm` (bit 10),
  flagging bits 24-22. In RTPS and RTPT, IR3's flag tests MAC3 >> 12
  whatever `sf`.
- SZ3 and OTZ saturate to 0..0xFFFF (bit 18). SX2 and SY2 saturate to
  -0x400..0x3FF (bits 14, 13). IR0 to 0..0x1000 (bit 12). Colors to 0..255
  (bits 21-19).
- The perspective division is UNR: h ≥ 2·SZ3 gives 0x1FFFF and bit 17;
  else normalize SZ3 so bit 15 is set (z = its leading zeros as 16 bits),
  n = h << z, d = SZ3 << z, u = UNR_TABLE[(d - 0x7FC0) >> 7] + 0x101,
  d = (0x2000080 - d·u) >> 8, d = (0x80 + d·u) >> 8, and the result is
  min(0x1FFFF, (n·d + 0x8000) >> 16). UNR_TABLE[i] = max(0, (0x40000 /
  (i + 0x100) + 1) / 2 - 0x101) for i in 0..=256.
- FLAG bit 31 is set when any of bits 30-23 or 18-13 is. Reading H (control
  26) sign-extends it. Writing LZCS (data 30) sets LZCR to its leading
  zeros, or leading ones when negative.

The libgte-style helpers: `rsin(a)` is the `sin_quarter` rule on a & 0xFFF
and `rcos(a)` is `rsin(a + 1024)`; `mul_matrix(a, b)` sums each element's
three products in 64 bits, >> 12, saturated to i16; `apply_matrix(m, v)`
is each row's sum >> 12, kept to 32 bits; `apply_matrix_sv` saturates
those to i16; `rot_matrix(r)` is `mul_matrix(rot_x(r.x),
mul_matrix(rot_y(r.y), rot_z(r.z)))` with rot_x = [1 0 0; 0 c -s; 0 s c],
rot_y = [c 0 s; 0 1 0; -s 0 c], rot_z = [c -s 0; s c 0; 0 0 1] (1 is
0x1000).

## Test vectors

`psx port vectors` writes, into `.psx/port/vectors/` (or `--out`):

- `fixed.jsonl`: one case a line, a number as `[word, F, raw]`:
  `{"op":"div","a":["i32",12,-8192],"b":["i16",12,0],"out":1}`. `n` is
  `mul_int`'s and `div_int`'s int, `k` the shift count, `to` `rescale`'s
  format; `out` is the result's raw word, or the int.
- `gte.jsonl`: `{"op":"execute","command":...,"data":[32],"ctrl":[32],
  "out":[64]}`: write the data registers, then the control registers, in
  order (null: not written), run the command, and read data 0-31 then
  control 0-31. Then the helpers by name with their inputs.
- `sin_quarter.json` and `unr_table.json`: the tables.

They are made by psx-fixed and psx-gte, so they match this psx. Run them
in the port's own tests: a case that differs names the rule
to fix before any recording does.
