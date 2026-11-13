(() => {
    const pending = new Map();
    const timers = new Map();
    let timerId = 0;
    const stringify = value => typeof value === 'string' ? value : (JSON.stringify(value) ?? 'undefined');
    globalThis.exit = () => { __exit(); throw new Error("Execution ended"); };
    globalThis.text = value => __text(stringify(value), false);
    globalThis.notify = value => __notify(stringify(value));
    globalThis.yield_control = () => { __text('', true); return Promise.resolve(); };
    globalThis.store = (key, value) => { if (typeof key !== 'string') throw new TypeError('store key must be a string'); const serialized = JSON.stringify(value); if (serialized === undefined) throw new TypeError('store value must be serializable'); __store(key, serialized); };
    globalThis.load = key => { const value = __load(key); return value == null ? undefined : JSON.parse(value); };
    globalThis.setTimeout = (callback, delay = 0) => { const id = ++timerId; timers.set(id, { callback, at: Date.now() + Math.min(2147483647, Math.max(0, Number(delay) || 0)) }); return id; };
    globalThis.clearTimeout = id => timers.delete(id);
    globalThis.__nextTimer = () => { let next = Infinity; for (const timer of timers.values()) next = Math.min(next, timer.at); return Number.isFinite(next) ? Math.max(0, next - Date.now()) : null; };
    globalThis.__tick = () => { const now = Date.now(); const due = [...timers].filter(([, timer]) => now >= timer.at); for (const [id, timer] of due) if (timers.delete(id)) timer.callback(); };
    globalThis.__deliver = (id, output, failed) => { const p = pending.get(id); if (!p) return; pending.delete(id); failed ? p.reject(new Error(output)) : p.resolve(output); };
    globalThis.tools = Object.freeze(Object.fromEntries(['exec_command','write_stdin'].map(name => [name, args => new Promise((resolve,reject) => { const id = __call(name, JSON.stringify(args)); pending.set(id, {resolve,reject}); })])));
    globalThis.ALL_TOOLS = Object.freeze([{name:'exec_command',description:'Run a shell command; returns output and exit code or session ID.'},{name:'write_stdin',description:'Send PTY input or poll an existing command session.'}]);
})();
