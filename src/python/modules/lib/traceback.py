"""traceback for Nova's Python: extracting, formatting and printing the
tracebacks of exceptions and the current stack, in CPython's format.

The interpreter keeps, for each exception, the frames it went through
(file, line, function, the columns of the failing expression); the
native `_traceback` module hands them over, and this module builds
CPython's FrameSummary / StackSummary / TracebackException objects and
texts from them. The library's own frames are not recorded, so library
paths never appear (CPython shows them).
"""

import sys
import _traceback

__all__ = ['extract_stack', 'extract_tb', 'format_exception',
           'format_exception_only', 'format_list', 'format_stack',
           'format_tb', 'print_exc', 'format_exc', 'print_exception',
           'print_last', 'print_stack', 'print_tb', 'clear_frames',
           'FrameSummary', 'StackSummary', 'TracebackException',
           'walk_stack', 'walk_tb', 'print_list']


def _write(text, file):
    if file is None:
        _traceback.write_stderr(text)
    else:
        file.write(text)


class FrameSummary:
    __slots__ = ('filename', 'lineno', 'end_lineno', 'colno', 'end_colno',
                 'name', '_line', 'locals')

    def __init__(self, filename, lineno, name, *, lookup_line=True,
                 locals=None, line=None, end_lineno=None, colno=None, end_colno=None):
        self.filename = filename
        self.lineno = lineno
        self.end_lineno = lineno if end_lineno is None else end_lineno
        self.colno = colno
        self.end_colno = end_colno
        self.name = name
        self._line = line
        self.locals = {k: repr(v) for k, v in locals.items()} if locals else None

    def __eq__(self, other):
        if isinstance(other, FrameSummary):
            return (self.filename == other.filename and
                    self.lineno == other.lineno and
                    self.name == other.name and
                    self.locals == other.locals)
        if isinstance(other, tuple):
            return (self.filename, self.lineno, self.name, self.line) == other
        return NotImplemented

    def __getitem__(self, pos):
        return (self.filename, self.lineno, self.name, self.line)[pos]

    def __iter__(self):
        return iter([self.filename, self.lineno, self.name, self.line])

    def __repr__(self):
        return "<FrameSummary file {filename}, line {lineno} in {name}>".format(
            filename=self.filename, lineno=self.lineno, name=self.name)

    def __len__(self):
        return 4

    @property
    def line(self):
        return self._line if self._line is not None else ''


def _summaries(entries):
    out = StackSummary()
    for filename, lineno, name, line, end_lineno, colno, end_colno in entries:
        out.append(FrameSummary(filename, lineno, name, line=line, end_lineno=end_lineno,
                                colno=colno, end_colno=end_colno))
    return out


class StackSummary(list):
    @classmethod
    def extract(klass, frame_gen, *, limit=None, lookup_lines=True, capture_locals=False):
        result = klass()
        for f in frame_gen:
            result.append(f)
        if limit is not None:
            result = klass(result[-limit:] if limit >= 0 else result[:-limit])
        return result

    @classmethod
    def from_list(klass, a_list):
        result = klass()
        for frame in a_list:
            if isinstance(frame, FrameSummary):
                result.append(frame)
            else:
                filename, lineno, name, line = frame
                result.append(FrameSummary(filename, lineno, name, line=line))
        return result

    def format_frame_summary(self, frame_summary):
        row = ['  File "{}", line {}, in {}\n'.format(
            frame_summary.filename, frame_summary.lineno, frame_summary.name)]
        if frame_summary.line:
            row.append('    {}\n'.format(frame_summary.line.strip()))
        if frame_summary.locals:
            for name, value in sorted(frame_summary.locals.items()):
                row.append('    {name} = {value}\n'.format(name=name, value=value))
        return ''.join(row)

    def format(self):
        result = []
        last_file = None
        last_line = None
        last_name = None
        count = 0
        for frame_summary in self:
            formatted = self.format_frame_summary(frame_summary)
            if formatted is None:
                continue
            if (last_file is None or last_file != frame_summary.filename or
                    last_line is None or last_line != frame_summary.lineno or
                    last_name is None or last_name != frame_summary.name):
                if count > 3:
                    count -= 3
                    result.append(
                        f'  [Previous line repeated {count} more '
                        f'time{"s" if count > 1 else ""}]\n'
                    )
                last_file = frame_summary.filename
                last_line = frame_summary.lineno
                last_name = frame_summary.name
                count = 0
            count += 1
            if count > 3:
                continue
            result.append(formatted)
        if count > 3:
            count -= 3
            result.append(
                f'  [Previous line repeated {count} more '
                f'time{"s" if count > 1 else ""}]\n'
            )
        return result


def _entries_of_tb(tb):
    out = []
    while tb is not None:
        entry = getattr(tb, '_entry', None)
        if entry is None:
            f = tb.tb_frame
            entry = (f.f_code.co_filename, tb.tb_lineno, f.f_code.co_name, None, None, None, None)
        out.append(entry)
        tb = tb.tb_next
    return out


def _limit(entries, limit):
    if limit is None:
        limit = getattr(sys, 'tracebacklimit', None)
    if limit is not None:
        if limit >= 0:
            return entries[:limit]
        return entries[limit:]
    return entries


def extract_tb(tb, limit=None):
    return _summaries(_limit(_entries_of_tb(tb), limit))


def format_list(extracted_list):
    return StackSummary.from_list(extracted_list).format()


def print_list(extracted_list, file=None):
    for item in StackSummary.from_list(extracted_list).format():
        _write(item, file)


def format_tb(tb, limit=None):
    return extract_tb(tb, limit=limit).format()


def print_tb(tb, limit=None, file=None):
    print_list(extract_tb(tb, limit=limit), file=file)


def _value_of(exc, value):
    if value is _sentinel_value:
        if isinstance(exc, type):
            return None
        return exc
    return value


_sentinel_value = object()


def _split_lines(text):
    """The text of a traceback as CPython's list: the header, one item per
    frame (its File line and source lines), then the exception lines."""
    lines = text.splitlines(keepends=True)
    out = []
    for line in lines:
        if line.startswith('    ') and out and out[-1].startswith('  File '):
            out[-1] += line
        elif line.startswith('    ') and out and out[-1].startswith('  File ') is False and out[-1].startswith('    '):
            out[-1] += line
        else:
            out.append(line)
    return out


def format_exception(exc, /, value=_sentinel_value, tb=_sentinel_value, limit=None, chain=True):
    value = _value_of(exc, value)
    if value is None:
        return ['NoneType: None\n']
    if limit is not None:
        te = TracebackException.from_exception(value, limit=limit)
        return list(te.format(chain=chain))
    return _split_lines(_traceback.format(value, chain))


def print_exception(exc, /, value=_sentinel_value, tb=_sentinel_value, limit=None, file=None, chain=True):
    for line in format_exception(exc, value, tb, limit=limit, chain=chain):
        _write(line, file)


def format_exception_only(exc, /, value=_sentinel_value, *, show_group=False):
    value = _value_of(exc, value)
    if value is None:
        return ['NoneType: None\n']
    return list(TracebackException.from_exception(value).format_exception_only(show_group=show_group))


def format_exc(limit=None, chain=True):
    return ''.join(format_exception(*sys.exc_info(), limit=limit, chain=chain))


def print_exc(limit=None, file=None, chain=True):
    print_exception(*sys.exc_info(), limit=limit, file=file, chain=chain)


def print_last(limit=None, file=None, chain=True):
    if not hasattr(sys, "last_exc") and not hasattr(sys, "last_type"):
        raise ValueError("no last exception")
    if hasattr(sys, "last_exc"):
        print_exception(sys.last_exc, limit=limit, file=file, chain=chain)
    else:
        print_exception(sys.last_type, sys.last_value, sys.last_traceback,
                        limit=limit, file=file, chain=chain)


def extract_stack(f=None, limit=None):
    entries = _traceback.stack()[:-1]
    entries = [e for e in entries if e[0] != '<traceback>']
    if limit is not None:
        entries = entries[-limit:] if limit >= 0 else entries[:-limit]
    return _summaries(entries)


def format_stack(f=None, limit=None):
    return format_list(extract_stack(f, limit=limit))


def print_stack(f=None, limit=None, file=None):
    print_list(extract_stack(f, limit=limit), file=file)


def walk_stack(f):
    return iter(())


def walk_tb(tb):
    while tb is not None:
        yield tb.tb_frame, tb.tb_lineno
        tb = tb.tb_next


def clear_frames(tb):
    pass


class TracebackException:
    def __init__(self, exc_type, exc_value, exc_traceback, *, limit=None,
                 lookup_lines=True, capture_locals=False, compact=False,
                 max_group_width=15, max_group_depth=10, _seen=None):
        if _seen is None:
            _seen = set()
        _seen.add(id(exc_value))
        self.exc_type = exc_type
        self._exc_value = exc_value
        self.exc_type_str = None if exc_type is None else self._qualname(exc_type)
        self._str = _traceback.exception_line(exc_value) if exc_value is not None else 'None'
        self.stack = _summaries(_limit(_traceback.entries(exc_value), limit)) if exc_value is not None else StackSummary()
        self.__notes__ = getattr(exc_value, '__notes__', None)
        self._limit = limit
        if exc_type is not None and issubclass(exc_type, SyntaxError):
            self.filename = exc_value.filename
            lno = exc_value.lineno
            self.lineno = str(lno) if lno is not None else None
            self.text = exc_value.text
            self.offset = exc_value.offset
            self.msg = exc_value.msg
        cause = exc_value.__cause__ if exc_value is not None else None
        context = exc_value.__context__ if exc_value is not None else None
        self.__suppress_context__ = exc_value.__suppress_context__ if exc_value is not None and hasattr(exc_value, '__suppress_context__') else False
        if cause is not None and id(cause) not in _seen:
            self.__cause__ = TracebackException(type(cause), cause, None, limit=limit, _seen=_seen)
        else:
            self.__cause__ = None
        if context is not None and id(context) not in _seen and not self.__suppress_context__:
            self.__context__ = TracebackException(type(context), context, None, limit=limit, _seen=_seen)
        else:
            self.__context__ = None
        self.exceptions = None

    @staticmethod
    def _qualname(t):
        module = getattr(t, '__module__', None)
        name = t.__qualname__
        if module not in ('__main__', 'builtins', None):
            return module + '.' + name
        return name

    @property
    def exc_type_str_(self):
        return self.exc_type_str

    @classmethod
    def from_exception(cls, exc, *args, **kwargs):
        return cls(type(exc), exc, getattr(exc, '__traceback__', None), *args, **kwargs)

    def __eq__(self, other):
        if isinstance(other, TracebackException):
            return self.__dict__ == other.__dict__
        return NotImplemented

    def __str__(self):
        return str(self._exc_value)

    def format_exception_only(self, *, show_group=False, _depth=0):
        if self.exc_type is None:
            yield 'NoneType: None\n'
            return
        if issubclass(self.exc_type, SyntaxError):
            if self.lineno is not None:
                yield '  File "{}", line {}\n'.format(self.filename or "<string>", self.lineno)
            if self.text is not None:
                yield '    {}\n'.format(self.text.strip())
                if self.offset is not None:
                    lead = len(self.text.rstrip('\n')) - len(self.text.rstrip('\n').lstrip())
                    yield '    {}^\n'.format(' ' * max(self.offset - 1 - lead, 0))
        yield self._str + '\n'
        if isinstance(self.__notes__, (list, tuple)):
            for note in self.__notes__:
                yield '{}\n'.format(note)

    def format(self, *, chain=True, _ctx=None):
        output = []
        exc = self
        if chain:
            while exc:
                if exc.__cause__ is not None:
                    chained_msg = ('\nThe above exception was the direct cause '
                                   'of the following exception:\n\n')
                    chained_exc = exc.__cause__
                elif exc.__context__ is not None and not exc.__suppress_context__:
                    chained_msg = ('\nDuring handling of the above exception, '
                                   'another exception occurred:\n\n')
                    chained_exc = exc.__context__
                else:
                    chained_msg = None
                    chained_exc = None
                output.append((chained_msg, exc))
                exc = chained_exc
        else:
            output.append((None, exc))
        for msg, exc in reversed(output):
            if msg is not None:
                yield msg
            if exc.stack:
                yield 'Traceback (most recent call last):\n'
                yield from exc.stack.format()
            yield from exc.format_exception_only()

    def print(self, *, file=None, chain=True):
        for line in self.format(chain=chain):
            _write(line, file)
