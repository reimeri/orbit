# General implementation guidance

Project-specific requirements override these defaults.

Use `nix develop` environment for development tooling.

## Project

- Start by reading the PROJECT.md file for information about this project and the goals

## Priorities and scope

- Identify the primary user task and prioritize it over supporting features.
- Keep implementation simple without reducing useful functionality to a barebones demo.

## Preserve agent-relevant data

- Never truncate or discard data that the agent might need. Output budgets limit what is sent inline, not what is preserved.
- If data is too large to send inline, save it completely to a private temporary file under `/tmp` and give the agent the file's absolute path and exact byte size instead. The agent can read or search the file for what it needs.
- Do not delete or otherwise manage these temporary files; leave cleanup to the OS. If saving fails, retain the data and report a recoverable error rather than returning incomplete output as though it were complete.
- Display-only shortening is fine when the complete underlying data remains available.

## Failure recovery

- When something inevitably goes wrong the last thing you want to do is to just present the user a error message and let them figure out how to continue
- Think of this like the user is the customer. When our service fails for some reason we don't just abandon the user and let them sort it out, no we try to everything we can to make our customer happy
- Always try to automatically recover from failures and if that is not possible provide a friendly error message to the user with succinct steps to fix the issue

## Backwards compatability

- No backwards compatability should be maintained in this project.

## Rust development workflow

- Read `docs/development.md` for build strategy and troubleshooting.
- Use `nix develop --command <tool> ...` or work inside `nix develop`.
- Prefer `cargo check --locked` for iteration and `cargo test --locked -p pico-core` for core-only changes.
- Preserve `target/`; do not routinely run `cargo clean`, release builds, or `nix build` in the edit loop.
- Keep compiler flags, feature selection, and linker configuration stable. Toolchain/flag changes rebuild dependencies.
- Run `just verify` before handoff; also run `nix flake check` when packaging changes and `just smoke-gpu` for graphics changes when a Wayland desktop is available.
- Keep iced out of `pico-core`. Target native Wayland only, with no X11/Xwayland backend.
- All V2 configuration/state must use `pico-evo`, never the old `pico` directories.

## Avoid Overly Defensive Code

Write idiomatic, straightforward Rust. Trust the type system and established invariants instead of adding redundant checks and abstractions.

- **Validate at boundaries.** Validate untrusted input once when parsing or constructing values. Pass validated, strongly typed values inward rather than repeatedly checking the same conditions.
- **Trust established invariants.** Do not defensively revalidate values already guaranteed by Rust's type system, validated constructors, or internal APIs. Avoid redundant `Option` checks, unnecessary pattern matching, and repeated validation of known-good data.
- **Avoid premature helper extraction.** Keep single-use logic inline at the call site unless extracting it meaningfully improves readability, encapsulates a complex operation, or enables reuse.
- **Keep error handling proportional.** Use `Result` and `?` for operations that can legitimately fail. Do not wrap infallible internal operations in unnecessary `Result` or `Option` types, introduce redundant error handling, or silently swallow errors with fallbacks such as `unwrap_or_default()`.
- **Do not hide invariant violations.** Handle expected failures explicitly, but do not add fallback branches for states that should be impossible under established invariants. Fix broken assumptions at their source rather than masking them with defaults.

## Testing guidelines

### Philosophy

- Optimize for confidence in correctness, not the number of tests or coverage percentage.
- Write the smallest set of tests that meaningfully protects important behavior.
- Every test should detect a plausible bug. If you cannot explain what bug a test would catch, reconsider adding it.
- Tests are not mandatory for every change. Avoid adding tests for trivial, cosmetic, or already adequately covered changes.

### Choosing what to test

- Before writing tests, inspect existing coverage and identify what could realistically go wrong.
- Prioritize complex logic, boundary conditions, error handling, state transitions, interactions between components, and previously observed bugs.
- Consider plausible incorrect implementations, then design inputs that distinguish correct behavior from those mistakes.
- Prefer a few carefully chosen, discriminating cases over many similar examples.
- Avoid redundant tests that exercise the same behavior without providing additional confidence.

### Test design

- Test observable behavior and public contracts rather than implementation details.
- Tests should remain valid when code is refactored without changing its behavior.
- Distinguish explicit unit-test fixture inputs from production defaults. Integration tests should exercise the real production configuration and derive expectations from its shared settings or observable relationships, not copy current default values into separate test fixtures. Include meaningful growth/overflow/transition checks so configuration-derived assertions do not become tautologies.
- Do not freeze incidental styling, defaults, catalog sizes, animation phases, or tuning values with hardcoded expectations. For consistency checks, derive expectations from the shared theme/configuration, explicit test inputs, or measured layout relationships; intentional styling/configuration changes should not require rewriting unrelated tests.
- Keep independent expected values for actual contracts (protocols, persisted formats, specified algorithms, and deliberate fixtures). Do not replace meaningful assertions with calls to the same implementation being tested, or make a test tautological merely to remove literals.
- Exercise behavior such as alignment, containment, overflow, state transitions, and bounded work rather than exact pixels, current defaults, or internal operation counts unless those exact values are the contract.
- Derive expected results independently from requirements, specifications, or manually verified examples. Do not simply copy results produced by the implementation under test.
- Include meaningful assertions about outcomes, not merely that code executes without crashing.
- Use asymmetric inputs, boundaries, and contrasting cases to expose mistakes that simple or uniform inputs would miss.
- Prefer realistic inputs and scenarios over artificially simplified cases that avoid the difficult parts of the problem.
- For regression tests, reproduce the original failure and verify the fix where practical.

### Test structure

- Prefer the lowest-cost test level that reliably detects the targeted failure.
- Use integration tests when correctness depends on interactions between components.
- Mock external dependencies when useful, but avoid mocking away the actual behavior being tested.
- Do not expose private internals or change production APIs solely to make testing easier.
- Follow existing testing conventions and reuse fixtures/helpers where appropriate.
- Keep tests deterministic, independent, and easy to understand.

### Advanced techniques

- Use property-based, fuzz, differential, or mutation testing when they meaningfully address a risk, not simply because the techniques are available.
- For randomized testing, generate valid, varied inputs that reach interesting code paths. Avoid large volumes of random invalid inputs that all exercise the same rejection behavior.
- Ensure properties and reference outputs have an independent basis for correctness.
- Avoid snapshot tests for volatile or irrelevant output.

### Reviewing tests

- Ask whether each test would fail if the targeted behavior were broken.
- Remove newly introduced duplicate, trivial, or implementation-coupled tests.
- Do not weaken assertions or change expected values merely to make a failing test pass.
- Run relevant tests after changes and report failures honestly.
- Briefly explain what important risks the new tests cover and any significant risks left untested.
