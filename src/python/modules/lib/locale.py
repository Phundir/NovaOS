"""locale for Nova's Python: the "C" locale and the locale-aware helpers.

Nova has a single locale, the portable "C" locale of POSIX (text is
always UTF-8). setlocale() accepts "C", "POSIX" and "" (the user's
default, which on Nova is "C") and answers "C"; any other name raises
locale.Error('unsupported locale setting') as CPython does when the C
library lacks a locale. localeconv() returns the C locale's conventions
(decimal point ".", no grouping, no currency), and getlocale() returns
(None, None), CPython's answer for "C".

The number formatting functions (format_string, str, atof, atoi,
currency, delocalize, localize) are CPython 3.13's code (PSF License),
driven by localeconv(): they give the same results as CPython running
under LC_NUMERIC=C, and would follow another convention if
_override_localeconv were filled. nl_langinfo() answers for the C
locale, with CODESET 'UTF-8'.

Limit: normalize() has no copy of the X11 locale alias database; it
canonicalizes the language, territory and encoding spelling
('fr_fr.utf8' -> 'fr_FR.UTF-8', 'C.utf8' -> 'C.UTF-8') but does not add
the default encoding of a bare name ('en_US' stays 'en_US').
"""

import sys
import re
from builtins import str as _builtin_str

__all__ = ["getlocale", "getdefaultlocale", "getpreferredencoding", "Error",
           "setlocale", "localeconv", "strcoll", "strxfrm",
           "str", "atof", "atoi", "format_string", "currency",
           "normalize", "LC_CTYPE", "LC_COLLATE", "LC_TIME", "LC_MONETARY",
           "LC_NUMERIC", "LC_ALL", "CHAR_MAX", "getencoding", "LC_MESSAGES"]

CHAR_MAX = 127
LC_CTYPE = 0
LC_NUMERIC = 1
LC_TIME = 2
LC_COLLATE = 3
LC_MONETARY = 4
LC_MESSAGES = 5
LC_ALL = 6

_CATEGORIES = (LC_CTYPE, LC_NUMERIC, LC_TIME, LC_COLLATE, LC_MONETARY,
               LC_MESSAGES, LC_ALL)


class Error(Exception):
    pass


Error.__module__ = 'locale'


def _check_category(category, query):
    if not isinstance(category, int):
        raise TypeError("'%s' object cannot be interpreted as an integer"
                        % type(category).__name__)
    if category not in _CATEGORIES:
        raise Error('locale query failed' if query else
                    'unsupported locale setting')


def _localeconv():
    """The conventions of the C locale, in CPython's key order."""
    return {'int_curr_symbol': '',
            'currency_symbol': '',
            'mon_decimal_point': '',
            'mon_thousands_sep': '',
            'mon_grouping': [],
            'positive_sign': '',
            'negative_sign': '',
            'int_frac_digits': CHAR_MAX,
            'frac_digits': CHAR_MAX,
            'p_cs_precedes': CHAR_MAX,
            'p_sep_by_space': CHAR_MAX,
            'n_cs_precedes': CHAR_MAX,
            'n_sep_by_space': CHAR_MAX,
            'p_sign_posn': CHAR_MAX,
            'n_sign_posn': CHAR_MAX,
            'decimal_point': '.',
            'thousands_sep': '',
            'grouping': []}


def _setlocale(category, value=None):
    """The low-level setlocale: only the C locale exists."""
    _check_category(category, value is None)
    if value is not None and not isinstance(value, _builtin_str):
        raise TypeError("setlocale() argument 2 must be str or None, not %s"
                        % type(value).__name__)
    if value not in (None, '', 'C', 'POSIX'):
        raise Error('unsupported locale setting')
    return 'C'


def strcoll(a, b):
    """strcoll(string, string) -> int. Compare two strings by code
    point, which is the collation of the C locale."""
    return (a > b) - (a < b)


def strxfrm(s):
    """strxfrm(string) -> string. The C locale's sort key: s itself."""
    return s


# With this dict, you can override some items of localeconv's return
# value. This is useful for testing purposes.
_override_localeconv = {}


def localeconv():
    """Return the numeric and monetary conventions of the locale."""
    d = _localeconv()
    if _override_localeconv:
        d.update(_override_localeconv)
    return d


### Number formatting APIs

def _grouping_intervals(grouping):
    last_interval = None
    for interval in grouping:
        if interval == CHAR_MAX:
            return
        if interval == 0:
            if last_interval is None:
                raise ValueError("invalid grouping")
            while True:
                yield last_interval
        yield interval
        last_interval = interval


def _group(s, monetary=False):
    """Insert the thousands separators, right to left."""
    conv = localeconv()
    thousands_sep = conv[monetary and 'mon_thousands_sep' or 'thousands_sep']
    grouping = conv[monetary and 'mon_grouping' or 'grouping']
    if not grouping:
        return (s, 0)
    if s[-1] == ' ':
        stripped = s.rstrip()
        right_spaces = s[len(stripped):]
        s = stripped
    else:
        right_spaces = ''
    left_spaces = ''
    groups = []
    for interval in _grouping_intervals(grouping):
        if not s or s[-1] not in "0123456789":
            left_spaces = s
            s = ''
            break
        groups.append(s[-interval:])
        s = s[:-interval]
    if s:
        groups.append(s)
    groups.reverse()
    return (
        left_spaces + thousands_sep.join(groups) + right_spaces,
        len(thousands_sep) * (len(groups) - 1)
    )


def _strip_padding(s, amount):
    """Remove `amount` characters of padding, left first."""
    lpos = 0
    while amount and s[lpos] == ' ':
        lpos += 1
        amount -= 1
    rpos = len(s) - 1
    while amount and s[rpos] == ' ':
        rpos -= 1
        amount -= 1
    return s[lpos:rpos + 1]


_percent_re = re.compile(r'%(?:\((?P<key>.*?)\))?'
                         r'(?P<modifiers>[-#0-9 +*.hlL]*?)[eEfFgGdiouxXcrs%]')


def _format(percent, value, grouping=False, monetary=False, *additional):
    if additional:
        formatted = percent % ((value,) + additional)
    else:
        formatted = percent % value
    if percent[-1] in 'eEfFgGdiu':
        formatted = _localize(formatted, grouping, monetary)
    return formatted


def _localize(formatted, grouping=False, monetary=False):
    """Apply the locale's decimal point and grouping to a number."""
    if '.' in formatted:
        seps = 0
        parts = formatted.split('.')
        if grouping:
            parts[0], seps = _group(parts[0], monetary=monetary)
        decimal_point = localeconv()[monetary and 'mon_decimal_point'
                                     or 'decimal_point']
        formatted = decimal_point.join(parts)
        if seps:
            formatted = _strip_padding(formatted, seps)
    else:
        seps = 0
        if grouping:
            formatted, seps = _group(formatted, monetary=monetary)
        if seps:
            formatted = _strip_padding(formatted, seps)
    return formatted


def _is_mapping(val):
    if isinstance(val, dict):
        return True
    try:
        import collections.abc
        return isinstance(val, collections.abc.Mapping)
    except ImportError:
        return hasattr(val, 'keys') and hasattr(val, '__getitem__')


def format_string(f, val, grouping=False, monetary=False):
    """Format like `f % val`, taking the locale into account.

    Grouping is applied if grouping is true; monetary selects the
    monetary separators."""
    percents = list(_percent_re.finditer(f))
    new_f = _percent_re.sub('%s', f)

    if _is_mapping(val):
        new_val = []
        for perc in percents:
            if perc.group()[-1] == '%':
                new_val.append('%')
            else:
                new_val.append(_format(perc.group(), val, grouping, monetary))
    else:
        if not isinstance(val, tuple):
            val = (val,)
        new_val = []
        i = 0
        for perc in percents:
            if perc.group()[-1] == '%':
                new_val.append('%')
            else:
                starcount = perc.group('modifiers').count('*')
                new_val.append(_format(perc.group(),
                                       val[i],
                                       grouping,
                                       monetary,
                                       *val[i + 1:i + 1 + starcount]))
                i += (1 + starcount)
    val = tuple(new_val)
    return new_f % val


def currency(val, symbol=True, grouping=False, international=False):
    """Format val according to the currency settings of the locale."""
    conv = localeconv()
    digits = conv[international and 'int_frac_digits' or 'frac_digits']
    if digits == 127:
        raise ValueError("Currency formatting is not possible using "
                         "the 'C' locale.")

    s = _localize(f'{abs(val):.{digits}f}', grouping, monetary=True)
    # '<' and '>' mark where the sign goes between symbol and value
    s = '<' + s + '>'

    if symbol:
        smb = conv[international and 'int_curr_symbol' or 'currency_symbol']
        precedes = conv[val < 0 and 'n_cs_precedes' or 'p_cs_precedes']
        separated = conv[val < 0 and 'n_sep_by_space' or 'p_sep_by_space']
        if precedes:
            s = smb + (separated and ' ' or '') + s
        else:
            if international and smb[-1] == ' ':
                smb = smb[:-1]
            s = s + (separated and ' ' or '') + smb

    sign_pos = conv[val < 0 and 'n_sign_posn' or 'p_sign_posn']
    sign = conv[val < 0 and 'negative_sign' or 'positive_sign']

    if sign_pos == 0:
        s = '(' + s + ')'
    elif sign_pos == 1:
        s = sign + s
    elif sign_pos == 2:
        s = s + sign
    elif sign_pos == 3:
        s = s.replace('<', sign)
    elif sign_pos == 4:
        s = s.replace('>', sign)
    else:
        s = sign + s
    return s.replace('<', '').replace('>', '')


def str(val):
    """Convert float to string, taking the locale into account."""
    return _format("%.12g", val)


def delocalize(string):
    """Parse a string as a normalized number according to the locale."""
    conv = localeconv()
    ts = conv['thousands_sep']
    if ts:
        string = string.replace(ts, '')
    dd = conv['decimal_point']
    if dd:
        string = string.replace(dd, '.')
    return string


def localize(string, grouping=False, monetary=False):
    """Format a normalized number string according to the locale."""
    return _localize(string, grouping, monetary)


def atof(string, func=float):
    """Parse a string as a float according to the locale settings."""
    return func(delocalize(string))


def atoi(string):
    """Convert a string to an integer according to the locale settings."""
    return int(delocalize(string))


### Locale names

_encoding_names = {
    'utf8': 'UTF-8', 'utf': 'UTF-8',
    'latin1': 'ISO8859-1', 'iso88591': 'ISO8859-1', 'l1': 'ISO8859-1',
    'iso885915': 'ISO8859-15', 'latin9': 'ISO8859-15',
    'iso88592': 'ISO8859-2', 'latin2': 'ISO8859-2',
    'ascii': 'ISO8859-1', 'usascii': 'ISO8859-1',
    'ansix341968': 'ANSI_X3.4-1968',
    'eucjp': 'eucJP', 'euckr': 'eucKR', 'gb2312': 'eucCN', 'big5': 'big5',
    'koi8r': 'KOI8-R', 'cp1251': 'CP1251', 'cp1252': 'CP1252',
}


def normalize(localename):
    """Return a normalized locale code for the given locale name
    ('fr_fr.utf8' -> 'fr_FR.UTF-8'); unknown spellings come back
    unchanged."""
    code = localename.lower()
    if ':' in code:
        code = code.replace(':', '.')
    if '@' in code:
        code, modifier = code.split('@', 1)
    else:
        modifier = ''
    if '.' in code:
        langname, encoding = code.split('.')[:2]
    else:
        langname = code
        encoding = ''

    if langname in ('c', 'posix'):
        lang = 'C'
    elif re.fullmatch(r'[a-z]{2,3}(_[a-z]{2})?', langname):
        if '_' in langname:
            lang, territory = langname.split('_')
            lang = lang + '_' + territory.upper()
        else:
            lang = langname
    else:
        return localename

    if encoding:
        key = encoding.replace('-', '').replace('_', '')
        enc = _encoding_names.get(key)
        if enc is None:
            return localename
        result = lang + '.' + enc
    else:
        result = lang
    if modifier:
        result += '@' + modifier
    return result


def _parse_localename(localename):
    """Return (language code, encoding) for a locale name."""
    code = normalize(localename)
    if '@' in code:
        code, modifier = code.split('@', 1)
        if modifier == 'euro' and '.' not in code:
            return code, 'iso-8859-15'
    if '.' in code:
        return tuple(code.split('.')[:2])
    elif code == 'C':
        return None, None
    elif code == 'UTF-8':
        return None, 'UTF-8'
    raise ValueError('unknown locale: %s' % localename)


def _build_localename(localetuple):
    """Build a locale code from (language code, encoding)."""
    try:
        language, encoding = localetuple
        if language is None:
            language = 'C'
        if encoding is None:
            return language
        else:
            return language + '.' + encoding
    except (TypeError, ValueError):
        raise TypeError('Locale must be None, a string, or an iterable of '
                        'two strings -- language code, encoding.') from None


def getdefaultlocale(envvars=('LC_ALL', 'LC_CTYPE', 'LANG', 'LANGUAGE')):
    """Return (language code, encoding) from the first environment
    variable of `envvars` that is set; (None, None) for "C"."""
    import os
    lookup = os.environ.get
    for variable in envvars:
        localename = lookup(variable, None)
        if localename:
            if variable == 'LANGUAGE':
                localename = localename.split(':')[0]
            break
    else:
        localename = 'C'
    return _parse_localename(localename)


def getlocale(category=LC_CTYPE):
    """Return the current setting of a category as (language code,
    encoding): (None, None) for the C locale."""
    localename = _setlocale(category)
    if category == LC_ALL and ';' in localename:
        raise TypeError('category LC_ALL is not supported')
    return _parse_localename(localename)


def setlocale(category, locale=None):
    """Set (or with locale=None, query) the locale of a category.

    Only "C" (also spelled "POSIX", or "" for the default) is available;
    the return value is always 'C'."""
    if locale and not isinstance(locale, _builtin_str):
        locale = normalize(_build_localename(locale))
    return _setlocale(category, locale)


def getencoding():
    """Return the encoding of the current locale: Nova's text is UTF-8."""
    return 'utf-8'


def getpreferredencoding(do_setlocale=True):
    """Return the charset that the user is likely using: 'utf-8'."""
    return 'utf-8'


### nl_langinfo, with the glibc item numbers

CODESET = 14
RADIXCHAR = 65536
THOUSEP = 65537
ABDAY_1, ABDAY_2, ABDAY_3, ABDAY_4, ABDAY_5, ABDAY_6, ABDAY_7 = range(131072, 131079)
DAY_1, DAY_2, DAY_3, DAY_4, DAY_5, DAY_6, DAY_7 = range(131079, 131086)
(ABMON_1, ABMON_2, ABMON_3, ABMON_4, ABMON_5, ABMON_6, ABMON_7, ABMON_8,
 ABMON_9, ABMON_10, ABMON_11, ABMON_12) = range(131086, 131098)
(MON_1, MON_2, MON_3, MON_4, MON_5, MON_6, MON_7, MON_8, MON_9, MON_10,
 MON_11, MON_12) = range(131098, 131110)
AM_STR = 131110
PM_STR = 131111
D_T_FMT = 131112
D_FMT = 131113
T_FMT = 131114
T_FMT_AMPM = 131115
ERA = 131116
ERA_D_FMT = 131118
ALT_DIGITS = 131119
ERA_D_T_FMT = 131120
ERA_T_FMT = 131121
CRNCYSTR = 262159
YESEXPR = 327680
NOEXPR = 327681

_langinfo = {
    CODESET: 'UTF-8', RADIXCHAR: '.', THOUSEP: '',
    AM_STR: 'AM', PM_STR: 'PM',
    D_T_FMT: '%a %b %e %H:%M:%S %Y', D_FMT: '%m/%d/%y', T_FMT: '%H:%M:%S',
    T_FMT_AMPM: '%I:%M:%S %p', ERA: '', ERA_D_FMT: '', ALT_DIGITS: '',
    ERA_D_T_FMT: '', ERA_T_FMT: '', CRNCYSTR: '-',
    YESEXPR: '^[yY]', NOEXPR: '^[nN]',
}
for _i, _name in enumerate(('Sunday', 'Monday', 'Tuesday', 'Wednesday',
                            'Thursday', 'Friday', 'Saturday')):
    _langinfo[DAY_1 + _i] = _name
    _langinfo[ABDAY_1 + _i] = _name[:3]
for _i, _name in enumerate(('January', 'February', 'March', 'April', 'May',
                            'June', 'July', 'August', 'September', 'October',
                            'November', 'December')):
    _langinfo[MON_1 + _i] = _name
    _langinfo[ABMON_1 + _i] = _name[:3]
del _i, _name


def nl_langinfo(key):
    """Return the C locale's value of a langinfo item."""
    if not isinstance(key, int):
        raise TypeError("'%s' object cannot be interpreted as an integer"
                        % type(key).__name__)
    try:
        return _langinfo[key]
    except KeyError:
        raise ValueError('unsupported langinfo constant') from None
