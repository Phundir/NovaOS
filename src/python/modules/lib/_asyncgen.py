"""Helpers of asynchronous generators (see src/python/coro.rs).

The generator's own frame yields two kinds of things: values marked by
`AsyncGenWrap` (its `yield`) and anything else (what an `await` inside it
suspends on). `_anext` resumes it until the first marked value, passing
the rest through to the task that awaits."""


def _is_value(out, marker):
    return type(out) is tuple and len(out) == 2 and out[0] is marker


def _anext(raw, marker, sent=None):
    error = None
    while True:
        try:
            if error is not None:
                e, error = error, None
                out = raw._throw(e)
            else:
                out = raw._send(sent)
        except StopIteration:
            raise StopAsyncIteration from None
        if _is_value(out, marker):
            return out[1]
        try:
            sent = yield out
        except BaseException as e:
            error = e
            sent = None


def _athrow(raw, marker, error):
    try:
        out = raw._throw(error)
    except StopIteration:
        raise StopAsyncIteration from None
    while not _is_value(out, marker):
        sent = yield out
        try:
            out = raw._send(sent)
        except StopIteration:
            raise StopAsyncIteration from None
    return out[1]


def _aclose(raw, marker):
    try:
        out = raw._throw(GeneratorExit)
    except (GeneratorExit, StopIteration, StopAsyncIteration):
        return
    if _is_value(out, marker):
        raise RuntimeError("async generator ignored GeneratorExit")
    yield out
