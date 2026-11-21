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
    globalThis.__deliver = (id, output, failed, structured) => { const p = pending.get(id); if (!p) return; pending.delete(id); failed ? p.reject(new Error(output)) : p.resolve(structured ? JSON.parse(output) : output); };
    const mediaUrl = (value, kind) => {
        if (typeof value === 'string') return value;
        if (!value || typeof value !== 'object' || Array.isArray(value)) throw new TypeError(`${kind} expects a data URL, ${kind}_url object, or ${kind} content block`);
        const url = value[`${kind}_url`];
        if (url !== undefined) { if (typeof url !== 'string') throw new TypeError(`${kind}_url must be a string`); return url; }
        if (value.type !== kind || typeof value.data !== 'string' || !value.data) throw new TypeError(`${kind} requires a nonempty ${kind} content block`);
        if (/^data:/i.test(value.data)) return value.data;
        const mime = value.mimeType ?? value.mime_type ?? 'application/octet-stream';
        if (typeof mime !== 'string') throw new TypeError(`Invalid ${kind} mimeType`);
        return `data:${mime};base64,${value.data}`;
    };
    globalThis.image = value => __media(mediaUrl(value, 'image'), false);
    globalThis.audio = value => {
        const url=mediaUrl(value,'audio');
        __media(url,true);
    };
    globalThis.generatedImage = result => {
        if (!result || typeof result !== 'object' || Array.isArray(result)) throw new TypeError('generatedImage expects {image_url, output_hint?}');
        const hint=result.output_hint;
        if (hint !== undefined && typeof hint !== 'string') throw new TypeError('output_hint must be a string');
        image(result);
        if (hint !== undefined) text(hint);
    };
    const emitBlock = block => {
        if (!block || typeof block !== 'object' || Array.isArray(block)) throw new TypeError('content expects an MCP content block or iterable of blocks');
        switch (block.type) {
        case 'text': if (block._meta?.['codex/encryptedContent'] === true) throw new TypeError('content cannot emit encrypted MCP text'); if (typeof block.text !== 'string') throw new TypeError('text block requires text'); text(block.text); break;
        case 'image': image(block); break;
        case 'audio': audio(block); break;
        case 'resource': if (!block.resource || typeof block.resource.uri !== 'string') throw new TypeError('resource block requires resource.uri'); text(JSON.stringify(block)); break;
        case 'resource_link': if (typeof block.uri !== 'string' || typeof block.name !== 'string') throw new TypeError('resource_link requires uri and name'); text(JSON.stringify(block)); break;
        default: throw new TypeError('Unsupported content block type: ' + block.type);
        }
    };
    globalThis.content = value => {
        const iterator = typeof value === 'string' ? undefined : value?.[Symbol.iterator];
        if (typeof iterator !== 'function') return emitBlock(value);
        for (const block of {[Symbol.iterator]: () => Reflect.apply(iterator, value, [])}) emitBlock(block);
    };
    globalThis.ALL_TOOLS = Object.freeze(JSON.parse(__tools));
    globalThis.tools = Object.freeze(Object.fromEntries(ALL_TOOLS.map(({name}) => [name, args => new Promise((resolve,reject) => { const id = __call(name, JSON.stringify(args ?? {})); pending.set(id, {resolve,reject}); })])));
})();
