# Sidebar wave CPU experiment — 2026-11-15

This is the historical benchmark report, recorded **before the production change**.
The subsequent minimal implementation selects `tick-driven`: waves reuse existing
activity redraws. The shared widget-local-clock redesign was not implemented.
Production Rust sources were unchanged during the measurements. `experiment.patch` contains the temporary fixture,
variants, and instrumentation so the measurements can be reproduced without
permanently adding benchmark switches to the application.

Base commit: `79d9a2f04e13c15f103806ec6e78d2fee4442763`.

## Environment and method

- AMD Ryzen 7 5800X3D, 16 logical CPUs; AMD Radeon RX 7900 GRE.
- Native Wayland/niri, hardware Vulkan (RADV), wgpu; no Lavapipe or Xwayland.
- 1714 × 1394 window, scale 1, on the 74.983 Hz output. The native window
  remained the same size within and between the reported runs.
- `nix develop`, Rust 1.99.1, `cargo build --locked --release -p pico-evo`;
  unchanged compiler flags/features/linker. No compilation during sampling.
- Real `App::view`, synthetic user/Markdown/tool transcript, expanded tool result,
  six sidebar sessions, one or six visible Working rows. No provider stream,
  tool execution, real sessions/credentials, or desktop notifications.
- The existing 100 ms app activity timer is enabled unless explicitly noted.
  The ordinary Git refresh and age subscriptions remain. The synthetic project
  is a private temporary directory, not a repository with an expensive Git scan.
- One process/window per suite. Cases change in place; startup is excluded.
  Repeated suites shuffle the case order within each round using a recorded seed.
- CPU = `100 × Δ(utime + stime) / CLK_TCK / elapsed_seconds`, from
  `/proc/self/stat`, including **all application threads**, excluding children,
  compositor, and GPU execution. **100% means one fully occupied CPU core**,
  not the whole 16-thread machine.
- Counters record wave-widget redraw-event visits and draws. With a single
  visible wave these approximate window redraw cadence; they are **not measured
  display presentations**. Multi-row rates divide visits by the visible row count.
  They help reject misleading measurements of hidden/stalled windows.
- Per-draw timing covers CPU-side mesh construction/submission only. It is wall
  time, not a GPU timer or separate process CPU accounting.

A short screening sweep used 4 s warm-up + 10 s measurement per case. Longer
repeated comparisons and multi-row/timer-disabled follow-ups are described below.
All CPU measurements include the small common instrumentation overhead.

## Variants

| Name | What changes |
| --- | --- |
| `current` | Current wave math/mesh, continuous next-frame requests. |
| `none` | No wave mesh or wave-driven redraw requests; same surrounding widgets and app timer. Approximate pre-wave baseline. |
| `frozen` | Phase-zero wave, no wave-driven requests; still redraws when the app does. |
| `reuse` | Retain geometry/indices, update vertex colors; still clone the mesh into Iced and upload it each draw. Same continuous redraw schedule. |
| `gpu-static` | Cache one static mesh on the GPU but keep continuous redraws. A diagnostic for removing wave generation/upload work, **not an equivalent animated solution or an implemented shader**. |
| `cap5/10/15/30` | Request timed wave redraws using a stable deadline. Other application events can still cause redraws between deadlines. These names are not total-window FPS caps. |
| `tick-driven` | Keep the wave moving, but request no extra redraws; use the existing app activity redraws. |

The patch also contains `static-cache`, `reuse10`, and `reuse-tick` probes; these
are not part of the reported comparison unless present in a result dataset.

## Results

The complete **43 samples**, including raw process statistics and suite settings,
are in [`results.json`](results.json). Values below are medians and observed
minimum–maximum ranges, **not confidence intervals**. CPU accounting resolution
is 0.05 percentage points per 20 s sample and about 0.067 points per 15 s sample;
extra displayed decimal places should not be interpreted as extra accuracy.

### One working row, existing activity timer enabled

Three samples per case, each 4 s warm-up + 20 s measurement; shuffled case order.

| Approach | CPU median | CPU range | Wave redraw visits/s |
| --- | ---: | ---: | ---: |
| No wave mesh (`none`) | 0.75% | 0.75–0.80% | 10.2 |
| Current animation | 4.10% | 3.95–4.25% | 74.9 |
| GPU-cached static wave, continuous redraws | 3.85% | 3.80–4.00% | 74.9 |
| Separate 10 Hz wave deadline | 1.30% | 1.25–1.30% | 20.0 |
| Animate only on existing activity redraws | **0.90%** | **0.80–0.95%** | 10.2 |

The separate 10 Hz deadline is **not** a 10 fps window cap: its redraws occur
between the existing activity timer's redraws, nearly doubling the cadence.
Conversely, eliminating wave construction/upload work while still continuously
redrawing the window saves little in the one-row case. A wave-only shader is
therefore not the first optimization suggested by this experiment.

### Six visible working rows, existing activity timer enabled

Two samples per case, each 4 s warm-up + 20 s measurement; shuffled case order.

| Approach | CPU median | CPU range | Wave redraw visits/s per row |
| --- | ---: | ---: | ---: |
| No wave meshes | 0.82% | 0.75–0.90% | 10.2 |
| Current animation | 6.57% | 6.45–6.70% | 74.9 |
| Animate only on existing activity redraws | 1.25% | 1.20–1.30% | 10.2 |

Reusing the activity cadence is a small, promising change for one wave, but it
was **not below 1% with six waves** in this fixture.

### Disable the application-wide activity timer: architectural diagnostic

Two samples per case, each 6 s warm-up + 15 s measurement; shuffled case order.
The wave remains animated and the selected session remains visually Working,
but other tick-driven indicators freeze. These are **not measurements of a
completed replacement for those indicators**.

| Wave scheduling | One row CPU median (range) | Six rows CPU median (range) |
| --- | ---: | ---: |
| Continuous | 3.80% (3.80–3.80%) | Not measured |
| 30 Hz deadline | 1.73% (1.73–1.73%) | Not measured |
| 15 Hz deadline | 0.90% (0.87–0.93%) | 1.27% (1.27–1.27%) |
| 10 Hz deadline | **0.63% (0.60–0.67%)** | **0.90% (0.87–0.93%)** |

Observed visits were approximately 74.9, 30.3, 15.3–15.4, and 10.4 per second,
respectively. Ordinary low-frequency app events still cause extra redraws.

This supports investigating a **shared, widget-local 10 Hz animation cadence**,
replacing—not adding to—the app-wide animation Tick. It offers more headroom
than simply piggybacking on the existing app timer. A production implementation
must keep the other activity/status indicators moving, coordinate their deadlines,
retain offscreen/frozen-state behavior, and cover all Working lifecycles (including
terminal persistence and parents with active children). It still needs its own
functional tests and a repeat of these measurements before claiming the target.

### Initial screening sweep

One 10 s sample per case after 4 s warm-up, one Working row, app activity timer
on. These shorter single observations are exploratory, not repeated estimates.

| Probe | CPU | Wave redraw visits/s |
| --- | ---: | ---: |
| Current | 4.20% | 75.7 |
| No wave mesh | 0.80% | 10.2 |
| Retain geometry/topology, update colors | 4.20% | 75.4 |
| GPU-cached static wave, continuous redraws | 3.90% | 75.1 |
| Separate 30 Hz deadline | 2.30% | 35.1 |
| Separate 15 Hz deadline | 1.60% | 25.0 |
| Separate 10 Hz deadline | 1.40% | 19.1 |
| Separate 5 Hz deadline | 1.10% | 15.2 |
| Frozen wave, no wave-driven redraws | 0.80% | 10.2 |
| Existing activity redraws only, animated wave | 0.90% | 10.2 |

The geometry-reuse probe still clones/uploads the mesh and recalculates colors;
it should not be generalized to every possible caching design. Its lack of an
observed improvement and the GPU-static diagnostic both point to redraw cadence
as the higher-value first change.

**Subsequent decision:** implement the small change that reuses current activity
redraws for visual evaluation. Coordinated widget-local animation remains a
possible follow-up. No production optimization was applied during the measurements
recorded here, and the archived patch still targets the base commit above.

## Reproduce

Use an otherwise unchanged checkout at the base commit. Keep the window visible,
leave its size/output unchanged, and avoid interacting with it during sampling.
Do not run builds or tests concurrently. A desktop session is required: the
headless smoke configuration is not a substitute for this measurement.

```sh
# Run from the repository root inside nix develop.
patch=docs/benchmarks/session-wave-2026-11-15/experiment.patch
artifacts=$(mktemp -d /tmp/pico-wave-repeat-XXXXXXXX)
git apply --check "$patch"
git apply "$patch"
cargo build --locked --release -p pico-evo
cp target/release/pico-evo "$artifacts/pico-evo-benchmark"
git apply -R "$patch"  # Keep experiments out of production source.

# Select your hardware Vulkan ICD if needed. The original machine used:
export VK_DRIVER_FILES=/run/opengl-driver/share/vulkan/icd.d/radeon_icd.x86_64.json
python3 docs/benchmarks/session-wave-2026-11-15/run.py \
  --binary "$artifacts/pico-evo-benchmark" --output "$artifacts/one-row" \
  --modes none,current,gpu-static,cap10,tick-driven \
  --rounds 3 --seconds 20 --warmup 4
```

`run.py` requires Python 3 (standard library only) in the development environment.
The output directory must not exist. It retains the full application log, raw
process samples, derived results, case order/settings, and an initial RGBA
screenshot; the log includes screenshot dimensions. Use `--rows 6` for the
multi-wave fixture. `--tick 0` disables the app activity timer **as a diagnostic**;
use at least 6 s warm-up for this mode, because switching away from a no-redraw
case may wait for the ordinary five-second Git tick. This diagnostic also freezes
other tick-driven status indicators; it is not a complete implementation of
widget-local replacements for them. The runner rejects low-cadence timer-enabled
results as suspicious after saving them; still inspect cadence and window
visibility for all runs, especially timer-disabled diagnostics.

After reproducing, rebuild the normal application if you use `target/release`
directly. Leave experiment artifacts in `/tmp` for inspection; do not overwrite
or silently discard failed runs.

## Limits and rejected trials

- This measures a non-streaming working session. Provider traffic, Markdown
  updates, scrolling, a different transcript/window, GPU/CPU load, and display
  refresh rate can change results. It does not establish an application-wide
  CPU ceiling under arbitrary load or certify subjective animation smoothness.
- The desktop was not an otherwise idle, dedicated benchmark machine. Repeated
  measurements and ranges are more informative than hundredths of a percent.
- GPU-static is deliberately not animated. It tests the value of eliminating
  mesh work while retaining window redraws, not visual equivalence.
- Native desktop trials were used after private headless Weston/GL trials
  produced only ~0.4–0.6 wave redraw visits/s; one compositor also crashed. These
  stalled trials are **excluded**, not reported as low-CPU successes. Software
  compositor/readback paths were not substituted for hardware presentation.
- Temporary fixture state and all original logs, binaries, patches, and captured
  pixels are retained under `/tmp/pico-wave-bench-20261115`. Failed pilot logs are
  retained there too. The checked-in dataset preserves complete successful raw
  samples and run settings; screenshots/binaries are not committed.

## Validation

- The existing rendered wave regression passed with the experimental code (no
  benchmark environment variables): animation, freeze/resume, visibility, clipping,
  and absence of app messages/layout invalidation.
- After restoring production sources: **`just verify` and `just smoke-gpu` passed**.
  The normal release binary was rebuilt successfully too, so `target/release`
  does not retain the instrumented application.
- `git apply --check` confirms the archived experiment applies to the base Rust
  sources. The runner was exercised by the native measurement suites; no timing
  threshold was added to the ordinary test suite.
