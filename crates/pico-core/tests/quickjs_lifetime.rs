//! Run normally with Cargo and under Memcheck with `just test-quickjs-memory`.
//! A successful process exit alone cannot detect the original use-after-free.
use pico_core::{
    CancellationToken,
    code_mode::{CodeMode, Event},
    shell::Shell,
};
use rquickjs::{
    Context, Module, Runtime,
    loader::{BuiltinLoader, BuiltinResolver},
    promise::PromiseState,
};
use std::{sync::Arc, time::Duration};

#[test]
fn pending_top_level_await_teardown_and_settled_controls() {
    for action in [None, Some("resolve(23)"), Some("reject('expected')")] {
        let runtime = Runtime::new().unwrap();
        let context = Context::full(&runtime).unwrap();
        context.with(|ctx| {
            let promise = Module::evaluate(
                ctx.clone(),
                "cell",
                "globalThis.result = await new Promise((resolve, reject) => {
                    globalThis.resolve = resolve; globalThis.reject = reject;
                });",
            )
            .unwrap();
            assert_eq!(promise.state(), PromiseState::Pending);
            if let Some(action) = action {
                ctx.eval::<(), _>(action).unwrap();
                if action.starts_with("resolve") {
                    promise.finish::<()>().unwrap();
                    assert_eq!(ctx.globals().get::<_, i32>("result").unwrap(), 23);
                } else {
                    assert!(promise.finish::<()>().unwrap_err().is_exception());
                    assert_eq!(
                        ctx.catch().as_string().unwrap().to_string().unwrap(),
                        "expected"
                    );
                }
            }
            // In the pending case the global resolvers retain async-module
            // reactions. Context collection used to free their module too soon.
        });
    }
}

#[test]
fn dynamic_import_and_borrowed_module_handles_release_their_references() {
    let runtime = Runtime::new().unwrap();
    runtime.set_loader(
        BuiltinResolver::default().with_module("hung"),
        BuiltinLoader::default().with_module("hung", "await new Promise(() => {});"),
    );
    let context = Context::full(&runtime).unwrap();
    context.with(|ctx| {
        let declared = Module::declare(ctx.clone(), "ready", "export const answer = 19;").unwrap();
        let retained = declared.clone();
        drop(declared);
        let (evaluated, promise) = retained.eval().unwrap();
        promise.finish::<()>().unwrap();
        assert_eq!(evaluated.get::<_, i32>("answer").unwrap(), 19);
        let import = Module::import(&ctx, "hung").unwrap();
        while ctx.execute_pending_job() {}
        assert_eq!(import.state(), PromiseState::Pending);
        // Memcheck also checks for leaked module allocations from declare().
    });
}

#[test]
fn bytecode_module_loader_balances_its_returned_reference() {
    static BUNDLE: rquickjs::loader::Bundle = rquickjs::embed! {
        "compiled": "tests/fixtures/lifetime-module.js",
    };
    let runtime = Runtime::new().unwrap();
    runtime.set_loader(BUNDLE, BUNDLE);
    let context = Context::full(&runtime).unwrap();
    context.with(|ctx| {
        let namespace = Module::import(&ctx, "compiled")
            .unwrap()
            .finish::<rquickjs::Object>()
            .unwrap();
        assert_eq!(namespace.get::<_, i32>("answer").unwrap(), 37);
        // Loading cached bytecode crosses both patched ownership boundaries:
        // QuickJS's JS_ReadModule and rquickjs's Module::load.
    });
}

#[test]
fn cancelled_code_mode_cell_finishes_teardown_and_allows_the_next_cell() {
    let (entered, ready) = std::sync::mpsc::channel();
    let mode = CodeMode::new(
        Arc::new(Shell::new(std::env::temp_dir())),
        Arc::new(move |event| {
            if let Event::Notification { .. } = event {
                entered.send(()).unwrap();
            }
            Ok(())
        }),
    );
    let cancel = CancellationToken::new();
    let running = mode
        .exec_output(
            "// @exec: {\"yield_time_ms\": 1}\n\
             setTimeout(() => notify('suspended'), 0);\n\
             await new Promise(resolve => { globalThis.keepAlive = resolve; });",
            "pending",
            &cancel,
        )
        .unwrap();
    // A timer notification runs after module evaluation installs its reactions;
    // unlike sleeping, this cannot cancel before the pending await is reached.
    ready.recv_timeout(Duration::from_secs(10)).unwrap();
    let result = mode
        .wait_output(
            &serde_json::json!({
                "cell_id": running.cell_id.unwrap(),
                "terminate": true,
                "yield_time_ms": 10000,
            })
            .to_string(),
            &cancel,
        )
        .unwrap();
    assert!(result.failed);
    assert!(result.summary().contains("Cell terminated"));
    let next = mode
        .exec_output(
            "await Promise.resolve(); text('recovered'); exit(); text('unreachable');",
            "next",
            &cancel,
        )
        .unwrap();
    assert!(!next.failed);
    assert!(next.summary().contains("recovered"));
    assert!(!next.summary().contains("unreachable"));
}
