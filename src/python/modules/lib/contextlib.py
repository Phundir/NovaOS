"""contextlib for Nova's Python: contextmanager, suppress, closing,
nullcontext, ExitStack."""


class _GeneratorContextManager:
    def __init__(self, func, args, kwds):
        self.gen = func(*args, **kwds)
        self.func, self.args, self.kwds = func, args, kwds

    def _recreate_cm(self):
        return self.__class__(self.func, self.args, self.kwds)

    def __call__(self, func):
        def inner(*args, **kwds):
            with self._recreate_cm():
                return func(*args, **kwds)
        return inner

    def __enter__(self):
        del self.args, self.kwds, self.func
        try:
            return next(self.gen)
        except StopIteration:
            raise RuntimeError("generator didn't yield") from None

    def __exit__(self, typ, value, traceback):
        if typ is None:
            try:
                next(self.gen)
            except StopIteration:
                return False
            else:
                raise RuntimeError("generator didn't stop")
        else:
            if value is None:
                value = typ()
            try:
                self.gen.throw(value)
            except StopIteration as exc:
                return exc is not value
            except RuntimeError as exc:
                if exc is value:
                    return False
                if isinstance(value, StopIteration) and exc.__cause__ is value:
                    return False
                raise
            except BaseException as exc:
                if exc is not value:
                    raise
                return False
            raise RuntimeError("generator didn't stop after throw()")


def contextmanager(func):
    def helper(*args, **kwds):
        return _GeneratorContextManager(func, args, kwds)
    helper.__name__ = getattr(func, '__name__', 'helper')
    helper.__qualname__ = getattr(func, '__qualname__', 'helper')
    helper.__doc__ = getattr(func, '__doc__', None)
    helper.__wrapped__ = func
    return helper


class suppress:
    def __init__(self, *exceptions):
        self._exceptions = exceptions

    def __enter__(self):
        pass

    def __exit__(self, exctype, excinst, exctb):
        if exctype is None:
            return False
        return issubclass(exctype, self._exceptions)


class closing:
    def __init__(self, thing):
        self.thing = thing

    def __enter__(self):
        return self.thing

    def __exit__(self, *exc_info):
        self.thing.close()


class nullcontext:
    def __init__(self, enter_result=None):
        self.enter_result = enter_result

    def __enter__(self):
        return self.enter_result

    def __exit__(self, *excinfo):
        pass


class ExitStack:
    def __init__(self):
        self._exit_callbacks = []

    def __enter__(self):
        return self

    def enter_context(self, cm):
        result = type(cm).__enter__(cm)
        self._exit_callbacks.append(lambda t, v, tb: type(cm).__exit__(cm, t, v, tb))
        return result

    def callback(self, callback, /, *args, **kwds):
        def _exit_wrapper(exc_type, exc, tb):
            callback(*args, **kwds)
        self._exit_callbacks.append(_exit_wrapper)
        return callback

    def push(self, exit):
        self._exit_callbacks.append(exit)
        return exit

    def close(self):
        self.__exit__(None, None, None)

    def __exit__(self, *exc_details):
        received_exc = exc_details[0] is not None
        suppressed_exc = False
        pending_raise = False
        while self._exit_callbacks:
            cb = self._exit_callbacks.pop()
            try:
                if cb(*exc_details):
                    suppressed_exc = True
                    pending_raise = False
                    exc_details = (None, None, None)
            except BaseException as new_exc:
                exc_details = (type(new_exc), new_exc, None)
                pending_raise = True
        if pending_raise:
            raise exc_details[1]
        return received_exc and suppressed_exc


class redirect_stdout:
    def __init__(self, new_target):
        self._new_target = new_target

    def __enter__(self):
        return self._new_target

    def __exit__(self, exctype, excinst, exctb):
        pass
