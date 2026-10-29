"""getpass for Nova's Python: prompt for a password, find the user name.

Nova's Python has no termios: the console cannot be asked to stop
echoing, so getpass() writes the prompt to `stream` (stderr by default)
and reads one line from sys.stdin, as CPython's fallback does. Unlike
that fallback it neither warns nor prints "Password input may be
echoed": on Nova this is the normal way, not a degraded one. A missing
input (end of file) raises EOFError; the trailing newline is removed.

getuser() looks at LOGNAME, USER, LNAME and USERNAME like CPython 3.13;
there is no password database to fall back on, so when none of them is
set it raises OSError('No username set in the environment').
"""

import os
import warnings  # installs UserWarning into builtins on Nova
import sys

__all__ = ["getpass", "getuser", "GetPassWarning"]


class GetPassWarning(UserWarning):
    pass


def _raw_input(prompt="", stream=None, input=None):
    if not stream:
        stream = sys.stderr
    if not input:
        input = sys.stdin
    prompt = str(prompt)
    if prompt:
        stream.write(prompt)
        stream.flush()
    line = input.readline()
    if not line:
        raise EOFError
    if line[-1] == '\n':
        line = line[:-1]
    return line


def getpass(prompt='Password: ', stream=None):
    """Prompt for a password and return the line typed, without its
    newline. The prompt goes to `stream` (default sys.stderr)."""
    return _raw_input(prompt, stream)


def fallback_getpass(prompt='Password: ', stream=None):
    """CPython's fallback: warn that the input may be echoed, then read."""
    import warnings
    warnings.warn("Can not control echo on the terminal.", GetPassWarning,
                  stacklevel=2)
    if not stream:
        stream = sys.stderr
    print("Warning: Password input may be echoed.", file=stream)
    return _raw_input(prompt, stream)


unix_getpass = getpass


def getuser():
    """Return the user name from the environment (LOGNAME, USER, LNAME,
    USERNAME, in that order); raise OSError when none is set."""
    for name in ('LOGNAME', 'USER', 'LNAME', 'USERNAME'):
        user = os.environ.get(name)
        if user:
            return user
    raise OSError('No username set in the environment')
