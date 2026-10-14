# Native core (`psx-core`): stage 1, the CPU

## What exists

- **R3000A interpreter.** It implements:
  - load and branch delay slots, including LWL/LWR merging with an in-flight load and the rule that a later write beats a pending load;
  - COP0 exceptions with the KU/IE stack, RFE, and EPC/BD for faults in delay slots;
  - interrupts gated by SR.IEc/IM and Cause.IP2, completing any in-flight load when an exception pre-empts an instruction;
  - cache isolation (stores dropped while SR.IsC is set);
  - overflow, address-error, reserved-instruction and coprocessor-unusable exceptions;
  - COP2 through `psx-gte`.
- **Bus.** RAM with mirrors, scratchpad (not reachable through KSEG1), BIOS ROM, expansion 1 (reads 0xFF), cache control, and an I/O register file
  that logs every access.
- **Devices so far:**
  - Interrupt controller: I_STAT acknowledge semantics, I_MASK, and the IP2 line.
  - DMA controller: starts transfers on CHCR, and implements channel 6 (ordering-table clear) exactly. Other channels are recorded as unmodelled.
- **Reset state.** SR starts at `0x10900000` to match Redux. Most SR bits are undefined on hardware at reset.
- **Not modelled yet:** instruction cache, real timing (one cycle per instruction), the GTE-command vs interrupt quirk, timers,
  GPU, CD-ROM, SPU, MDEC, pads and memory cards.

## Validation: `psx lockstep`

`psx lockstep --bios <bios> --steps N` streams PCSX-Redux's `-trace` output (interpreter + debugger) and,
**before every instruction**, checks:

- the PC;
- the instruction word;
- every source-operand register value that Redux prints.

Two things are injected and counted, never hidden:

1. **Hardware values.** When a register loaded from I/O space is next used, the core adopts Redux's value and writes it into its I/O register file.
   Redux's trace memory column for I/O is a side-effect-free peek, not the value the hardware returned, so the adoption waits until the register is used.
2. **Interrupts.** When Redux enters `0x80000080` or `0xBFC00180` and the core did not, the core takes an external interrupt and holds IP2
   until software acknowledges I_STAT.

`--exact` single-steps Redux over GDB and compares the full register file (pc, r1–r31, hi, lo) after each step.
It runs at about 20 steps/s, so it is for close inspection of a divergence.

## Results (SCPH-7502 BIOS, no disc)

The table records what each run surfaced and the fix it led to.

| Run | Matched instructions | Outcome |
|---|---|---|
| first | 7,814,488 | SPU status read: the trace peek shows `0000` but the register received `0x0800`. Fix: lazy adoption of I/O values |
| second | 24,344,343 | `mfc0 Cause` in the IRQ handler: IP2 was not held after a followed interrupt. Fix: minimal interrupt controller |
| third | 28,953,890 | RAM written by DMA channel 6 (ordering-table clear) was missing. Fix: DMA controller with OTC |
| fourth | **100,000,000**, no divergence | 105,501,945 operand values checked. 681 interrupts followed and 5,787 I/O values adopted. ~530k instructions/s compared |

The only unmodelled DMA in 30M instructions is channel 2 (GPU, RAM→GPU), which writes nothing to RAM.

## GPU wiring: the BIOS boot logo from the native core

The GP0/GP1 ports and DMA channel 2 (block and linked-list) now feed `psx-gpu`. GPUSTAT is plausible but untimed;
in lockstep its reads are adopted from Redux like any other I/O.

After 100M lockstepped instructions (no divergence, `--dump .psx/lockstep/bios100m`), the core's VRAM holds the
**Sony Computer Entertainment logo**. It was drawn entirely by the native CPU, DMA and `psx-gpu` from the real BIOS's commands.
The comparison is against Redux's VRAM at vsync 350, while the same logo is static on screen:

| Region | Pixels differing |
|---|---|
| Texture pages (x ≥ 640) | **0 / 196,608** (uploads are byte-exact) |
| Framebuffer (640×512) | 2,150 / 327,680 (0.66 %) |

Every differing pixel lies inside the **Gouraud-shaded diamond** and is one 5-bit step lower in green, in vertical
stripes. Flat fills, textured text and the logo textures all match exactly. The difference is therefore
colour-interpolation rounding in `psx-gpu`.

The rounding has not been tuned to Redux: Redux's software GPU is another emulator, not hardware truth. The referee should be
a hardware-verified Gouraud test (e.g. JaCzekanski ps1-tests `gpu/`) or DuckStation regtest output.

## Next

- **GPU wiring:** send GP0/GP1 port writes and DMA channel 2 (block and linked-list) into `psx-gpu`, then compare the BIOS screen with Redux's.
- **Self-contained devices:** timers, then the CD-ROM controller, so that injection counts fall towards zero.
- **Game code:** lockstep from a side-loaded EXE (the probe), then from boot of Croc.
