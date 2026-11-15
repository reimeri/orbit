/* Exercise JS_ReadModule's failure path through the C API, without violating
   rquickjs::Module::load's valid-bytecode precondition. Run under Memcheck. */
#include <assert.h>
#include <string.h>
#include "quickjs.h"

int main(void)
{
    JSRuntime *rt = JS_NewRuntime();
    assert(rt);
    JSContext *ctx = JS_NewContext(rt);
    assert(ctx);
    const char code[] = "export const answer = 37;";
    JSValue mod = JS_Eval(ctx, code, strlen(code), "bytecode",
                         JS_EVAL_TYPE_MODULE | JS_EVAL_FLAG_COMPILE_ONLY);
    assert(!JS_IsException(mod));
    size_t len;
    uint8_t *bytes = JS_WriteObject(ctx, &len, mod, JS_WRITE_OBJ_BYTECODE);
    assert(bytes && len > 1);
    JS_FreeValue(ctx, mod);

    /* 0.11.0 has no bytecode checksum: preserving the header and truncating
       the final function byte reaches JS_ReadModule's cleanup after allocation. */
    JSValue broken = JS_ReadObject(ctx, bytes, len - 1, JS_READ_OBJ_BYTECODE);
    assert(JS_IsException(broken));
    JS_FreeValue(ctx, JS_GetException(ctx));

    mod = JS_ReadObject(ctx, bytes, len, JS_READ_OBJ_BYTECODE);
    js_free(ctx, bytes);
    assert(!JS_IsException(mod));
    JSModuleDef *definition = JS_VALUE_GET_PTR(mod);
    JSValue promise = JS_EvalFunction(ctx, mod); /* consumes mod */
    assert(!JS_IsException(promise));
    assert(JS_PromiseState(ctx, promise) == JS_PROMISE_FULFILLED);
    JSValue ns = JS_GetModuleNamespace(ctx, definition);
    assert(!JS_IsException(ns));
    JSValue value = JS_GetPropertyStr(ctx, ns, "answer");
    int32_t answer;
    assert(JS_ToInt32(ctx, &answer, value) == 0 && answer == 37);
    JS_FreeValue(ctx, value);
    JS_FreeValue(ctx, ns);
    JS_FreeValue(ctx, promise);
    JS_FreeContext(ctx);
    JS_FreeRuntime(rt);
    return 0;
}
