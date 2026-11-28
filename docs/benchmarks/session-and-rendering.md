# Session and rendering benchmarks

For current storage, recovery, and rendering behavior, see the
[performance guide](../performance.md). Run the commands below from the repository root.

## Session storage and viewport indexing

```sh
nix develop --command cargo run --locked --release -p orbit-core --example performance
nix develop --command just verify
nix develop --command just smoke-gpu   # when a Wayland desktop is available
```

The example creates isolated temporary state, uses no credentials/network, and
preserves the normal build cache. Its transcript has 20,000 approximately 1 KiB
entries plus 2,000 provider-context items. Its library fixture has 10,000 summary
rows and one large body: the summary query does not read any bodies. Baselines
reproduce the old deep-copy/full-pretty-JSON-write algorithms on the same fixture;
they are not an end-to-end comparison between separately built application versions.

The example also adds 500 child sessions with 100 assignments each. In a local
release run, taking the parent-indexed child-status snapshot and looking up all
10,000 main sessions averaged **0.22 ms** (200 iterations). This measures only
child-status projection, not rendering, search, or loading SQLite. The coordinator
updates running and unacknowledged-attention counts from each child's latest
assignment; the sidebar refreshes its snapshot once per catalog revision. Searching
or rendering rows does not revisit historical assignments.

One local release run (milliseconds per operation):

| Operation | Time |
| --- | ---: |
| Deep-copy transcript and provider context baseline | 13.72 ms |
| Shared full-session snapshot | 0.000077 ms |
| Full JSON durable rewrite baseline | 81.92 ms |
| Incremental durable draft update | 1.89 ms |
| Incremental durable one-entry update | 3.43 ms |
| Load 10,000 sidebar summaries | 23.83 ms |
| Load selected 20,000-entry session | 44.13 ms |
| Height-index viewport lookup | 0.000019 ms |
| Height-index append | 0.000047 ms |

At entry 15,000, the viewport/overscan range held **16 of 20,000 rows**. A process
waiting for a JS timer accrued zero `/proc/self/stat` CPU ticks during a one-second
sample; that is limited-resolution evidence, not a claim of literally zero work.

Storage timings are particularly variable: an earlier run measured approximately
38–50 ms for incremental saves. `fsync` latency, filesystem/storage activity and
cache state remain relevant. Do not treat the table as a cross-machine guarantee
or infer application frame rates from these microbenchmarks.

## Historical subagent assignments

Follow-up to [whole-app audit finding 4](app-performance.html). Two release builds
(before: `9f86c9d`; after: indexed restoration) launched against copies of the
same audit databases on ext4. Both have 10,000 main sessions, 500 children and
one large unselected transcript. Only the history case has the 50,000 runs:
all notices delivered, half the runs failed and not acknowledged.

| Renderer / build | Sessions only (ms) | With history (ms) | Extra ms | Sessions only (MiB) | With history (MiB) | Extra MiB |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| RADV / Weston GL · before | 110.8 | 197.8 | 87.0 | 50.8 | 81.4 | 30.6 |
| RADV / Weston GL · after | 108.9 | 122.8 | 13.9 | 49.2 | 55.4 | 6.1 |
| Lavapipe / Weston Pixman · before | 300.7 | 549.2 | 248.5 | 129.6 | 180.3 | 50.7 |
| Lavapipe / Weston Pixman · after | 300.4 | 485.3 | 185.0 | 129.9 | 156.1 | 26.3 |

The [launch probe](app-performance/launch.py) measures process start to the first
buffer-backed Wayland surface commit and samples process PSS after six seconds.
The two cases report medians of three fresh processes after a warm-up, with
fixture order rotated; “extra” is the difference of those medians. These are
whole-window costs: the sessions-only control has no child assignment statuses.

To separate older history from latest status, a second run compared all 50,000
assignments to the same fixture with only assignment 100 of each child retained
(500 runs; database repacked). With the same latest statuses visible, adding
the 49,500 older rows took **4.3 ms** on RADV (118.5 → 122.8 ms) and **3.6 ms**
on Lavapipe (476.7 → 480.3 ms). Median PSS differed by less than **0.1 MiB** on
both paths. Most of the remaining gap versus zero runs is thus already present
with just the latest assignments.

Opening a pre-index database builds the indexes once; **that launch is not
represented by the medians**. The first launch on the existing history fixture,
including index creation and other warm-up costs, took 394 ms on RADV and
651 ms on Lavapipe. The `performance` example also checks that an old run
is not resident and can be resolved by ID in the 50,000-run library.

## Text-layout cache reuse

A local isolated measurement used a real ~33 KiB tool result, a 700px-wide tool
viewport and 22 mounted rows. Each step advanced the viewport by one row, rebuilt
iced's UI and retained its widget cache (100 updates, the first stationary).

| CPU-side layout per update | Before | After |
|---|---:|---:|
| Debug: one-row advance | 88.06 ms | 2.15 ms |
| Release: one-row advance | 7.84 ms | 0.19 ms |
| Release: unchanged visible rows | 0.008 ms | 0.008 ms |

Previously, positional widget reuse changed the text in almost every slot;
painting and selection each shaped an independent paragraph. The measured phase
constructed 2,154 selection paragraphs plus the independent painting paragraphs.
Afterwards it constructed **99 total paragraphs**, one per newly entering row.
The same identity fix applies to sliding transcript windows. Supplying keys to
iced 0.14's `keyed_column` alone is insufficient: its splice heuristic still
diffs equal-length lists positionally. `keyed_column.rs` remaps the actual trees.

These are local microbenchmarks, not end-to-end frame times or an application CPU
usage guarantee. These historical measurements used the former renderer's shared
CPU text-layout path (current rendering tests use wgpu),
simulated offsets, no draw/present, and separately instrumented test builds.
First-use font initialization also affects expansion; the change does not remove
that cost. Normal full-output normalization/reflow remains linear in text size.

Deterministic regressions count new paragraphs for forward/reverse tool and chat
scrolling, changed text, width reflow, measurement epochs and session/entry identity.
They also check eviction rather than an unbounded offscreen paragraph cache.
To run the wgpu comparison against iced Rich (including Unicode/bidi,
fractional scaling and clipping):

```sh
nix develop --command just test shared_paragraph_matches_rich_text_pixels
```

## Activity-group label fitting

Opening a 37-tool group exposed repeated shaping in `ellipsized_text.rs`: each
new label measured its complete text, then shaped a binary search of candidate
prefixes, then measured the winner. This was not eager tool-output rendering.

Fitting now uses the full paragraph's glyph-cluster advances in **logical byte
order** to choose a grapheme-safe prefix. It measures the ellipsis and verifies
the final candidate, backing off only if contextual reshaping makes it too wide.
A fitting label needs one measurement; ordinary truncation needs three instead of
an unconditional series of binary-search probes. Ligatures/complex scripts may
produce a slightly shorter cutoff. The final paragraph must fit, and full source
text remains available to widget operations and tool details. Grouping, overscan,
tool-output caching and compiler profiles are unchanged.

A local **debug** before/after microbenchmark used a read-only snapshot of a real
293-entry session with eight activity groups. The target group contained 37 tools
and 33 reasoning entries. Both versions used the same snapshot, dependency
artifacts, debug flags and software-renderer UI construction/layout path. Each
width was tested in three fresh processes per version, with two group openings
per process; the table reports medians of six openings. Closing unmounts rows, so
these include reopening, not just a warm unchanged-layout measurement.

| CPU-side operation | Before | After |
|---|---:|---:|
| Expansion layout, 1280px window | 181.39 ms | 65.46 ms |
| Expansion layout, 1024px window | 157.80 ms | 63.36 ms |
| Label fitting within layout, 1280px | 172.82 ms | 57.16 ms |
| Label fitting within layout, 1024px | 149.48 ms | 55.12 ms |
| Text measurements per opening, 1280px | 186 | 61 |
| Text measurements per opening, 1024px | 184 | 61 |

Expansion layout was **60–64% faster (2.5–2.8×)**. The harness mounted 49 transcript
rows, including 23 tool rows, and **zero expanded tool-output caches**. Expansion
handling/index preparation remained about 0.14 ms, widget construction about
0.75–0.80 ms, and unchanged cached layout about 0.85 ms. Full-label and final-label
shaping still cost time; this does not establish a sub-frame debug expansion.

These are CPU microbenchmarks, not click-to-screen latency or release-build
claims. They exclude draw/present and subsequent row-size/scroll events. The
snapshot and instrumented harness stayed in private temporary files; no saved
session was modified or added to the repository. Regression tests use real fonts
to cover fit boundaries, combining marks, joined emoji, mixed RTL/LTR, retained
widget caches and the ordinary measurement count; a deterministic width-change
fixture exercises the contextual-overflow correction path.

## Smaller chat overscan

The chat now mounts rows intersecting its viewport plus **120 logical pixels on
both sides**, rather than one extra viewport on each side. This applies to prose,
activity rows and expanded details. The sidebar retains its original policy.
Measured viewport heights below 600px are honored; 600px is only the chat's
initial fallback. End anchoring uses that same height. If a row measurement
exposes more rows without moving the scroll anchor, their Markdown/output caches
are prepared before the next view is built.

A debug comparison used the same private 37-tool-group snapshot and isolated
UI-layout harness as above, with the glyph-based fitting optimization enabled in
**both** versions. Medians of six openings (three fresh processes, two openings
per process):

| Expansion metric | Viewport-sized margin | 120px margin |
|---|---:|---:|
| Layout, 1280px window | 65.86 ms | 39.95 ms |
| Layout, 1024px window | 63.22 ms | 38.09 ms |
| Mounted transcript rows | 49 | 26 |
| Mounted tool rows | 23 | 13 |
| Text measurements | 61 | 37 |

Expansion layout fell by about **39–40%**. Widget construction fell from about
0.76 ms to 0.49 ms; update/index preparation remained about 0.13 ms. Shaping and
cache policy were not changed; fewer offscreen rows are simply prepared.

There is a scrolling tradeoff. An additional simulation traversed the group
forward and back using fixed 30px, 120px and 600px moves, retaining widget state
between moves. At 1280px window width:

| Move size | Median layout before → after | 95th percentile before → after |
|---|---:|---:|
| 30px | 1.25 → 1.31 ms | 6.15 → 6.31 ms |
| 120px | 3.89 → 6.40 ms | 14.28 → 11.98 ms |
| 600px | 15.27 → 25.61 ms | 33.31 → 34.00 ms |

Larger moves can shape more new rows during the move because fewer were prepared
ahead of time. Thus this is **not an across-the-board scrolling speedup**. Mean
layout totals for an opening plus a full forward/back traversal were lower
(about 261 → 238 ms for 30px moves, 202 → 189 ms for 120px, and 185 → 177 ms for
600px). The 1024px runs showed the same tradeoff.

The scrolling simulation uses the same estimated row heights in both versions
and excludes draw/present, real wheel-event cadence and subsequent measurement
corrections. It does not establish end-to-end frame latency. Tests cover narrow
viewport boundaries, large jumps, tall rows, scroll-anchor retention, cache
refresh after shrinking measurements, and unchanged sidebar ranges.

## Width-aware prefix shaping

After reducing overscan, measuring complete labels was still the largest opening
cost. `ellipsized_text.rs` now initially shapes a grapheme-safe prefix estimated
from available width and font size (roughly three graphemes per em plus headroom,
minimum 32). If that prefix fits and the original source has more text, the probe
doubles until it overflows or reaches the actual end. This avoids a series of tiny
measurements while still allowing thin glyphs or wide rows to show more text.

The overflowing probe feeds the existing glyph-advance ellipsis fitter, including
final candidate verification and contextual-width backoff. Its advance arrays and
grapheme boundaries are now limited to the probe rather than the whole label.
Zero width shapes only an empty string; infinite width can still show the full
label. Original label storage, widget operations, tool details, cache policy,
120px overscan, and compiler settings are unchanged. This is a shaping-work
optimization, not a new truncation limit or an ASCII-only renderer.

The initial estimate is not a universal byte/time bound: a single grapheme can
contain many code points, and zero-advance or unusually thin text may require
larger probes. Labels that fit only after probe growth can take more measurements
than a short label. Source copying/cache-key construction still uses the complete
value. Complex-script cutoffs may differ slightly, as with the preceding fitter.

The same isolated 37-tool-group snapshot, debug dependencies, window sizes and
opening/scrolling harness were used before and after. Final paired runs had no
concurrent builds. Medians of six openings per width (three fresh processes, two
openings per process):

| Expansion metric | Complete-label measurement | Width-aware prefix |
|---|---:|---:|
| Layout, 1280px window | 39.78 ms | 29.37 ms |
| Layout, 1024px window | 38.78 ms | 26.37 ms |
| Label fitting, 1280px | 34.23 ms | 23.72 ms |
| Label fitting, 1024px | 33.09 ms | 20.43 ms |
| Bytes passed to label measurements, 1280px | 4,203 | 2,717 |
| Bytes passed to label measurements, 1024px | 3,990 | 2,210 |

Expansion layout decreased by **26–32%**. Both versions mounted 26 transcript
rows, including 13 tools, and made 37 label measurements per opening: the gain
comes from shorter shaping inputs, not fewer mounted rows or measurement calls.

The same forward/back scrolling simulation also improved larger moves (median
CPU layout time):

| Move size | 1280px before → after | 1024px before → after |
|---|---:|---:|
| 30px | 1.31 → 1.30 ms | 1.33 → 1.31 ms |
| 120px | 6.48 → 4.61 ms | 6.37 → 4.15 ms |
| 600px | 25.62 → 19.67 ms | 24.79 → 17.37 ms |

These remain CPU microbenchmarks, not release or click-to-screen measurements;
scrolling uses estimated row heights and excludes subsequent measurement events
and draw/present. Regression tests cover long ASCII/Unicode prefixes without
shaping their large suffixes, geometric growth for thin glyphs, zero/infinite
width, grapheme integrity, actual fit, and existing cache/overflow behavior.

## Verification and remaining measurement limits

These measurements do not include live model latency, eight simultaneous real
model streams, or a full frame-time/allocation profile. Loading one huge selected
body still takes time off the UI thread; formatting one enormous indivisible
Markdown block may still be expensive, since virtualization is at message level.
Compaction still requires sequential model calls for its new chunks. Resource use
can grow with deliberately retained runtime results/JS store data across many
conversations because the requested policy has no global cap.

## Sidebar wave CPU investigation

See the [2026-11-15 native-GPU benchmark](session-wave-2026-11-15/README.md)
for full-window measurements of continuous redraws, timed redraws, geometry
reuse, a static GPU-cache diagnostic, and reuse of the existing activity cadence.
The measured `tick-driven` variant is the one used in production: waves
reuse the activity timer, including during finalization and child work.

## Large text editor input

See [editor performance](../editor-performance.md) for the opt-in paste/layout/
scrollbar benchmark, debug and release baselines, and remaining single-paragraph costs.

## Transcript selection hit-testing

Selection candidates now compare their distance with the current winner **before**
calling the text renderer's hit test. Equal-distance candidates keep the first
winner, as before. Each shaped selectable paragraph also retains grapheme byte
boundaries and logical-line ranges. Hits use binary search for the nearest
boundary (earlier boundary wins a tie), direct line-start/predecessor lookup, and
binary search for highlight endpoint cursors instead of rescanning the text.
Indexes rebuild with paragraph shaping and survive width-only reflow. They add
O(graphemes + logical lines) memory to mounted paragraphs; construction is linear
and outside pointer-event handling. Existing renderer newline mapping is unchanged.

### Reproducing the benchmark

`crates/orbit-app/src/chat_selection_benchmark.rs` is an ignored benchmark using
the real Markdown viewer, selection root, scrollable and iced UI update path:

```sh
nix develop --command ./scripts/test-renderer.sh \
  cargo test --locked --release -p orbit selection_motion_benchmark \
  -- --ignored --nocapture --test-threads=1
```

Omit `--release` for the debug comparison. To include a long local assistant
message, set `ORBIT_SELECTION_BENCH_TEXT=/absolute/path/to/message.md`; its contents
are never printed. The synthetic cases need no session data. Run benchmarks alone.

Each case uses a 760x600 viewport and explicit 16px text, alternates the pointer
left/right, and measures near both the top and bottom. Results are medians of
three 200-event batches after a warm-up batch. Assertions check that both pointer
positions move the selection, scrolling reaches later text, and no paragraphs
are reshaped or application messages emitted during the measured work.
Parsing, initial layout/index construction, drawing and GPU submission are **not**
timed. Lavapipe initializes the headless renderer; these are CPU event timings,
not full-window CPU usage or frame times. Hit counts are test-only instrumentation.

### Local before/after results

Both versions used the same benchmark and toolchain. Milliseconds per event:

| Fixture / pointer location | Release before | Release after | Debug before | Debug after |
| --- | ---: | ---: | ---: | ---: |
| Local 14,609-byte message, top | 0.207988 | 0.017750 | 2.217222 | 0.117481 |
| Local 14,609-byte message, bottom | 0.199838 | 0.032373 | 2.178716 | 0.204105 |
| 64,000-byte source paragraph, top | 0.786564 | 0.000618 | 9.190375 | 0.006033 |
| 64,000-byte source paragraph, bottom | 0.845305 | 0.055584 | 9.549609 | 0.422080 |
| 200 short paragraphs, top | 0.218093 | 0.006970 | 2.255400 | 0.035868 |
| 200 short paragraphs, bottom | 0.201761 | 0.033194 | 2.185024 | 0.214335 |

The local message has 178 selectable blocks. Its text hits/event fell from 134
to 1.5 near the top and 121 near the bottom (averages over alternating positions).
Before/after selection endpoints matched in every fixture/location/profile.

The bottom cases expose the remaining limits rather than hiding them: traversal
is still in document order, so earlier blocks can be successively closer winners.
The renderer's native hit test also still walks visual runs within a large
paragraph. This change does **not** add block-level virtualization or remove
large-paragraph shaping/highlight costs.

Regression tests compare indexed Unicode snapping with a linear reference,
exercise retained widget state across width reflow/content edits, and verify
closer/tied candidates and bounded hit work when noncompetitive blocks are added.
Existing selection tests cover bidi, multi-click word/paragraph extension, links,
hard lines, CRLF tool copying, and scrolling with a stationary pointer.

## Selection-index reuse during scrolling

Selectable text now keeps its flattened text and word/paragraph selection index in
retained widget state, alongside the shaped paragraph. Rebuilding a view no longer
segments unchanged text. Text edits, logical byte-offset changes, and changes
between Markdown-block and plain-text paragraph semantics invalidate the index;
width, font/style, and scroll changes do not. Tool rows continue to use their
shared whole-output index, including boundaries beyond the mounted visual row.
The cache lives only as long as the mounted widget: there is no new history-wide
cache or change to eviction/overscan policy. The Markdown viewer's separate
copy-reconstruction block strings are still constructed during view building.

### Reproduce

```sh
nix develop --command ./scripts/test-renderer.sh \
  cargo test --locked --release -p orbit selection_scroll_benchmark \
  -- --ignored --nocapture --test-threads=1

# Existing pointer-selection benchmark, also rerun for this change:
nix develop --command ./scripts/test-renderer.sh \
  cargo test --locked --release -p orbit selection_motion_benchmark \
  -- --ignored --nocapture --test-threads=1
```

The scrolling benchmark in `chat_selection_benchmark.rs` uses the production
Markdown viewer, selection root, scroll helper, and viewport observer. It retains
widget state while rebuilding the view and alternates 120px wheel moves in a
760x600 viewport with an explicit 16px font. Each fixture reports medians and p95
for 120 cycles after initial layout. Assertions require scroll observations and
no newly constructed paragraphs or selection indexes during unchanged rebuilds.
The separately reported `word_indexes_separate` stage deliberately rebuilds the
old indexes as a diagnostic; it is **not** included in frame-stage totals.

### Paired release measurements

AMD Ryzen 7 5800X3D, pinned Nix environment and unchanged release profile. Three
fresh-process samples per version ran serially, with no concurrent builds. Each
phase below is the median of the three per-process medians. The table adds view
construction and retained layout to ensure work was eliminated rather than just
moved between those phases; these sums are **not full frame times**.

| Synthetic message | View + layout before | After | Reduction |
| --- | ---: | ---: | ---: |
| 20 paragraphs, 3,440 bytes | 0.0483 ms | 0.00554 ms | 88.5% |
| 200 paragraphs, 34,400 bytes | 0.494 ms | 0.0562 ms | 88.6% |
| 2,000 paragraphs, 344,000 bytes | 5.126 ms | 0.616 ms | 88.0% |
| One plain paragraph, 64,000 bytes | 0.765 ms | 0.0223 ms | 97.1% |
| 2,000 code lines, 120,012 bytes | 2.299 ms | 0.346 ms | 85.0% |
| One heavily formatted paragraph, 34,000 bytes | 0.382 ms | 0.0130 ms | 96.6% |
| One heavily formatted paragraph, 170,000 bytes | 1.963 ms | 0.116 ms | 94.1% |

For the 2,000-paragraph case, view construction fell from 4.927 to 0.447 ms and
retained layout from 0.198 to 0.170 ms. Index construction still occurs on initial
mounting and actual text/semantic changes; this does not remove cold shaping.

The existing pointer-motion benchmark retained identical selection endpoints and
hit counts. Representative bottom-of-document times were 0.0571 to 0.0581 ms/event
for the 64KB paragraph and 0.0346 to 0.0340 ms/event for 200 blocks: no material
change to hit-testing cost was expected from this optimization.

GPU preparation/submission/presentation, whole-app view/update work, and real input
cadence are excluded. Lavapipe initializes the test renderer, but measured drawing
only records CPU commands. The synthetic fixtures are not the user's document.
**Remaining limit at this stage:** the 170KB formatted single-paragraph fixture
still took about 62 ms to record drawing commands, due to repeated span-bound
scans. The decoration-cache follow-up below addresses that repeated drawing cost;
block-level virtualization remains separate.

Regressions exercise retained-widget double/triple clicks after same-length edits,
Unicode offsets, paragraph-mode changes, and style/width changes. A tool-row check
changes the unseen source suffix while leaving visible text unchanged, requiring
the new shared logical-line extent. Construction counters protect reuse without
wall-clock assertions; existing selection/copy/reflow and virtualization checks
remain in the suite.

## Decoration geometry caching

Decorated selectable spans now retain their `Paragraph::span_bounds` rectangles
alongside the mounted paragraph. Layout rebuilds the cache after reshaping,
bounds changes/reflow, or changes in which spans need decorations. Iced span
equality excludes paint-only properties, so the decorated-span set is checked
separately; padding/highlight-color changes on already-decorated spans reuse the
bounds. Unchanged view rebuilds and scrolling reuse them too. Replacing decorated text with undecorated text clears the cached geometry.

The cached rectangles are paragraph-local and unpadded. Painting still applies
current span styles, padding, colors, and layout translation in the original span
and decoration order. Clipping behavior is unchanged. The cache contains only
decorated spans and is dropped with the widget tree; no history-wide cache or new
retention policy was introduced.

### Measurements

Use the same `selection_scroll_benchmark` command above. It now also asserts no
geometry queries across unchanged layout/update/draw cycles and reports
`cold_draw_ms` alongside `cold_layout_ms`. The baseline used the same additional
cold-draw timer, before the production cache change. This prevents presenting work
moved into initial layout as though it had disappeared entirely.

Same AMD Ryzen 7 5800X3D, pinned release environment, seven fixtures, and 120 scroll
cycles per fixture. Three fresh-process samples per version ran serially, without
concurrent builds. Values are medians of the three per-process medians, in
milliseconds. The baseline **already includes selection-index reuse**.

| Synthetic message | CPU draw recording before | After |
| --- | ---: | ---: |
| 20 paragraphs, 3,440 bytes | 0.0127 ms | 0.0117 ms |
| 200 paragraphs, 34,400 bytes | 0.0140 ms | 0.0126 ms |
| 2,000 paragraphs, 344,000 bytes | 0.0208 ms | 0.0193 ms |
| One heavily formatted paragraph, 34,000 bytes | 2.402 ms | 0.0235 ms |
| One heavily formatted paragraph, 170,000 bytes | 60.173 ms | 0.0831 ms |
| One plain paragraph, 64,000 bytes | 0.0142 ms | 0.0107 ms |
| 2,000 code lines, 120,012 bytes | 0.0201 ms | 0.0197 ms |

The two long formatted-paragraph cases improve by roughly **102x and 724x** in
CPU draw recording. Retained view construction plus layout remains cheap: for the
170KB case, the sum of the phase medians was 0.0724 ms before and 0.0622 ms after.
Short paragraphs and undecorated text were already inexpensive; their small
absolute timing differences should not be treated as application-level gains.

**Cold layout is still expensive.** This change caches the existing span-bound
algorithm rather than replacing it with a single-pass algorithm. The 170KB case's
initial layout increased from 112.3 to 173.7 ms, while its first draw fell from
64.1 to 0.115 ms. Their combined cost stayed around 175 ms. Geometry is computed
for all mounted decorated paragraphs, including those outside the visible clip.
Initial mounting, edits, and reflow can still incur the full computation, and
cached geometry consumes memory proportional to those mounted span regions.

These are CPU microbenchmarks, not end-to-end frame times or a measurement of the
user's document. GPU preparation/submission/presentation and whole-app overhead
remain excluded. Pointer-selection benchmarks were also rerun: endpoints and hit
counts matched, and event costs remained comparable (the 64KB paragraph's bottom
case was 0.0622 to 0.0611 ms/event).

### Regression coverage

The native iced Rich pixel comparison now retains the selectable widget across
scrolling, narrowing, text-size/font changes, decoration-only additions/removals,
text edits, and replacement with undecorated text. It covers backgrounds, borders,
padding, underlines, strikethrough, Unicode, wrapping, clipping, and fractional
scale. Test-only query counters require reuse for scroll-only rebuilds and refresh
for geometry changes, without hard-coded timing thresholds. Existing selection,
copy, and virtualization tests remain unchanged.

## Single-pass decoration bounds on mount and reflow

Layout now collects the rectangles for **all** decorated spans in one pass over
the shaped glyphs, instead of restarting the glyph iterator for each span. A
temporary table maps glyph metadata to the decorated span's output slot. This
makes geometry collection linear in glyphs plus spans. Undecorated paragraphs
skip the pass; unchanged layouts keep the existing cache. The collector matches
iced Rich's first contiguous glyph group per span, including for bidirectional
text, and preserves paint order.

The `selection_scroll_benchmark` command above now also narrows the mounted
760 px viewport to 570 px after the last scroll cycle and times that relayout.
Medians of three fresh processes per version in the pinned release environment
(Ryzen 7 5800X3D); times are CPU layout only, not whole frames:

| Single formatted paragraph | Cold layout, before → after | Width change, before → after |
| --- | ---: | ---: |
| 34,000 bytes | 24.7 → 21.6 ms | 3.0 → 1.2 ms |
| 170,000 bytes | 172.7 → 114.1 ms | 72.9 → 6.0 ms |

The large fixture's reflow now fits a 16.7 ms layout budget. Its first mount
still does not: the full paragraph must still be shaped, wrapped and indexed.

## Git status: untracked line counts

Git status still polls every five seconds and gets fresh tracked diffs and
untracked membership from Git. The worker stats each untracked regular file
and reuses its line count (including zero for binary files) if its device,
inode, size, mtime and ctime match. A miss reads the file; the count is cached
only if the open descriptor still matches after the scan. The in-memory LRU
holds up to 8,192 versions across projects. Eviction costs a reread, not an
approximation of the displayed total.

In an optimized local probe with a single unchanged 64 MiB untracked text log
(4,194,304 lines), median time for a complete warm status refresh fell from
35.18 ms to 7.98 ms over 21 refreshes per version. This measures background
refresh latency, not UI frame time. The Git regression test also monitors real
file accesses, so it detects repeated reads even with a warm OS page cache.

### Polling past the line-count cache capacity

A sequential scan used to evict the next file it would need, so even **one**
file over capacity could turn every subsequent poll into a full reread. Now a
miss can replace an LRU entry only if that entry was last used before the
current scan began. Once the full cache has been touched, the worker still
counts overflow files exactly but does not admit them. This protects a stable
set during repeated scans without pinning stale versions or growing the cache.
Each resident count carries one monotonic last-use timestamp. Membership churn
and switches between large projects can still cause cold misses.

Same release fixture before and after, one committed repository of unchanged
8 KiB text files. Each row is the median of the three polls **after** the
initial poll at that size. `rchar` is logical read-call bytes, including Git
overhead, not physical disk traffic or file-content bytes alone.

| Untracked files | Logical bytes/poll, before → after | Refresh time, before → after |
| ---: | ---: | ---: |
| 8,192 | 205,183 → 205,183 | 22.33 → 22.71 ms |
| 8,193 | 67,322,260 → 213,395 | 86.12 → 22.75 ms |

The remaining content read at 8,193 files is one 8 KiB file per warm poll.
Git enumeration and per-file metadata checks still run every time. Reproduce
inside `nix develop`:

```sh
GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=/dev/null XDG_CONFIG_HOME="$(mktemp -d)" \
  cargo test --locked --release -p orbit untracked_poll_benchmark -- --ignored --nocapture --test-threads=1
```

### Layout within a long Markdown reply

Long assistant replies and expanded reasoning now mount a window of keyed
**top-level Markdown items** inside the existing chat scroll surface. The
complete Markdown and logical selection/copy traversal are unchanged. Unseen
heights are estimates; measured heights survive leaving/re-entering the message
without retaining its widgets. A streaming reply that crosses into windowed
layout starts from its measured eager heights. The index updates from the
first reparsed item, including earlier items affected by a late link definition.

**Paired release measurement** on the Ryzen 7 5800X3D: one answer with 2,000
numbered paragraphs containing Unicode, a link and inline code. Both arms run
the actual transcript view with the same parsed content, theme and font/device
cache. The control disables the inner window, not the outer transcript index.
Median of three samples after a warmup pair, alternating arm order. CPU wall
time includes widget construction, layout, redraw-event updates, scroll
operations and rebuilding until measurements settle; no GPU draw, readback or
presentation is timed.

| Phase | Eager control | Windowed |
| --- | ---: | ---: |
| Mount fresh reply, after preparation (760 × 600 window) | 59.46 ms | 0.78 ms |
| Jump to middle | 1.65 ms | 0.89 ms |
| Narrow window to 510 px while in middle | 15.01 ms | 0.13 ms |
| Jump to end | 1.67 ms | 0.94 ms |

Initial parsing/indexing is **not free**: including the separately timed
preparation step, median prepare + mount was **68.83 → 10.55 ms**. Both arms
prepare the production index before choosing a renderer. The audit’s original
122 ms was a single cold layout with a different fixture/driver warmup; it is
not the baseline for these ratios. Full-document copy still traverses the
selected content on demand. A single huge paragraph, table, quote, list or
code fence remains one layout unit, and composer editing is unaffected.

Reproduce inside `nix develop`:

```sh
scripts/test-renderer.sh cargo test --locked --release -p orbit long_reply_layout_benchmark -- --ignored --nocapture --test-threads=1
```

## Selection during image import

Follow-up to finding 3 of the [whole-app audit](app-performance.html). The probe
drops two distinct noisy 3840 × 2160 JPEGs into a stored session, starts the
first import, and immediately selects another stored session. It drives the
production UI update, media preparation and SQLite publication paths. Creating
the JPEGs is not timed. Selection ends when the destination composer is ready,
not when a frame is presented.

Five local release runs on ext4, each in a fresh test process:

| Elapsed time or process memory | Median | Range |
| --- | ---: | ---: |
| Selection request → destination composer ready | 4.7 ms | 4.1–5.4 ms |
| First drop → first image prepared | 337 ms | 333–375 ms |
| First drop → both images persisted in the originating draft | 654 ms | 646–689 ms |
| Sampled peak process PSS (41 MiB baseline) | 174 MiB | 171–176 MiB |

Selection finished before the first import in every run. PSS is sampled from
`/proc/self/smaps_rollup` roughly every 5 ms; it includes the UI test harness
and is not an allocation limit. This exercises file drops, not a live Wayland
clipboard. Reproduce inside `nix develop`:

```sh
scripts/test-renderer.sh cargo test --locked --release -p orbit two_4k_import_probe -- --ignored --nocapture --test-threads=1
```

## Persistence during concurrent tool turns

Follow-up to [finding 5](app-performance.html#turn). The delay on ext4 was
primarily **waiting for other turns' SQLite commits**, not waiting for a blocking
worker or preparing SQL. At eight turns, 88 commits occupied the single
connection for 168–178 ms in groups that finished in 179–184 ms. Those commits
do not overlap; wait times from different turns *do*, so adding waits across
turns would overstate the delay.

| Storage | Turns | Group makespan (ms) | Worker queue (ms/turn) | DB mutex wait (ms/turn) | Transaction body (ms/turn) | Commit (ms/turn) |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| ext4 | 1 | 66.90 | 0.09 | <0.01 | 0.45 | 22.15 |
| tmpfs | 1 | 44.34 | 0.09 | <0.01 | 0.40 | 0.21 |
| ext4 | 8 | 183.11 | 0.09 | 148.29 | 0.48 | 21.50 |
| tmpfs | 8 | 48.79 | 0.08 | 2.30 | 0.42 | 0.20 |

Same release binary, Ryzen 7 5800X3D, ext4 on a Crucial MX500 SATA SSD vs
tmpfs. Each local turn runs one QuickJS call, sends its result back to the
loopback peer, streams 64 text deltas, and verifies the restored final answer.
Each row excludes the first group as warmup. Makespan is the median of three
groups. The remaining columns are medians of **per-turn sums** (3 single turns
or 24 concurrent turns); all measured turns made 11 session saves.

The test-only trace times submission to entry of the blocking save worker,
then separately times acquiring the shared connection, executing the transaction
(including serialization), and calling SQLite commit with WAL and
`synchronous=FULL`. It does not attribute snapshotting, network or other
scheduling to a DB phase. Commit includes more than a device flush: in a
**separate** syscall-traced run the median successful `fsync` on
`sessions.sqlite3-wal` was 1.74 ms on ext4 vs 0.007 ms on tmpfs (413 calls
per filesystem, including setup and warmup). These syscall timings corroborate
the storage difference; they are not subtracted from the untraced measurements.

Reproduce inside `nix develop` (check mounts; `/tmp` is not always persistent).
`SAVE` and `TX` rows report microseconds and are keyed by sample, group size
and turn; sample 0 is warmup. The trace is drained after the group stopwatch stops.

```sh
findmnt -T /tmp -o SOURCE,FSTYPE
findmnt -T /dev/shm -o SOURCE,FSTYPE
probe() { cargo test --locked --release -p orbit-core --lib local_turn_benchmark -- --ignored --nocapture --test-threads=1; }
TMPDIR=/tmp probe
TMPDIR=/dev/shm probe
# Optional, run separately with strace available; repeat on the other mount.
log=$(mktemp)
TMPDIR=/tmp CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER="strace -f -T -yy -e trace=fsync,fdatasync -o $log" probe
```

**Durability check:** `failed_tool_start_commit_rolls_back_without_dispatch`
uses a deferred foreign-key failure: the raw call commits, the tool-start write
runs, but COMMIT fails. The previous checkpoint remains readable, the tool's
side effect never occurs, and retrying the save after clearing the fault
persists the interrupted outcome. This checks application ordering, not a
physical power loss. No checkpoint or sync policy changed. If fewer commits
are investigated, distinguish ordinary saves from acceptance, tool-start and
result saves before trying to combine any of them.

## Cold sidebar switch to a tool-heavy session

This investigation used a private copy of a session with 295 transcript records,
285 provider items and two fork checkpoints (about 8 MB of stored JSON in total).
With activity collapsed it has just nine transcript rows, but the first prompt
alone is 42,886 characters. A later Markdown reply also contains a long table.

Local Ryzen 7 5800X3D and Radeon RX 7900 GRE; private Sway output at the user's
window size (2552×1394, 144 Hz), hardware Vulkan. Each trial started a fresh
Orbit process on a short outgoing chat, with no unsaved draft or executing turn.
The probe injected a Wayland pointer click and checked the pixels in the chat
pane via screencopy. The endpoint is the compositor timestamp of the first frame
showing the destination transcript, not a sidebar highlight, a database write
or a loader-completion message. OS page caches were not flushed.

| Profile | Click → displayed, before (3 trials) | Click → displayed, after (5 trials) | Storage load alone, after |
| --- | ---: | ---: | ---: |
| Unoptimized debug | 410–428 ms | 95–102 ms (median 96 ms) | 16–18 ms |
| Release | 59–60 ms | 23 ms | 10 ms |

The benchmark used the same transcript, context and fork payloads in both arms;
no messages were summarized, shortened or discarded. The debug timings also
include first-use font and glyph costs. The main wins were avoiding first-frame
layout of the old prompt, parallel decoding of the active history without
eagerly reconstructing the fork histories, and not opening unused fonts or
rasterizing glyphs again when the GPU atlas grows. The first subsequent fork
or new checkpoint may need to decode its stored prefix once.

At 1280×800, 60 Hz, three further debug trials displayed the chat in 96 ms.
These are local click-to-frame measurements, not a 100 ms guarantee under
storage contention, cold OS caches or different display/driver configurations.
