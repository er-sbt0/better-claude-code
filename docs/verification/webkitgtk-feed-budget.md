# The WebKitGTK budget for §7's feed — partially measured

§4 drew a frame budget for *one* linear feed plus CodeMirror and xterm. §2 has
carried "spike the WebKitGTK performance budget — and re-draw it" as the one
remaining item that **could invalidate a stack decision rather than add work to
it**, and the branching surface made it heavier twice: pan/zoom over a DAG
(`branching-surface.md`, §19), and then *several* virtualized feeds at once,
because the prototype keeps every column mounted and dims the unfocused ones.

This record is **incomplete on purpose**. What was measured is stated as
measured; what was not is stated as not, and the mechanism proposed at the end
is a hypothesis with the experiment that would settle it. §16's rule about
near-certainty applies to our own performance claims as much as to the
provider's help text.

**Measured:** 2026-09-13, WebKitGTK via `python3-gi` (`WebKit2` 4.1) under
XWayland, `AppleWebKit/605.1.15 Version/60.5`. 14 GB machine.
**Harness:** `docs/verification/feed-budget-spike.{sh,py}` +
`apps/desktop/src/bench/`. It builds the app the way it ships (`BENCH=1` adds
`bench.html` to the Vite inputs and changes nothing else), serves `dist/` over
http, and drives a real WebKitGTK window through five camera phases —
`pan-right`, `pan-down`, `pan-back`, `zoom-out`, `zoom-in` — sampling every
frame.

## What was observed

5,000 blocks per column, the mix a real run produces (`bench/synthetic.ts`).

| columns | fps | mean | p95 | p99 | max | frames > 33 ms | DOM nodes |
|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 59.5 | 16.8 ms | 18 | 22 | 31 | **0** | 114 |
| 3 | 57.9 | 17.3 ms | 18 | 32 | 122 | 3 | 306 |
| 12 | 48.9 | 20.4 ms | 33 | 132 | 245 | 16 | 594 |

At 12 columns the phases separate, and the separation is the interesting part:
`zoom-out` and `pan-down` stay clean (16.7 and 16.9 ms mean, **zero** frames
over 33 ms) while `pan-right` degrades (26.5 ms mean, p95 66 ms, 10 frames over
33 ms). Horizontal panning is what moves new columns into view; vertical panning
and zooming move the same ones.

**Virtualization works, and the table is the evidence.** DOM nodes rise from 114
to 594 across a twelvefold rise in mounted feeds — roughly 50 nodes per column,
flat per column, independent of the 5,000 blocks behind each. `windowing.ts`'s
prefix-sum-plus-binary-search is doing its job, and nothing here argues for a
virtualization library.

## What was not measured, and why

- **6 columns.** It completed once, before the numbers were being captured, and
  died on the re-run. Not retried; see below.
- **Memory, properly.** One sample only: **352 MB** of WebKit RSS at 1 column /
  500 blocks.
- **Anything past 12 columns**, any block count other than 5,000, and the
  catch-up cost of resuming a frozen feed.

**Why it stopped.** Repeated runs drove the machine into multi-gigabyte memory
growth and crashed it more than once. The 6-column failure was silent — no
error, no output — which is what the OOM killer looks like. Measuring stopped
there; a benchmark that takes the machine down is not a benchmark, and the
remaining configurations are not worth a desktop session to anyone.

## The hypothesis, and the experiment that would settle it

**Not established. Do not cite this as a finding.**

The memory is not in the DOM and not in JS: DOM nodes stay flat per column, and
the synthetic feed is ~60k small objects at 12 columns — hundreds of megabytes
at the very worst, not gigabytes. What *does* scale with block count is the
scroll geometry. `windowing.ts` estimates a text block at
`24 + ceil(len/64) * 18`, so 5,000 mixed blocks is on the order of **400,000
pixels of column height** — and that height has to exist, because it is what
makes the scrollbar honest.

What composites that height is the **canvas's own pan/zoom transform**, which
every column sits under. A 400,000 × 560 px layer at 4 bytes per pixel is
~900 MB of backing store, per column — which would explain the gigabytes, the
352 MB at one-tenth the blocks, and why *horizontal* panning is the phase that
degrades, since it is the one that brings a new column into view.

**One tempting version of this explanation is wrong and is recorded so it is not
proposed again.** The prototype dims unfocused columns to `opacity: .2`, and
opacity is a compositing trigger — but this build does not do that. `Canvas.tsx`
applies `opacity` and a transition only to the *parent* during the ~900 ms fork
peel (`onParent`), and the branching record deliberately dropped persistent
dimming: "with a real camera the thing that says *this one* is where the camera
is pointed." So unfocused columns are fully opaque and carry no transition, and
the dimming is not what is allocating. Anyone re-proposing `opacity: .2` for the
unfocused state should price it against this record first.

**The experiment:** one column, block counts 500 / 2,000 / 5,000 / 20,000, in a
memory-capped cgroup (a few hundred MB, not gigabytes), recording peak RSS
against DOM nodes. If RSS scales with block count while DOM nodes stay flat, the
mechanism is backing store and the fix is to stop compositing a tall column —
cap the spacer, or dim without `opacity`. If RSS stays flat, the hypothesis is
wrong and the growth is somewhere else entirely.

## What this means for the surface today

- **The budget holds where V1 actually lives.** One to three columns is 58–60
  fps with at most 3 slow frames across five camera phases. §3's "compare
  siblings" is two columns.
- **Twelve columns is usable but no longer smooth** — 49 fps and 16 frames over
  33 ms, concentrated in horizontal pan.
- **The memory ceiling, not the frame rate, is what bounds this**, and it is
  unmeasured. The record's "unmount or freeze unfocused feeds" answer is still
  the obvious one, and still not free: a frozen feed that resumes has to catch
  up on a stream.
- **Nothing here invalidates a stack decision.** §2's worst case does not appear
  to have happened: WebKitGTK renders a virtualized feed at the sizes V1 ships
  at. The open question is how many feeds may be mounted at once, which is a
  question about our compositing and not about the engine.
