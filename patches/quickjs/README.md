# QuickJS pending-module lifetime fix

Bases: checksum-pinned crates.io **rquickjs-sys 0.11.0** (bundling QuickJS-NG
0.11.0) and **rquickjs-core 0.11.0**. Nix applies both patches without fuzz and
prepares `.build-deps/rquickjs-sys` and `.build-deps/rquickjs-core` for the shell,
package, and formatting check. Cargo's registry cache is never modified.

## Upstream research and provenance

`module-lifetime.patch` backports the engine changes from QuickJS-NG
[PR #1877](https://github.com/quickjs-ng/quickjs/pull/1877), commit
[`8b34053991af13f02bbbd1015493ce448616b8d6`](https://github.com/quickjs-ng/quickjs/commit/8b34053991af13f02bbbd1015493ce448616b8d6),
by xyaman. The PR was **open and unmerged when checked on 2026-11-15**; this is
not a claim that the fix is in an upstream release. The engine changes are the
same, with paths/context adapted to the pinned crate; upstream's newer
`api-test.c` tests are replaced by our Rust integration regressions.

The PR explicitly calls out additional reference leaks in rquickjs. The local
`module-handles.patch` addresses those in our pinned wrapper. Keep the two
patches together when updating/removing this workaround.

`writable-build-inputs.patch` is a build-only adaptation: the upstream build
script copies source files into `OUT_DIR` with their permissions. Nix-store
sources are read-only, so rerunning that build script could not overwrite its
own outputs. Reading/writing their bytes instead keeps generated files writable
without mutating the immutable source or changing the engine's compiler flags.

## Bug and ownership

Pico evaluates code-mode cells as ES modules. Cancelling a pending top-level
await can leave async-module reactions alive during garbage collection.
Previously `JS_FreeContext` unconditionally freed the module definition, then
the reaction finalizers decremented its freed reference count. This caused
intermittent SIGABRTs in `js_free_value_rt` and could corrupt allocator metadata.
A successful rerun did not establish memory safety.

The engine fix keeps the existing module-list ownership reference:

- `js_free_module_def` frees the module contents, detaches the list entry, and
  releases **one reference**, instead of unconditionally freeing the allocation.
- Async reaction values can subsequently release their references safely. The
  last reference frees the allocation; freeing a still-linked module remains
  an explicit invariant failure, not a suppressed abort.
- `JS_ReadModule` acquires its returned reference only after successful parsing,
  removing both the extra success-path reference and failure-path leaks.

The wrapper's `Module` is a borrowed pointer plus a `Ctx` that keeps the context
alive. It has no module-value destructor, and cloning it only clones the
context handle. `declare` and `load` must therefore release the extra owned
`JSValue` they receive after extracting its pointer. Native-module borrowed
pointers and `eval`'s duplicate/consume pair are unchanged.

This fixes the engine used by both tests and normal code-mode. It does not
change ES-module semantics, run cancelled jobs, retry failed tests, disable
assertions, or serialize production cells.

## Regression checks and maintenance

`crates/pico-core/tests/quickjs_lifetime.rs` covers retained unresolved top-level
await reactions, fulfillment/rejection controls, pending dynamic import,
cloned module handles and evaluation, embedded bytecode loading, and real
code-mode cancellation followed by another cell and `exit()` after an await.
The embedded fixture is compiled with the same patched engine.
`bytecode-lifetime.c` separately exercises truncated and successful bytecode
reads through the raw C API, including evaluation consuming the returned module
value. This avoids violating the Rust loader's valid-bytecode precondition.
The memory-check script builds this small probe against the patched engine in
a private `/tmp` directory; it leaves artifacts for OS cleanup.

Run these inside `nix develop`:

```sh
cargo test --locked -p pico-core --test quickjs_lifetime
just test-quickjs-memory
just verify
nix flake check
```

The memory check uses Cargo's executable runner (not different compiler flags)
to run this integration target under Valgrind, followed by the C bytecode probe. Invalid reads/writes and definite
or indirect leaks fail the command even if every Rust assertion passes. It runs
in both `just verify` and the packaged build's checks. Possible/still-reachable
process-global Rust/Tokio allocations are not treated as module leaks.

When revising either patch, re-enter the shell. If Cargo reuses artifacts after
a generated symlink changes (Nix normalizes source mtimes), run
`cargo clean -p rquickjs-sys -p rquickjs-core` once to invalidate only these
dependencies, not the full target directory. Update the pinned hashes and audit
wrapper ownership again when upgrading rquickjs. Remove the patches only after
upstream has equivalent fixes for both engine lifetime and wrapper accounting.
