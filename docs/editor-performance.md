# Large text editor benchmark

This records the original baseline and subsequent editor optimizations. The opt-in
benchmark lives in `crates/pico-app/src/growing_editor_benchmark.rs`. It exercises
the real iced editor and Pico's scrollbar measurement without the surrounding
transcript or questionnaire UI. Original results are retained below for comparison.

## Reproduce

```sh
# All cases, three samples each, optimized build:
nix develop --command ./scripts/test-renderer.sh \
  cargo test --locked --release -p pico-evo editor_stage_benchmark \
  -- --ignored --nocapture --test-threads=1

# The smoke test's 700-line answer, unoptimized build:
nix develop --command env PICO_EDITOR_BENCH_CASE=unicode-700 \
  PICO_EDITOR_BENCH_SAMPLES=1 ./scripts/test-renderer.sh \
  cargo test --locked -p pico-evo editor_stage_benchmark \
  -- --ignored --nocapture --test-threads=1
```

`PICO_EDITOR_BENCH_CASE` selects one exact case name; an unknown name is an error.
`PICO_EDITOR_BENCH_SAMPLES` is a positive sample count (default 3). Output includes
CSV rows with payload bytes, logical lines, edit time, native layout time,
scrollbar measurement time, and native cached logical-line count. Preserve the
complete stdout/stderr when redirecting a run. Avoid concurrent builds/benchmarks.

The ignored benchmark is compiled but not executed by ordinary tests. Assertions
check lossless text after paste and edits, actual overflow, and revealing the
paste's final cursor; there are no machine-dependent timing assertions.

## Method

- Local AMD Ryzen 7 5800X3D; pinned Nix development environment and Cargo profiles.
- Renderer/device and a short Unicode font/layout warmup precede measurement.
  Each sample has fresh content, widget state and scrollbar cache. These are warm
  process measurements, not cold application startup measurements. Additional
  glyph/cache work can still vary between samples.
- Explicit fixture: 15 px text, 900 px outer width, 160 px editor height, wrapped
  text, production padding/gutter. Resize uses 420 px width. These are controlled
  fixture settings, not assertions about application defaults or desktop scaling.
- **Unlaid:** paste before the editor has ever been laid out, as in smoke stage 30.
  **Mounted:** lay out the empty editor before pasting, as with a visible composer.
- **Edit:** `Content::perform`; payload generation/allocation is excluded.
  **Native:** widget construction, tree diff and native `TextEditor::layout`.
  **Scrollbar:** the actual `State::measure`, after native layout, matching the
  production order. Diagnostic cache inspection and assertions are outside timing.
- Original phases: paste, unchanged layout, insert `x` at the paste tail, resize, then move
  to document start and insert `y`. `type-start` includes navigation, not just
  insertion. Many-line fixtures end in a newline, so tail typing edits a new line;
  start typing edits an existing nonempty line. Single-paragraph edits affect the
  entire logical line.
- ASCII, Greek-only, CJK-only and mixed-Unicode 700-line controls; ASCII/mixed
  7,000-line inputs; matched ASCII/Unicode single-paragraph fixtures at 700 and
  7,000 repetitions. Payload size is reported in bytes, not inferred from names.
- The test renderer uses Lavapipe, but there is **no drawing, presentation or GPU
  readback in the measured regions**. This isolates CPU editing/layout costs; it
  is not a GPU throughput benchmark.

## Baseline results

Release results below are **medians of three samples**, in milliseconds. The
many-line Unicode fixture is the smoke answer repeated with `λ界`. Each many-line
fixture has one additional empty logical line because it ends with a newline.

| Payload | Initial state | Bytes | Paste edit | Native layout | Scrollbar |
| --- | --- | ---: | ---: | ---: | ---: |
| ascii-700 | unlaid | 36,400 | 0.74 | 30.09 | 0.59 |
| ascii-700 | mounted | 36,400 | 0.75 | 0.22 | 23.92 |
| unicode-700 | unlaid | 38,500 | 0.74 | 556.17 | 1.21 |
| unicode-700 | mounted | 38,500 | 0.75 | 4.88 | 545.83 |
| ascii-7000 | mounted | 364,000 | 82.59 | 0.24 | 244.46 |
| unicode-7000 | unlaid | 385,000 | 84.93 | 5809.55 | 20.79 |
| unicode-7000 | mounted | 385,000 | 83.80 | 5.24 | 5579.37 |
| paragraph-ascii-7000 | mounted | 329,000 | 0.20 | 244.49 | 8.61 |
| paragraph-unicode-7000 | mounted | 350,000 | 0.22 | 5500.69 | 13.91 |

Other useful observations:

- In the mounted `unicode-700` resize, native layout took 0.01 ms but scrollbar measurement took 546.81 ms.
- An unchanged mounted `unicode-7000` layout took 0.005 ms natively plus 0.461 ms for the scrollbar.
- Native initial layout for the 700-line ASCII / Greek-only / CJK-only controls was 30.09 / 24.73 / 554.38 ms.
- Native layout cached all 701 logical lines for the unlaid 700-line input, but only 7 for the mounted input before scrollbar measurement.

Single-paragraph **tail typing after paste**, mounted editor (one character):

| Payload | Edit | Native layout | Scrollbar |
| --- | ---: | ---: | ---: |
| paragraph-ascii-700 | 0.003 | 22.95 | 0.56 |
| paragraph-unicode-700 | 0.007 | 546.24 | 1.44 |
| paragraph-ascii-7000 | 0.038 | 240.48 | 26.03 |
| paragraph-unicode-7000 | 0.037 | 5505.42 | 34.99 |

The focused **debug** run (one sample per initial state) reproduced the smoke
pause: `unicode-700` took **12,266.97 ms native + 9.15 ms scrollbar** when unlaid,
versus **105.03 ms native + 12,190.13 ms scrollbar** when mounted. Paste insertion
itself took about 2.3 ms. This corrects the earlier hypothesis that scrollbar
measurement alone explained the collapsed smoke frame: that specific transition
is dominated by native layout, while the visible-composer path hits the scrollbar.

The baseline exposed three costs: shaping with unbounded dimensions before the
first layout, measuring offscreen rows for the scrollbar at mounted widths, and
repeatedly shifting lines during multiline insertion. CJK was slower than the
Greek and ASCII controls on this font setup; these stage timings alone do not
separate fallback from shaping. They exclude clipboard transfer, persistence,
peak memory and frame presentation.

## Implemented optimizations and remeasurement

- Initial content is stored without shaping; native layout applies metrics and viewport bounds together.
- Multiline insertion preserves the original line/attribute construction but splices the new lines in one operation.
- The scrollbar reads native cached row counts and estimates unmeasured lines. No full-document shaping or duplicate text/layout cache is needed. A cheap linear scan of logical-line metadata remains.
- Drag/track requests seek directly to a logical line/fraction in application-message order. They shape the destination logical line and viewport, not the intervening lines. Estimated heights refine without changing the native text anchor; top/bottom seeks are exact.
- Preferred script/common font families accept their nearest weight instead of requiring an exact match. A local diagnostic trace previously attempted **225 fallback faces** for one `界` run before finding a usable face. Timing instrumentation was temporary and is not in production.
- The native shape-run cache is enabled with direction-aware keys, font-database invalidation, a 4 MiB owned-payload budget and a 4,096-entry cap. This reuses unchanged word/run shaping while preserving advanced Unicode shaping. Large runs that exceed the cache budget remain fully available but are not cached.
- An edit followed by a seek before layout can leave an offscreen caret unshaped. Drawing now returns no paint geometry for that case instead of panicking or shaping the caret line. IME stays enabled with its existing viewport-origin anchor; composition/commit reveals the caret normally.

Same machine, profiles, geometry and original ten fixtures. Release values below
are **medians of total edit + native layout + scrollbar time** across three samples,
in milliseconds. The new cache persists within the benchmark process, as it does
in the application; these are not cold-start or cold-font measurements.

| Operation | Before | After |
| --- | ---: | ---: |
| Paste 700 Unicode lines, new editor | 558.144 | 0.151 |
| Paste 700 Unicode lines, mounted editor | 551.518 | 0.112 |
| Paste 7,000 Unicode lines, mounted editor | 5,668.662 | 1.786 |
| Resize 7,000 Unicode lines, mounted editor | 5,545.633 | 0.040 |
| Type one character in 329 KB ASCII paragraph | 266.865 | 32.025 |
| Type one character in 350 KB Unicode paragraph | 5,540.442 | 35.075 |

The focused debug smoke-input case dropped from about **12.28 seconds to 2.55 ms** for initial paste/layout. The newly laid-out multiline case caches 14 native logical lines rather than all 701; the mounted case caches 7.

### Remaining limit after the first optimization: huge single logical lines

A new **294,000-byte distinct-word paragraph** deliberately exceeds the useful working set of the word cache. It takes **80.1 ms** for paste/layout and **81.0 ms** for a tail edit in release. There is no baseline measurement for this additional fixture. It prevents interpreting the repeated-word speedups as a guarantee for arbitrary input.

This result motivated the incremental shaping measurements below. See the patch
notes for the [cosmic-text](../patches/cosmic-text/README.md),
[iced-graphics](../patches/iced-graphics/README.md) and
[iced-widget](../patches/iced-widget/README.md) regressions.

## Profiling and incremental prefix shaping

The next investigation isolated the remaining 294,000-byte distinct-word paragraph
case rather than increasing the global shaping cache or rewriting the editor.
Temporary timers measured bidi analysis/level adjustment, run-cache lookup, fresh
run shaping, glyph copying, cache insertion, wrap planning, and glyph placement.
Unattributed paragraph work includes Unicode segmentation, word/span traversal,
allocation and timer overhead; allocator cost was **not** measured independently.
The instrumentation is not part of the shipped dependency patch.

Representative instrumented first tail edit in a mounted editor, milliseconds:

| Component | Before | After |
| --- | ---: | ---: |
| Bidi analysis and level adjustment | 5.991 | 6.276 |
| Run-key construction/cache lookup | 4.259 | 0.002 |
| Fresh run shaping on cache misses | 54.364 | 0.017 |
| Glyph copying from run results | 3.565 | <0.001 |
| Run-cache insertion/eviction | 2.415 | <0.001 |
| Remaining paragraph-build work | 12.779 | 8.218 |
| Wrap planning/recycled-layout preparation | 1.066 | 1.084 |
| Glyph placement/layout assembly | 4.148 | 4.097 |

The original edit made **49,001 run-cache lookups**, with **14,019 misses**.
The incremental edit made **2 lookups**, with **1 miss**. A subsequent tail
character needed one new run; backspace reused an existing run. Timers add overhead,
so these component samples are diagnostic—not the end-to-end speed comparison.

### Smallest implemented path

`incremental-shaping.patch` retains shaped words before the earliest text mutation.
The existing line's glyph vectors are reused; there is no duplicate text snapshot
or enlarged global cache. Each word gains its logical byte range, and each line
tracks the unchanged prefix across split/append operations, including batched edits.

Bidi and line-break analysis still run over the whole paragraph. Reuse requires
unchanged word boundaries, blank status, resolved span level and paragraph direction.
Words touching the edit are rebuilt, as are tab words before recalculating tab
stops. Non-text invalidations revoke reuse. The existing owner contract for font
changes is unchanged: iced resets all lines on font-system revision changes.

Wrapping and glyph placement also still run in full. This deliberately targets
the measured dominant cost while leaving Unicode layout and caret behavior intact.
Edits near the beginning have little or no reusable prefix.

### Uninstrumented release results

Same machine, font setup and geometry; medians of three samples for the mounted
editor. Values include edit + native layout + scrollbar measurement. The original
`type` operation is unchanged; `type-again` and `backspace` are new follow-up steps
(insert `z`, then remove it) and therefore have no earlier baseline.

| Operation | Before (ms) | After (ms) |
| --- | ---: | ---: |
| Initial paste | 80.978 | 84.180 |
| First tail character | 81.483 | 19.200 |
| Another tail character | — | 15.604 |
| Tail backspace | — | 15.198 |
| Resize | 6.653 | 6.331 |
| Move to start and insert | 78.479 | 80.472 |

This removes most of the repeated shaping cost, not all paragraph-sized work.
Bidi analysis, segmentation/traversal and layout are still linear in paragraph
size. The large fixture is still not guaranteed to fit a 16.7 ms frame budget,
and initial paste/start edits remain roughly 80 ms. A full intra-paragraph
viewport/checkpoint design is a separate, larger follow-up rather than part of
this change.

Differential regressions compare complete edited layouts with fresh construction
and an empty run cache at two widths. Cases include combining marks, stable and
changed RTL/LTR direction, isolates, tabs, differently attributed split/join text,
explicit invalidation, and multiple unlaid edits replacing a word with another
word at the **same byte range**. The last case detects incorrectly advancing the
reusable prefix after a later append. Existing UI selection/caret/IME tests remain
in the suite; benchmark timings are not hard-coded test assertions.
