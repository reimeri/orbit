(() => {
    const { __exit, __text, __notify, __store, __load, __media, __tools, __call } = globalThis;
    for (const key of ['__exit','__text','__notify','__store','__load','__media','__tools','__call']) delete globalThis[key];
    const { stringify: encode, parse: decode } = JSON;
    const pending = new Map();
    const remember = pending.set.bind(pending), lookup = pending.get.bind(pending), forget = pending.delete.bind(pending);
    const timers = new Map();
    const now = Date.now, {min, max} = Math, number = Number, finite = Number.isFinite;
    const eachTimer = timers.forEach.bind(timers), setTimer = timers.set.bind(timers), removeTimer = timers.delete.bind(timers);
    let timerId = 0;
    const stringify = value => typeof value === 'string' ? value : (encode(value) ?? 'undefined');
    globalThis.exit = () => { __exit(); throw new Error("Execution ended"); };
    globalThis.text = value => __text(stringify(value), false);
    globalThis.notify = value => __notify(stringify(value));
    globalThis.yield_control = () => { __text('', true); return Promise.resolve(); };
    globalThis.store = (key, value) => { if (typeof key !== 'string') throw new TypeError('store key must be a string'); const serialized = encode(value); if (serialized === undefined) throw new TypeError('store value must be serializable'); __store(key, serialized); };
    globalThis.load = key => { const value = __load(key); return value == null ? undefined : decode(value); };
    globalThis.setTimeout = (callback, delay = 0) => { const id = ++timerId; setTimer(id, { callback, at: now() + min(2147483647, max(0, number(delay) || 0)) }); return id; };
    globalThis.clearTimeout = removeTimer;
    const nextTimer = () => { let next = Infinity; eachTimer(timer => {next = min(next, timer.at)}); return finite(next) ? max(0, next - now()) : null; };
    const tick = () => {
        const at = now(), last = timerId;
        eachTimer((timer, id) => { if (id <= last && timer.at <= at && removeTimer(id)) timer.callback(); });
    };
    const deliver = (id, output, failed, structured) => { const p = lookup(id); if (!p) return; forget(id); failed ? p.reject(new Error(output)) : p.resolve(structured ? decode(output) : output); };
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
        case 'resource': if (!block.resource || typeof block.resource.uri !== 'string') throw new TypeError('resource block requires resource.uri'); text(encode(block)); break;
        case 'resource_link': if (typeof block.uri !== 'string' || typeof block.name !== 'string') throw new TypeError('resource_link requires uri and name'); text(encode(block)); break;
        default: throw new TypeError('Unsupported content block type: ' + block.type);
        }
    };
    globalThis.content = value => {
        const iterator = typeof value === 'string' ? undefined : value?.[Symbol.iterator];
        if (typeof iterator !== 'function') return emitBlock(value);
        for (const block of {[Symbol.iterator]: () => Reflect.apply(iterator, value, [])}) emitBlock(block);
    };
    globalThis.ALL_TOOLS = Object.freeze(decode(__tools));
    globalThis.tools = Object.freeze(Object.fromEntries(ALL_TOOLS.map(({name}) => [name, args => new Promise((resolve,reject) => { const id = __call(name, encode(args ?? {})); remember(id, {resolve,reject}); })])));
    return {nextTimer, tick, deliver};
})();
