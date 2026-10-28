"""asyncio for Nova's Python: the event loop, futures, tasks, and the
usual tools (sleep, gather, wait_for, timeout, Queue, Event, Lock,
Semaphore, TaskGroup...).

The loop follows CPython's base_events._run_once step by step: a FIFO of
ready callbacks, a heap of timers, and each turn runs exactly the
callbacks that were ready when it began. Futures schedule their
callbacks, tasks step their coroutine on `call_soon`: programs print in
the same order as under CPython.
"""

import heapq
import time as _time
from collections import deque as _deque

ALL_COMPLETED = 'ALL_COMPLETED'
FIRST_COMPLETED = 'FIRST_COMPLETED'
FIRST_EXCEPTION = 'FIRST_EXCEPTION'

_PENDING = 'PENDING'
_CANCELLED = 'CANCELLED'
_FINISHED = 'FINISHED'


class CancelledError(BaseException):
    pass


TimeoutError = TimeoutError


class InvalidStateError(Exception):
    pass


class QueueEmpty(Exception):
    pass


class QueueFull(Exception):
    pass


class LimitOverrunError(Exception):
    pass


_running_loop = None
_current_tasks = {}
_all_tasks = []


# ----------------------------------------------------------------------
#  Handles and the loop
# ----------------------------------------------------------------------

class Handle:
    def __init__(self, callback, args, loop):
        self._callback = callback
        self._args = args
        self._loop = loop
        self._cancelled = False

    def cancel(self):
        self._cancelled = True

    def cancelled(self):
        return self._cancelled

    def _run(self):
        self._callback(*self._args)


class TimerHandle(Handle):
    _counter = 0

    def __init__(self, when, callback, args, loop):
        super().__init__(callback, args, loop)
        self._when = when
        TimerHandle._counter += 1
        self._seq = TimerHandle._counter

    def when(self):
        return self._when

    def __lt__(self, other):
        return (self._when, self._seq) < (other._when, other._seq)


class AbstractEventLoop:
    pass


class BaseEventLoop(AbstractEventLoop):
    def __init__(self):
        self._ready = _deque()
        self._scheduled = []
        self._stopping = False
        self._closed = False
        self._running = False

    def time(self):
        return _time.monotonic()

    def call_soon(self, callback, *args, context=None):
        handle = Handle(callback, args, self)
        self._ready.append(handle)
        return handle

    def call_later(self, delay, callback, *args, context=None):
        return self.call_at(self.time() + delay, callback, *args)

    def call_at(self, when, callback, *args, context=None):
        timer = TimerHandle(when, callback, args, self)
        heapq.heappush(self._scheduled, timer)
        return timer

    def create_future(self):
        return Future(loop=self)

    def create_task(self, coro, *, name=None, context=None):
        return Task(coro, loop=self, name=name)

    def is_running(self):
        return self._running

    def is_closed(self):
        return self._closed

    def stop(self):
        self._stopping = True

    def close(self):
        self._closed = True
        self._ready.clear()
        self._scheduled.clear()

    def _run_once(self):
        while self._scheduled and self._scheduled[0]._cancelled:
            heapq.heappop(self._scheduled)
        if not self._ready and self._scheduled:
            timeout = self._scheduled[0]._when - self.time()
            if timeout > 0:
                _time.sleep(timeout)
        end_time = self.time() + 1e-9
        while self._scheduled:
            handle = self._scheduled[0]
            if handle._when >= end_time:
                break
            heapq.heappop(self._scheduled)
            if not handle._cancelled:
                self._ready.append(handle)
        ntodo = len(self._ready)
        for _ in range(ntodo):
            handle = self._ready.popleft()
            if not handle._cancelled:
                handle._run()

    def run_forever(self):
        global _running_loop
        previous = _running_loop
        _running_loop = self
        self._running = True
        try:
            while True:
                self._run_once()
                if self._stopping:
                    break
        finally:
            self._stopping = False
            self._running = False
            _running_loop = previous

    def run_until_complete(self, future):
        new_task = not isfuture(future)
        future = ensure_future(future, loop=self)

        def done(fut):
            self.stop()

        future.add_done_callback(done)
        try:
            self.run_forever()
        except BaseException:
            if new_task and future.done() and not future.cancelled():
                future.exception()
            raise
        finally:
            future.remove_done_callback(done)
        if not future.done():
            raise RuntimeError('Event loop stopped before Future completed.')
        return future.result()

    def run_in_executor(self, executor, func, *args):
        future = self.create_future()
        try:
            future.set_result(func(*args))
        except BaseException as e:
            future.set_exception(e)
        return future


EventLoop = BaseEventLoop
_default_loop = None


def new_event_loop():
    return BaseEventLoop()


def get_running_loop():
    if _running_loop is None:
        raise RuntimeError('no running event loop')
    return _running_loop


def _get_running_loop():
    return _running_loop


def get_event_loop():
    global _default_loop
    if _running_loop is not None:
        return _running_loop
    if _default_loop is None or _default_loop.is_closed():
        _default_loop = new_event_loop()
    return _default_loop


def set_event_loop(loop):
    global _default_loop
    _default_loop = loop


# ----------------------------------------------------------------------
#  Futures
# ----------------------------------------------------------------------

class Future:
    _asyncio_future_blocking = False

    def __init__(self, *, loop=None):
        self._loop = loop if loop is not None else get_event_loop()
        self._state = _PENDING
        self._result = None
        self._exception = None
        self._callbacks = []
        self._cancel_message = None

    def get_loop(self):
        return self._loop

    def cancel(self, msg=None):
        if self._state != _PENDING:
            return False
        self._state = _CANCELLED
        self._cancel_message = msg
        self._schedule_callbacks()
        return True

    def _schedule_callbacks(self):
        callbacks = self._callbacks[:]
        self._callbacks[:] = []
        for callback in callbacks:
            self._loop.call_soon(callback, self)

    def cancelled(self):
        return self._state == _CANCELLED

    def done(self):
        return self._state != _PENDING

    def _make_cancelled_error(self):
        if self._cancel_message is None:
            return CancelledError()
        return CancelledError(self._cancel_message)

    def result(self):
        if self._state == _CANCELLED:
            raise self._make_cancelled_error()
        if self._state != _FINISHED:
            raise InvalidStateError('Result is not ready.')
        if self._exception is not None:
            raise self._exception
        return self._result

    def exception(self):
        if self._state == _CANCELLED:
            raise self._make_cancelled_error()
        if self._state != _FINISHED:
            raise InvalidStateError('Exception is not set.')
        return self._exception

    def add_done_callback(self, fn, *, context=None):
        if self._state != _PENDING:
            self._loop.call_soon(fn, self)
        else:
            self._callbacks.append(fn)

    def remove_done_callback(self, fn):
        filtered = [f for f in self._callbacks if f != fn]
        removed = len(self._callbacks) - len(filtered)
        if removed:
            self._callbacks[:] = filtered
        return removed

    def set_result(self, result):
        if self._state != _PENDING:
            raise InvalidStateError(f'{self._state}: {self!r}')
        self._result = result
        self._state = _FINISHED
        self._schedule_callbacks()

    def set_exception(self, exception):
        if self._state != _PENDING:
            raise InvalidStateError(f'{self._state}: {self!r}')
        if isinstance(exception, type):
            exception = exception()
        if type(exception) is StopIteration:
            raise TypeError('StopIteration interacts badly with generators and cannot be raised into a Future')
        self._exception = exception
        self._state = _FINISHED
        self._schedule_callbacks()

    def __await__(self):
        if not self.done():
            self._asyncio_future_blocking = True
            yield self
        if not self.done():
            raise RuntimeError("await wasn't used with future")
        return self.result()

    __iter__ = __await__

    def __repr__(self):
        return f'<Future {self._state.lower()}>'


def isfuture(obj):
    return getattr(obj, '_asyncio_future_blocking', None) is not None and not isinstance(obj, type)


def _set_result_unless_cancelled(fut, result):
    if fut.cancelled():
        return
    fut.set_result(result)


# ----------------------------------------------------------------------
#  Tasks
# ----------------------------------------------------------------------

class Task(Future):
    _counter = 0

    def __init__(self, coro, *, loop=None, name=None, context=None):
        super().__init__(loop=loop)
        if not iscoroutine(coro):
            raise TypeError(f'a coroutine was expected, got {coro!r}')
        Task._counter += 1
        self._name = f'Task-{Task._counter}' if name is None else str(name)
        self._coro = coro
        self._fut_waiter = None
        self._must_cancel = False
        self._num_cancels_requested = 0
        self._loop.call_soon(self._step)
        _all_tasks.append(self)

    def get_coro(self):
        return self._coro

    def get_name(self):
        return self._name

    def set_name(self, value):
        self._name = str(value)

    def cancelling(self):
        return self._num_cancels_requested

    def uncancel(self):
        if self._num_cancels_requested > 0:
            self._num_cancels_requested -= 1
        return self._num_cancels_requested

    def cancel(self, msg=None):
        if self.done():
            return False
        self._num_cancels_requested += 1
        if self._fut_waiter is not None:
            if self._fut_waiter.cancel(msg=msg):
                return True
        self._must_cancel = True
        self._cancel_message = msg
        return True

    def _step(self, exc=None):
        if self.done():
            return
        if self._must_cancel:
            if not isinstance(exc, CancelledError):
                exc = self._make_cancelled_error()
            self._must_cancel = False
        coro = self._coro
        self._fut_waiter = None
        _current_tasks[self._loop] = self
        try:
            if exc is None:
                result = coro.send(None)
            else:
                result = coro.throw(exc)
        except StopIteration as e:
            if self._must_cancel:
                self._must_cancel = False
                super().cancel(msg=self._cancel_message)
            else:
                super().set_result(e.value)
        except CancelledError as e:
            self._cancel_message = e.args[0] if e.args else None
            super().cancel()
        except (KeyboardInterrupt, SystemExit) as e:
            super().set_exception(e)
            raise
        except BaseException as e:
            super().set_exception(e)
        else:
            blocking = getattr(result, '_asyncio_future_blocking', None)
            if blocking is not None:
                if blocking:
                    result._asyncio_future_blocking = False
                    result.add_done_callback(self._wakeup)
                    self._fut_waiter = result
                    if self._must_cancel:
                        if self._fut_waiter.cancel(msg=self._cancel_message):
                            self._must_cancel = False
                else:
                    new_exc = RuntimeError(f'yield was used instead of yield from in task {self!r} with {result!r}')
                    self._loop.call_soon(self._step, new_exc)
            elif result is None:
                self._loop.call_soon(self._step)
            else:
                new_exc = RuntimeError(f'Task got bad yield: {result!r}')
                self._loop.call_soon(self._step, new_exc)
        finally:
            _current_tasks.pop(self._loop, None)

    def _wakeup(self, future):
        try:
            future.result()
        except BaseException as exc:
            self._step(exc)
        else:
            self._step()

    def __repr__(self):
        state = self._state.lower()
        return f"<Task {state} name='{self._name}'>"


def iscoroutine(obj):
    return type(obj).__name__ == 'coroutine'


def iscoroutinefunction(func):
    return getattr(func, '_is_coroutine', False) is True


def ensure_future(coro_or_future, *, loop=None):
    if isfuture(coro_or_future):
        return coro_or_future
    if loop is None:
        loop = get_event_loop()
    if iscoroutine(coro_or_future):
        return loop.create_task(coro_or_future)
    if hasattr(coro_or_future, '__await__'):
        async def _wrap(awaitable):
            return await awaitable
        return loop.create_task(_wrap(coro_or_future))
    raise TypeError('An asyncio.Future, a coroutine or an awaitable is required')


def create_task(coro, *, name=None, context=None):
    return get_running_loop().create_task(coro, name=name)


def current_task(loop=None):
    if loop is None:
        loop = get_running_loop()
    return _current_tasks.get(loop)


def all_tasks(loop=None):
    if loop is None:
        loop = get_running_loop()
    return {t for t in _all_tasks if t._loop is loop and not t.done()}


# ----------------------------------------------------------------------
#  sleep, gather, wait, wait_for, timeout, shield
# ----------------------------------------------------------------------

class _Sleep0:
    def __await__(self):
        yield


async def sleep(delay, result=None):
    if delay <= 0:
        await _Sleep0()
        return result
    loop = get_running_loop()
    future = loop.create_future()
    handle = loop.call_later(delay, _set_result_unless_cancelled, future, result)
    try:
        return await future
    finally:
        handle.cancel()


class _GatheringFuture(Future):
    def __init__(self, children, *, loop):
        super().__init__(loop=loop)
        self._children = children
        self._cancel_requested = False

    def cancel(self, msg=None):
        if self.done():
            return False
        ret = False
        for child in self._children:
            if child.cancel(msg=msg):
                ret = True
        if ret:
            self._cancel_requested = True
        return ret


def gather(*coros_or_futures, return_exceptions=False):
    if not coros_or_futures:
        loop = get_event_loop()
        outer = loop.create_future()
        outer.set_result([])
        return outer

    def _done_callback(fut):
        nonlocal nfinished
        nfinished += 1
        if outer is None or outer.done():
            if not fut.cancelled():
                fut.exception()
            return
        if not return_exceptions:
            if fut.cancelled():
                exc = fut._make_cancelled_error()
                outer.set_exception(exc)
                return
            else:
                exc = fut.exception()
                if exc is not None:
                    outer.set_exception(exc)
                    return
        if nfinished == nfuts:
            results = []
            for fut in children:
                if fut.cancelled():
                    res = fut._make_cancelled_error()
                else:
                    res = fut.exception()
                    if res is None:
                        res = fut.result()
                results.append(res)
            if outer._cancel_requested:
                exc = fut._make_cancelled_error()
                outer.set_exception(exc)
            else:
                outer.set_result(results)

    arg_to_fut = {}
    children = []
    nfuts = 0
    nfinished = 0
    loop = None
    outer = None
    for arg in coros_or_futures:
        if arg not in arg_to_fut:
            fut = ensure_future(arg, loop=loop)
            if loop is None:
                loop = fut._loop
            if fut is not arg:
                fut._log_destroy_pending = False
            nfuts += 1
            arg_to_fut[arg] = fut
            fut.add_done_callback(_done_callback)
        else:
            fut = arg_to_fut[arg]
        children.append(fut)
    outer = _GatheringFuture(children, loop=loop)
    return outer


async def _cancel_and_wait(fut):
    loop = get_running_loop()
    waiter = loop.create_future()

    def cb(f):
        if not waiter.done():
            waiter.set_result(None)

    fut.add_done_callback(cb)
    try:
        fut.cancel()
        await waiter
    finally:
        fut.remove_done_callback(cb)


async def wait(fs, *, timeout=None, return_when=ALL_COMPLETED):
    if isfuture(fs) or iscoroutine(fs):
        raise TypeError(f'expect a list of futures, not {type(fs).__name__}')
    if not fs:
        raise ValueError('Set of Tasks/Futures is empty.')
    loop = get_running_loop()
    fs = set(fs)
    if any(iscoroutine(f) for f in fs):
        raise TypeError('Passing coroutines is forbidden, use tasks explicitly.')
    return await _wait(fs, timeout, return_when, loop)


async def _wait(fs, timeout, return_when, loop):
    waiter = loop.create_future()
    timeout_handle = None
    if timeout is not None:
        timeout_handle = loop.call_later(timeout, _release_waiter, waiter)
    counter = len(fs)

    def _on_completion(f):
        nonlocal counter
        counter -= 1
        if (counter <= 0 or return_when == FIRST_COMPLETED or
                return_when == FIRST_EXCEPTION and (not f.cancelled() and f.exception() is not None)):
            if timeout_handle is not None:
                timeout_handle.cancel()
            if not waiter.done():
                waiter.set_result(None)

    for f in fs:
        f.add_done_callback(_on_completion)
    try:
        await waiter
    finally:
        if timeout_handle is not None:
            timeout_handle.cancel()
        for f in fs:
            f.remove_done_callback(_on_completion)
    done, pending = set(), set()
    for f in fs:
        if f.done():
            done.add(f)
        else:
            pending.add(f)
    return done, pending


def _release_waiter(waiter, *args):
    if not waiter.done():
        waiter.set_result(None)


class Timeout:
    def __init__(self, when):
        self._when = when
        self._state = 'CREATED'
        self._timeout_handler = None
        self._task = None

    def when(self):
        return self._when

    def reschedule(self, when):
        if self._timeout_handler is not None:
            self._timeout_handler.cancel()
        self._when = when
        if when is None:
            self._timeout_handler = None
        else:
            loop = get_running_loop()
            if when <= loop.time():
                self._timeout_handler = loop.call_soon(self._on_timeout)
            else:
                self._timeout_handler = loop.call_at(when, self._on_timeout)

    def expired(self):
        return self._state in ('EXPIRING', 'EXPIRED')

    async def __aenter__(self):
        self._state = 'ENTERED'
        self._task = current_task()
        self._cancelling = self._task.cancelling()
        self.reschedule(self._when)
        return self

    async def __aexit__(self, exc_type, exc_val, exc_tb):
        if self._timeout_handler is not None:
            self._timeout_handler.cancel()
            self._timeout_handler = None
        if self._state == 'EXPIRING':
            self._state = 'EXPIRED'
            if self._task.uncancel() <= self._cancelling and exc_type is not None:
                if issubclass(exc_type, CancelledError):
                    raise TimeoutError from exc_val
        elif self._state == 'ENTERED':
            self._state = 'EXITED'
        return None

    def _on_timeout(self):
        self._task.cancel()
        self._state = 'EXPIRING'
        self._timeout_handler = None


def timeout(delay):
    loop = get_running_loop()
    return Timeout(loop.time() + delay if delay is not None else None)


def timeout_at(when):
    return Timeout(when)


async def wait_for(fut, timeout):
    if timeout is not None and timeout <= 0:
        fut = ensure_future(fut)
        if fut.done():
            return fut.result()
        await _cancel_and_wait(fut)
        try:
            return fut.result()
        except CancelledError as exc:
            raise TimeoutError from exc
    async with Timeout(get_running_loop().time() + timeout if timeout is not None else None):
        return await fut


def shield(arg):
    inner = ensure_future(arg)
    if inner.done():
        return inner
    loop = inner._loop
    outer = loop.create_future()

    def _inner_done_callback(inner):
        if outer.cancelled():
            return
        if inner.cancelled():
            outer.cancel()
        else:
            exc = inner.exception()
            if exc is not None:
                outer.set_exception(exc)
            else:
                outer.set_result(inner.result())

    def _outer_done_callback(outer):
        if not inner.done():
            inner.remove_done_callback(_inner_done_callback)

    inner.add_done_callback(_inner_done_callback)
    outer.add_done_callback(_outer_done_callback)
    return outer


def as_completed(fs, *, timeout=None):
    if isfuture(fs) or iscoroutine(fs):
        raise TypeError(f'expect an iterable of futures, not {type(fs).__name__}')
    loop = get_event_loop()
    todo = {ensure_future(f, loop=loop) for f in set(fs)}
    done = Queue()
    timeout_handle = None

    def _on_timeout():
        for f in todo:
            f.remove_done_callback(_on_completion)
            done.put_nowait(None)
        todo.clear()

    def _on_completion(f):
        if not todo:
            return
        todo.remove(f)
        done.put_nowait(f)
        if not todo and timeout_handle is not None:
            timeout_handle.cancel()

    async def _wait_for_one():
        f = await done.get()
        if f is None:
            raise TimeoutError
        return f.result()

    for f in todo:
        f.add_done_callback(_on_completion)
    if todo and timeout is not None:
        timeout_handle = loop.call_later(timeout, _on_timeout)
    for _ in range(len(todo)):
        yield _wait_for_one()


# ----------------------------------------------------------------------
#  run
# ----------------------------------------------------------------------

def _cancel_all_tasks(loop):
    to_cancel = [t for t in _all_tasks if t._loop is loop and not t.done()]
    if not to_cancel:
        return
    for task in to_cancel:
        task.cancel()
    loop.run_until_complete(gather(*to_cancel, return_exceptions=True))


def run(main, *, debug=None):
    if _running_loop is not None:
        raise RuntimeError('asyncio.run() cannot be called from a running event loop')
    if not iscoroutine(main):
        raise ValueError(f'a coroutine was expected, got {main!r}')
    loop = new_event_loop()
    set_event_loop(loop)
    try:
        return loop.run_until_complete(main)
    finally:
        try:
            _cancel_all_tasks(loop)
        finally:
            set_event_loop(None)
            loop.close()
            _all_tasks[:] = [t for t in _all_tasks if t._loop is not loop]


class Runner:
    def __init__(self, *, debug=None, loop_factory=None):
        self._loop = None

    def __enter__(self):
        self._loop = new_event_loop()
        return self

    def __exit__(self, *exc):
        _cancel_all_tasks(self._loop)
        self._loop.close()

    def run(self, coro):
        return self._loop.run_until_complete(coro)

    def get_loop(self):
        return self._loop


# ----------------------------------------------------------------------
#  Synchronization: Event, Lock, Semaphore, Condition
# ----------------------------------------------------------------------

class Event:
    def __init__(self):
        self._waiters = _deque()
        self._value = False

    def is_set(self):
        return self._value

    def set(self):
        if not self._value:
            self._value = True
            for fut in self._waiters:
                if not fut.done():
                    fut.set_result(True)

    def clear(self):
        self._value = False

    async def wait(self):
        if self._value:
            return True
        fut = get_running_loop().create_future()
        self._waiters.append(fut)
        try:
            await fut
            return True
        finally:
            self._waiters.remove(fut)


class Lock:
    def __init__(self):
        self._waiters = None
        self._locked = False

    def locked(self):
        return self._locked

    async def acquire(self):
        if not self._locked and (self._waiters is None or all(w.cancelled() for w in self._waiters)):
            self._locked = True
            return True
        if self._waiters is None:
            self._waiters = _deque()
        fut = get_running_loop().create_future()
        self._waiters.append(fut)
        try:
            try:
                await fut
            finally:
                self._waiters.remove(fut)
        except CancelledError:
            if not self._locked:
                self._wake_up_first()
            raise
        self._locked = True
        return True

    def release(self):
        if self._locked:
            self._locked = False
            self._wake_up_first()
        else:
            raise RuntimeError('Lock is not acquired.')

    def _wake_up_first(self):
        if not self._waiters:
            return
        fut = next(iter(self._waiters))
        if not fut.done():
            fut.set_result(True)

    async def __aenter__(self):
        await self.acquire()
        return None

    async def __aexit__(self, exc_type, exc, tb):
        self.release()


class Semaphore:
    def __init__(self, value=1):
        if value < 0:
            raise ValueError('Semaphore initial value must be >= 0')
        self._waiters = None
        self._value = value

    def locked(self):
        return self._value == 0 or any(not w.cancelled() for w in (self._waiters or ()))

    async def acquire(self):
        if not self.locked():
            self._value -= 1
            return True
        if self._waiters is None:
            self._waiters = _deque()
        fut = get_running_loop().create_future()
        self._waiters.append(fut)
        try:
            try:
                await fut
            finally:
                self._waiters.remove(fut)
        except CancelledError:
            if fut.done() and not fut.cancelled():
                self._value += 1
            raise
        finally:
            while self._value > 0:
                if not self._wake_up_next():
                    break
        return True

    def release(self):
        self._value += 1
        self._wake_up_next()

    def _wake_up_next(self):
        if not self._waiters:
            return False
        for fut in self._waiters:
            if not fut.done():
                self._value -= 1
                fut.set_result(True)
                return True
        return False

    async def __aenter__(self):
        await self.acquire()
        return None

    async def __aexit__(self, exc_type, exc, tb):
        self.release()


class BoundedSemaphore(Semaphore):
    def __init__(self, value=1):
        self._bound_value = value
        super().__init__(value)

    def release(self):
        if self._value >= self._bound_value:
            raise ValueError('BoundedSemaphore released too many times')
        super().release()


class Condition:
    def __init__(self, lock=None):
        self._lock = lock if lock is not None else Lock()
        self._waiters = _deque()
        self.acquire = self._lock.acquire
        self.release = self._lock.release
        self.locked = self._lock.locked

    async def __aenter__(self):
        await self._lock.acquire()
        return None

    async def __aexit__(self, exc_type, exc, tb):
        self._lock.release()

    async def wait(self):
        self.release()
        try:
            fut = get_running_loop().create_future()
            self._waiters.append(fut)
            try:
                await fut
                return True
            finally:
                self._waiters.remove(fut)
        finally:
            await self.acquire()

    async def wait_for(self, predicate):
        result = predicate()
        while not result:
            await self.wait()
            result = predicate()
        return result

    def notify(self, n=1):
        idx = 0
        for fut in self._waiters:
            if idx >= n:
                break
            if not fut.done():
                idx += 1
                fut.set_result(False)

    def notify_all(self):
        self.notify(len(self._waiters))


# ----------------------------------------------------------------------
#  Queues
# ----------------------------------------------------------------------

class Queue:
    def __init__(self, maxsize=0):
        self._maxsize = maxsize
        self._getters = _deque()
        self._putters = _deque()
        self._unfinished_tasks = 0
        self._finished = Event()
        self._finished.set()
        self._init(maxsize)

    def _init(self, maxsize):
        self._queue = _deque()

    def _get(self):
        return self._queue.popleft()

    def _put(self, item):
        self._queue.append(item)

    def _wakeup_next(self, waiters):
        while waiters:
            waiter = waiters.popleft()
            if not waiter.done():
                waiter.set_result(None)
                break

    def qsize(self):
        return len(self._queue)

    @property
    def maxsize(self):
        return self._maxsize

    def empty(self):
        return not self._queue

    def full(self):
        if self._maxsize <= 0:
            return False
        return self.qsize() >= self._maxsize

    async def put(self, item):
        while self.full():
            putter = get_running_loop().create_future()
            self._putters.append(putter)
            try:
                await putter
            except BaseException:
                putter.cancel()
                try:
                    self._putters.remove(putter)
                except ValueError:
                    pass
                if not self.full() and not putter.cancelled():
                    self._wakeup_next(self._putters)
                raise
        return self.put_nowait(item)

    def put_nowait(self, item):
        if self.full():
            raise QueueFull
        self._put(item)
        self._unfinished_tasks += 1
        self._finished.clear()
        self._wakeup_next(self._getters)

    async def get(self):
        while self.empty():
            getter = get_running_loop().create_future()
            self._getters.append(getter)
            try:
                await getter
            except BaseException:
                getter.cancel()
                try:
                    self._getters.remove(getter)
                except ValueError:
                    pass
                if not self.empty() and not getter.cancelled():
                    self._wakeup_next(self._getters)
                raise
        return self.get_nowait()

    def get_nowait(self):
        if self.empty():
            raise QueueEmpty
        item = self._get()
        self._wakeup_next(self._putters)
        return item

    def task_done(self):
        if self._unfinished_tasks <= 0:
            raise ValueError('task_done() called too many times')
        self._unfinished_tasks -= 1
        if self._unfinished_tasks == 0:
            self._finished.set()

    async def join(self):
        if self._unfinished_tasks > 0:
            await self._finished.wait()


class PriorityQueue(Queue):
    def _init(self, maxsize):
        self._queue = []

    def _put(self, item):
        heapq.heappush(self._queue, item)

    def _get(self):
        return heapq.heappop(self._queue)


class LifoQueue(Queue):
    def _init(self, maxsize):
        self._queue = []

    def _put(self, item):
        self._queue.append(item)

    def _get(self):
        return self._queue.pop()


# ----------------------------------------------------------------------
#  TaskGroup
# ----------------------------------------------------------------------

class TaskGroup:
    def __init__(self):
        self._entered = False
        self._exiting = False
        self._aborting = False
        self._loop = None
        self._parent_task = None
        self._tasks = set()
        self._errors = []
        self._on_completed_fut = None

    async def __aenter__(self):
        self._entered = True
        self._loop = get_running_loop()
        self._parent_task = current_task()
        return self

    async def __aexit__(self, et, exc, tb):
        self._exiting = True
        if exc is not None and et is not None and not issubclass(et, CancelledError):
            self._errors.append(exc)
        if et is not None and not self._aborting:
            self._abort()
        while self._tasks:
            if self._on_completed_fut is None:
                self._on_completed_fut = self._loop.create_future()
            try:
                await self._on_completed_fut
            except CancelledError:
                if not self._aborting:
                    self._abort()
            self._on_completed_fut = None
        if self._errors:
            errors = self._errors
            self._errors = None
            if len(errors) == 1:
                raise errors[0]
            raise errors[0]
        return None

    def create_task(self, coro, *, name=None, context=None):
        if not self._entered:
            raise RuntimeError(f'TaskGroup {self!r} has not been entered')
        if self._exiting and not self._tasks:
            raise RuntimeError(f'TaskGroup {self!r} is finished')
        task = self._loop.create_task(coro, name=name)
        self._tasks.add(task)
        task.add_done_callback(self._on_task_done)
        return task

    def _abort(self):
        self._aborting = True
        for t in self._tasks:
            if not t.done():
                t.cancel()

    def _on_task_done(self, task):
        self._tasks.discard(task)
        if self._on_completed_fut is not None and not self._tasks:
            if not self._on_completed_fut.done():
                self._on_completed_fut.set_result(True)
        if task.cancelled():
            return
        exc = task.exception()
        if exc is None:
            return
        self._errors.append(exc)
        if not self._aborting:
            self._abort()
            if self._parent_task is not None and not self._parent_task.done():
                self._parent_task.cancel()


def to_thread(func, *args, **kwargs):
    async def run_now():
        return func(*args, **kwargs)
    return run_now()
