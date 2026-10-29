"""timeit for Nova's Python: measure the execution time of small snippets.

CPython 3.13's design (PSF License): the statement and the setup are
pasted into the source of a function `inner(_it, _timer)` that is
compiled once and then runs the statement in a bare for-loop over
itertools.repeat(None, number), so the loop itself costs as little as
possible and nothing is looked up by name on each pass. Callables are
accepted in place of source strings and called from the same loop.

The default timer is time.perf_counter. CPython also switches the
garbage collector off while timing; Nova's Python has no gc module
(reference counting only), so there is nothing to switch off. main()
implements the command line of `python -m timeit`.
"""

import itertools
import sys
import time

__all__ = ["Timer", "timeit", "repeat", "default_timer"]

dummy_src_name = "<timeit-src>"
default_number = 1000000
default_repeat = 5
default_timer = time.perf_counter

_globals = globals

# Do not change the indentation of the template: reindent() relies on
# setup being indented 4 spaces and stmt 8 spaces.
template = """
def inner(_it, _timer{init}):
    {setup}
    _t0 = _timer()
    for _i in _it:
        {stmt}
        pass
    _t1 = _timer()
    return _t1 - _t0
"""

_USAGE = """\
Command line usage:
    python timeit.py [-n N] [-r N] [-s S] [-p] [-h] [--] [statement]

Options:
  -n/--number N: how many times to execute 'statement' (default: see below)
  -r/--repeat N: how many times to repeat the timer (default 5)
  -s/--setup S: statement to be executed once initially (default 'pass').
                Execution time of this setup statement is NOT timed.
  -p/--process: use time.process_time() (default is time.perf_counter())
  -v/--verbose: print raw timing results; repeat for more digits precision
  -u/--unit: set the output time unit (nsec, usec, msec, or sec)
  -h/--help: print this usage message and exit
  --: separate options from statement, use when statement starts with -
  statement: statement to be timed (default 'pass')
"""


def reindent(src, indent):
    """Helper to reindent a multi-line statement."""
    return src.replace("\n", "\n" + " " * indent)


class Timer:
    """Time the execution of a statement (a string or a callable).

    `setup` runs once before each timing; `globals` is the namespace the
    code runs in (timeit's own by default, as in CPython).
    """

    def __init__(self, stmt="pass", setup="pass", timer=default_timer,
                 globals=None):
        self.timer = timer
        local_ns = {}
        global_ns = _globals() if globals is None else globals
        init = ''
        if isinstance(setup, str):
            # Check that the code compiles outside a function.
            compile(setup, dummy_src_name, "exec")
            stmtprefix = setup + '\n'
            setup = reindent(setup, 4)
        elif callable(setup):
            local_ns['_setup'] = setup
            init += ', _setup=_setup'
            stmtprefix = ''
            setup = '_setup()'
        else:
            raise ValueError("setup is neither a string nor callable")
        if isinstance(stmt, str):
            compile(stmtprefix + stmt, dummy_src_name, "exec")
            stmt = reindent(stmt, 8)
        elif callable(stmt):
            local_ns['_stmt'] = stmt
            init += ', _stmt=_stmt'
            stmt = '_stmt()'
        else:
            raise ValueError("stmt is neither a string nor callable")
        src = template.format(stmt=stmt, setup=setup, init=init)
        self.src = src
        code = compile(src, dummy_src_name, "exec")
        exec(code, global_ns, local_ns)
        self.inner = local_ns["inner"]

    def print_exc(self, file=None):
        """Print the traceback of the exception being handled to `file`
        (default sys.stderr)."""
        import traceback
        (file or sys.stderr).write(traceback.format_exc())

    def timeit(self, number=default_number):
        """Time `number` executions of the statement, in seconds."""
        it = itertools.repeat(None, number)
        return self.inner(it, self.timer)

    def repeat(self, repeat=default_repeat, number=default_number):
        """Call timeit() `repeat` times and return the list of results."""
        r = []
        for i in range(repeat):
            t = self.timeit(number)
            r.append(t)
        return r

    def autorange(self, callback=None):
        """Return (number, time_taken) with time_taken >= 0.2 s, trying
        number = 1, 2, 5, 10, 20, 50, ... in turn."""
        i = 1
        while True:
            for j in 1, 2, 5:
                number = i * j
                time_taken = self.timeit(number)
                if callback:
                    callback(number, time_taken)
                if time_taken >= 0.2:
                    return (number, time_taken)
            i *= 10


def timeit(stmt="pass", setup="pass", timer=default_timer,
           number=default_number, globals=None):
    """Convenience function to create Timer object and call timeit method."""
    return Timer(stmt, setup, timer, globals).timeit(number)


def repeat(stmt="pass", setup="pass", timer=default_timer,
           repeat=default_repeat, number=default_number, globals=None):
    """Convenience function to create Timer object and call repeat method."""
    return Timer(stmt, setup, timer, globals).repeat(repeat, number)


def main(args=None, *, _wrap_timer=None):
    """The command line of `python -m timeit`; returns an exit code."""
    if args is None:
        args = sys.argv[1:]
    import getopt
    try:
        opts, args = getopt.getopt(args, "n:u:s:r:pvh",
                                   ["number=", "setup=", "repeat=",
                                    "process", "verbose", "unit=", "help"])
    except getopt.error as err:
        print(err)
        print("use -h/--help for command line help")
        return 2

    timer = default_timer
    stmt = "\n".join(args) or "pass"
    number = 0  # auto-determine
    setup = []
    repeat = default_repeat
    verbose = 0
    time_unit = None
    units = {"nsec": 1e-9, "usec": 1e-6, "msec": 1e-3, "sec": 1.0}
    precision = 3
    for o, a in opts:
        if o in ("-n", "--number"):
            number = int(a)
        if o in ("-s", "--setup"):
            setup.append(a)
        if o in ("-u", "--unit"):
            if a in units:
                time_unit = a
            else:
                print("Unrecognized unit. Please select nsec, usec, msec, or sec.",
                      file=sys.stderr)
                return 2
        if o in ("-r", "--repeat"):
            repeat = int(a)
            if repeat <= 0:
                repeat = 1
        if o in ("-p", "--process"):
            timer = time.process_time
        if o in ("-v", "--verbose"):
            if verbose:
                precision += 1
            verbose += 1
        if o in ("-h", "--help"):
            print(_USAGE, end=' ')
            return 0
    setup = "\n".join(setup) or "pass"

    import os
    sys.path.insert(0, os.curdir)
    if _wrap_timer is not None:
        timer = _wrap_timer(timer)

    t = Timer(stmt, setup, timer)
    if number == 0:
        callback = None
        if verbose:
            def callback(number, time_taken):
                msg = "{num} loop{s} -> {secs:.{prec}g} sec"
                plural = (number != 1)
                print(msg.format(num=number, s='s' if plural else '',
                                 secs=time_taken, prec=precision))
        try:
            number, _ = t.autorange(callback)
        except:
            t.print_exc()
            return 1
        if verbose:
            print()

    try:
        raw_timings = t.repeat(repeat, number)
    except:
        t.print_exc()
        return 1

    def format_time(dt):
        unit = time_unit
        if unit is not None:
            scale = units[unit]
        else:
            scales = [(scale, unit) for unit, scale in units.items()]
            scales.sort(reverse=True)
            for scale, unit in scales:
                if dt >= scale:
                    break
        return "%.*g %s" % (precision, dt / scale, unit)

    if verbose:
        print("raw times: %s" % ", ".join(map(format_time, raw_timings)))
        print()
    timings = [dt / number for dt in raw_timings]

    best = min(timings)
    print("%d loop%s, best of %d: %s per loop"
          % (number, 's' if number != 1 else '',
             repeat, format_time(best)))

    worst = max(timings)
    if worst >= best * 4:
        import warnings
        warnings.warn_explicit("The test results are likely unreliable. "
                               "The worst time (%s) was more than four times "
                               "slower than the best time (%s)."
                               % (format_time(worst), format_time(best)),
                               UserWarning, '', 0)
    return None


if __name__ == "__main__":
    sys.exit(main())
