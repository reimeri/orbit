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
- Phases: paste, unchanged layout, insert `x` at the paste tail, resize, then move
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

## Baseline diagnosis

1. **The smoke pause is primarily native initialization, not our scrollbar.**
   In the pinned `iced_graphics::text::Editor::update`, metrics are applied before
   viewport size. Cosmic-text's `set_metrics` immediately shapes using the old
   bounds; a new buffer has no height limit. The unlaid benchmark caches every
   logical line natively. A mounted editor instead caches only a small viewport.
   First candidate: apply valid metrics and bounds together, avoiding the
   unbounded intermediate layout. This needs a focused regression in the pinned
   dependency patch; merely optimizing Pico's scrollbar will not fix this path.
2. **For an already-visible editor, Pico's full-document scrollbar measurement
   dominates large multiline pastes and width changes.** Unchanged layout and
   small edits are much cheaper; the immediate problem is eager cold-cache work,
   not simply the cached line-count summation. Evaluate bounded/lazy measurement
   with viewport anchoring and estimated offscreen heights, or incremental work,
   after fixing native initialization. Retain all text and normal wrapping.
3. **Multiline insertion itself has poor scaling.** Cosmic-text 0.15's `insert_at`
   repeatedly inserts middle lines at the same index in its `Vec`, shifting the
   suffix on each insertion. The measurements warrant a separate bulk-insertion
   experiment; reducing layout cost alone does not eliminate paste latency at
   much larger line counts.
4. **One enormous logical line is a separate risk.** Native shaping dominates
   even in a mounted editor, including a one-character edit. Logical-line
   virtualization alone cannot solve this. Profile shaping/wrapping before
   choosing a deeper editor change; do not split Unicode text arbitrarily.
5. **CJK-containing text is much more expensive than the ASCII/Greek controls
   on this font setup.** This narrows the investigation to font/shaping behavior,
   not plain UTF-8 byte handling. These stage timings do not isolate font fallback
   from shaping; investigate that path rather than attributing every Unicode
   input to the same cost or relying only on ASCII benchmarks.

These are exploratory local measurements, not cross-machine guarantees. The
benchmark does not measure clipboard transfer, draft persistence, selection
replacement, peak memory, actual frame latency, or all Unicode/wrapping patterns.
Keep existing interaction/Unicode tests when implementing improvements; add
bounded-work regressions for the chosen approach rather than hard wall-clock
thresholds. The implementation and remaining limitations are described below.

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

### Remaining limit: huge single logical lines

A new **294,000-byte distinct-word paragraph** deliberately exceeds the useful working set of the word cache. It takes **80.1 ms** for paste/layout and **81.0 ms** for a tail edit in release. There is no baseline measurement for this additional fixture. It prevents interpreting the repeated-word speedups as a guarantee for arbitrary input.

Native layout still processes an entire logical line, and cache pressure can cause reshaping. Therefore enormous single paragraphs/minified input are not yet guaranteed to stay within a frame budget. Further improvement would require incremental/intra-paragraph layout work with careful Unicode/bidi handling, not more scrollbar changes. Peak memory and all Unicode/font combinations have not been benchmarked.

### Regression coverage

The new checks cover lossless middle/selection-replacement paste with mixed line endings; bounded newly shaped logical-line counts for large pastes and middle/end seeks; cursor preservation; preferred non-exact font weights and font removal; cached-vs-fresh bidi/cluster/attribute layout; and focused drawing plus IME commit after edit/seek/reflow. Existing growing-height, wrapped overflow, wheel/drag batching, fractional viewport, ligature/copy and questionnaire tests remain in the verification suite.

See the patch maintenance notes under `patches/cosmic-text`, `patches/iced-graphics`, and `patches/iced-widget`. Cargo and the Nix package use the same checksum-pinned patched sources.
