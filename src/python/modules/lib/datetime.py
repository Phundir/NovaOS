"""datetime for Nova's Python: dates, times, durations and fixed zones.

The proleptic Gregorian calendar of CPython's datetime, its reprs, its
ISO 8601 formats (isoformat and fromisoformat), strftime and strptime
for the usual directives, arithmetic and comparisons with the same
rules (naive and aware objects do not mix). Nova's clock has no time
zone: local time is UTC, so now() and utcnow() agree.
"""

import time as _time
import math as _math

__all__ = ("date", "datetime", "time", "timedelta", "timezone", "tzinfo",
           "MINYEAR", "MAXYEAR", "UTC")

MINYEAR = 1
MAXYEAR = 9999
_MAXORDINAL = 3652059

_DAYS_IN_MONTH = [-1, 31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]

_DAYS_BEFORE_MONTH = [-1]
dbm = 0
for dim in _DAYS_IN_MONTH[1:]:
    _DAYS_BEFORE_MONTH.append(dbm)
    dbm += dim
del dbm, dim


def _cmp(x, y):
    return 0 if x == y else 1 if x > y else -1


def _is_leap(year):
    return year % 4 == 0 and (year % 100 != 0 or year % 400 == 0)


def _days_before_year(year):
    y = year - 1
    return y * 365 + y // 4 - y // 100 + y // 400


def _days_in_month(year, month):
    if month == 2 and _is_leap(year):
        return 29
    return _DAYS_IN_MONTH[month]


def _days_before_month(year, month):
    return _DAYS_BEFORE_MONTH[month] + (month > 2 and _is_leap(year))


def _ymd2ord(year, month, day):
    return (_days_before_year(year) +
            _days_before_month(year, month) +
            day)


_DI400Y = _days_before_year(401)
_DI100Y = _days_before_year(101)
_DI4Y = _days_before_year(5)


def _ord2ymd(n):
    n -= 1
    n400, n = divmod(n, _DI400Y)
    year = n400 * 400 + 1
    n100, n = divmod(n, _DI100Y)
    n4, n = divmod(n, _DI4Y)
    n1, n = divmod(n, 365)
    year += n100 * 100 + n4 * 4 + n1
    if n1 == 4 or n100 == 4:
        return year - 1, 12, 31
    leapyear = n1 == 3 and (n4 != 24 or n100 == 3)
    month = (n + 50) >> 5
    preceding = _DAYS_BEFORE_MONTH[month] + (month > 2 and leapyear)
    if preceding > n:
        month -= 1
        preceding -= _DAYS_IN_MONTH[month] + (month == 2 and leapyear)
    n -= preceding
    return year, month, n + 1


_MONTHNAMES = [None, "Jan", "Feb", "Mar", "Apr", "May", "Jun",
               "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"]
_DAYNAMES = [None, "Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"]
_FULLMONTHNAMES = [None, "January", "February", "March", "April", "May", "June",
                   "July", "August", "September", "October", "November", "December"]
_FULLDAYNAMES = [None, "Monday", "Tuesday", "Wednesday", "Thursday", "Friday",
                 "Saturday", "Sunday"]


def _build_struct_time(y, m, d, hh, mm, ss, dstflag):
    wday = (_ymd2ord(y, m, d) + 6) % 7
    dnum = _days_before_month(y, m) + d
    return _time.struct_time((y, m, d, hh, mm, ss, wday, dnum, dstflag))


def _format_time(hh, mm, ss, us, timespec='auto'):
    specs = {
        'hours': '{:02d}',
        'minutes': '{:02d}:{:02d}',
        'seconds': '{:02d}:{:02d}:{:02d}',
        'milliseconds': '{:02d}:{:02d}:{:02d}.{:03d}',
        'microseconds': '{:02d}:{:02d}:{:02d}.{:06d}'
    }
    if timespec == 'auto':
        timespec = 'microseconds' if us else 'seconds'
    elif timespec == 'milliseconds':
        us //= 1000
    try:
        fmt = specs[timespec]
    except KeyError:
        raise ValueError('Unknown timespec value')
    else:
        return fmt.format(hh, mm, ss, us)


def _format_offset(off, sep=':'):
    s = ''
    if off is not None:
        if off.days < 0:
            sign = "-"
            off = -off
        else:
            sign = "+"
        hh, mm = divmod(off, timedelta(hours=1))
        mm, ss = divmod(mm, timedelta(minutes=1))
        s += "%s%02d%s%02d" % (sign, hh, sep, mm)
        if ss or ss.microseconds:
            s += "%s%02d" % (sep, ss.seconds)
            if ss.microseconds:
                s += '.%06d' % ss.microseconds
    return s


def _wrap_strftime(object, format, timetuple):
    """Formats with the C directives, %z %Z %f %:z expanded here."""
    out = []
    i, n = 0, len(format)
    while i < n:
        ch = format[i]
        i += 1
        if ch != '%':
            out.append(ch)
            continue
        if i >= n:
            out.append('%')
            break
        ch = format[i]
        i += 1
        y, mo, d, hh, mi, ss, wd, yd = timetuple[:8]
        if ch == 'f':
            out.append('%06d' % getattr(object, 'microsecond', 0))
        elif ch == 'z':
            off = object.utcoffset() if hasattr(object, 'utcoffset') else None
            out.append(_format_offset(off, '') if off is not None else '')
        elif ch == ':' and i < n and format[i] == 'z':
            i += 1
            off = object.utcoffset() if hasattr(object, 'utcoffset') else None
            out.append(_format_offset(off, ':') if off is not None else '')
        elif ch == 'Z':
            name = object.tzname() if hasattr(object, 'tzname') else None
            out.append(name.replace('%', '%%').replace('%%', '%') if name else '')
        elif ch == 'Y':
            out.append('%d' % y if y >= 1000 else '%d' % y)
        elif ch == 'y':
            out.append('%02d' % (y % 100))
        elif ch == 'm':
            out.append('%02d' % mo)
        elif ch == 'd':
            out.append('%02d' % d)
        elif ch == 'e':
            out.append('%2d' % d)
        elif ch == 'H':
            out.append('%02d' % hh)
        elif ch == 'I':
            out.append('%02d' % (hh % 12 or 12))
        elif ch == 'M':
            out.append('%02d' % mi)
        elif ch == 'S':
            out.append('%02d' % ss)
        elif ch == 'p':
            out.append('AM' if hh < 12 else 'PM')
        elif ch == 'a':
            out.append(_DAYNAMES[wd + 1])
        elif ch == 'A':
            out.append(_FULLDAYNAMES[wd + 1])
        elif ch == 'b' or ch == 'h':
            out.append(_MONTHNAMES[mo])
        elif ch == 'B':
            out.append(_FULLMONTHNAMES[mo])
        elif ch == 'j':
            out.append('%03d' % yd)
        elif ch == 'w':
            out.append('%d' % ((wd + 1) % 7))
        elif ch == 'u':
            out.append('%d' % (wd + 1))
        elif ch == 'U':
            out.append('%02d' % ((yd + 6 - (wd + 1) % 7) // 7))
        elif ch == 'W':
            out.append('%02d' % ((yd + 6 - wd) // 7))
        elif ch == 'G' or ch == 'V':
            iy, iw, _ = _isocalendar(y, mo, d)
            out.append('%d' % iy if ch == 'G' else '%02d' % iw)
        elif ch == 'c':
            out.append('%s %s %2d %02d:%02d:%02d %d' % (_DAYNAMES[wd + 1], _MONTHNAMES[mo], d, hh, mi, ss, y))
        elif ch == 'x':
            out.append('%02d/%02d/%02d' % (mo, d, y % 100))
        elif ch == 'X':
            out.append('%02d:%02d:%02d' % (hh, mi, ss))
        elif ch == 'D':
            out.append('%02d/%02d/%02d' % (mo, d, y % 100))
        elif ch == 'F':
            out.append('%d-%02d-%02d' % (y, mo, d))
        elif ch == 'T':
            out.append('%02d:%02d:%02d' % (hh, mi, ss))
        elif ch == 'R':
            out.append('%02d:%02d' % (hh, mi))
        elif ch == 'C':
            out.append('%02d' % (y // 100))
        elif ch == 'n':
            out.append('\n')
        elif ch == 't':
            out.append('\t')
        elif ch == '%':
            out.append('%')
        else:
            out.append('%' + ch)
    return ''.join(out)


def _isocalendar(year, month, day):
    week1monday = _isoweek1monday(year)
    today = _ymd2ord(year, month, day)
    week, day = divmod(today - week1monday, 7)
    if week < 0:
        year -= 1
        week1monday = _isoweek1monday(year)
        week, day = divmod(today - week1monday, 7)
    elif week >= 52:
        if today >= _isoweek1monday(year + 1):
            year += 1
            week = 0
    return year, week + 1, day + 1


def _isoweek1monday(year):
    THURSDAY = 3
    firstday = _ymd2ord(year, 1, 1)
    firstweekday = (firstday + 6) % 7
    week1monday = firstday - firstweekday
    if firstweekday > THURSDAY:
        week1monday += 7
    return week1monday


def _isoweek_to_gregorian(year, week, day):
    if not MINYEAR <= year <= MAXYEAR:
        raise ValueError(f"Year is out of range: {year}")
    if not 0 < week < 53:
        out_of_range = True
        if week == 53:
            first_weekday = _ymd2ord(year, 1, 1) % 7
            if (first_weekday == 4 or (first_weekday == 3 and _is_leap(year))):
                out_of_range = False
        if out_of_range:
            raise ValueError(f"Invalid week: {week}")
    if not 0 < day < 8:
        raise ValueError(f"Invalid weekday: {day} (range is [1, 7])")
    day_offset = (week - 1) * 7 + (day - 1)
    day_1 = _isoweek1monday(year)
    ord_day = day_1 + day_offset
    return _ord2ymd(ord_day)


def _check_tzname(name):
    if name is not None and not isinstance(name, str):
        raise TypeError("tzinfo.tzname() must return None or string, "
                        "not '%s'" % type(name).__name__)


def _check_utc_offset(name, offset):
    assert name in ("utcoffset", "dst")
    if offset is None:
        return
    if not isinstance(offset, timedelta):
        raise TypeError("tzinfo.%s() must return None "
                        "or timedelta, not '%s'" % (name, type(offset).__name__))
    if not -timedelta(1) < offset < timedelta(1):
        raise ValueError("%s()=%s, must be strictly between "
                         "-timedelta(hours=24) and timedelta(hours=24)" %
                         (name, offset))


def _check_date_fields(year, month, day):
    year = _index(year)
    month = _index(month)
    day = _index(day)
    if not MINYEAR <= year <= MAXYEAR:
        raise ValueError(f'year {year} is out of range')
    if not 1 <= month <= 12:
        raise ValueError('month must be in 1..12')
    dim = _days_in_month(year, month)
    if not 1 <= day <= dim:
        raise ValueError('day is out of range for month')
    return year, month, day


def _check_time_fields(hour, minute, second, microsecond, fold):
    hour = _index(hour)
    minute = _index(minute)
    second = _index(second)
    microsecond = _index(microsecond)
    if not 0 <= hour <= 23:
        raise ValueError('hour must be in 0..23')
    if not 0 <= minute <= 59:
        raise ValueError('minute must be in 0..59')
    if not 0 <= second <= 59:
        raise ValueError('second must be in 0..59')
    if not 0 <= microsecond <= 999999:
        raise ValueError('microsecond must be in 0..999999')
    if fold not in (0, 1):
        raise ValueError('fold must be either 0 or 1')
    return hour, minute, second, microsecond, fold


def _index(value):
    if isinstance(value, int) and not isinstance(value, bool):
        return value
    if isinstance(value, float):
        raise TypeError("'float' object cannot be interpreted as an integer")
    if isinstance(value, bool):
        return int(value)
    try:
        return value.__index__()
    except AttributeError:
        raise TypeError("an integer is required (got type %s)" % type(value).__name__)


def _check_tzinfo_arg(tz):
    if tz is not None and not isinstance(tz, tzinfo):
        raise TypeError("tzinfo argument must be None or of a tzinfo subclass")


def _divide_and_round(a, b):
    q, r = divmod(a, b)
    r *= 2
    greater_than_half = r > b if b > 0 else r < b
    if greater_than_half or r == b and q % 2 == 1:
        q += 1
    return q


class timedelta:
    """Difference between two datetime values."""
    __nova_type_name__ = 'datetime.timedelta'

    __slots__ = '_days', '_seconds', '_microseconds', '_hashcode'

    def __new__(cls, days=0, seconds=0, microseconds=0,
                milliseconds=0, minutes=0, hours=0, weeks=0):
        for name, value in (
                ("days", days), ("seconds", seconds), ("microseconds", microseconds),
                ("milliseconds", milliseconds), ("minutes", minutes), ("hours", hours),
                ("weeks", weeks)):
            if not isinstance(value, (int, float)):
                raise TypeError(
                    f"unsupported type for timedelta {name} component: {type(value).__name__}")

        d = s = us = 0

        days += weeks * 7
        seconds += minutes * 60 + hours * 3600
        microseconds += milliseconds * 1000

        if isinstance(days, float):
            dayfrac, days = _math.modf(days)
            daysecondsfrac, daysecondswhole = _math.modf(dayfrac * (24. * 3600.))
            assert daysecondswhole == int(daysecondswhole)
            s = int(daysecondswhole)
            assert days == int(days)
            d = int(days)
        else:
            daysecondsfrac = 0.0
            d = days
        assert isinstance(daysecondsfrac, float)
        assert abs(daysecondsfrac) <= 1.0
        assert isinstance(d, int)
        assert abs(s) <= 24 * 3600

        if isinstance(seconds, float):
            secondsfrac, seconds = _math.modf(seconds)
            assert seconds == int(seconds)
            seconds = int(seconds)
            secondsfrac += daysecondsfrac
            assert abs(secondsfrac) <= 2.0
        else:
            secondsfrac = daysecondsfrac
        assert isinstance(secondsfrac, float)
        assert abs(secondsfrac) <= 2.0

        assert isinstance(seconds, int)
        days, seconds = divmod(seconds, 24 * 3600)
        d += days
        s += int(seconds)
        assert isinstance(s, int)
        assert abs(s) <= 2 * 24 * 3600

        usdouble = secondsfrac * 1e6
        assert abs(usdouble) < 2.1e6
        if isinstance(microseconds, float):
            microseconds = round(microseconds + usdouble)
            seconds, microseconds = divmod(microseconds, 1000000)
            seconds += s
            days, s = divmod(seconds, 24 * 3600)
            d += days
        else:
            microseconds = int(microseconds)
            seconds, microseconds = divmod(microseconds, 1000000)
            seconds += s
            days, s = divmod(seconds, 24 * 3600)
            d += days
            microseconds = round(microseconds + usdouble)
        assert isinstance(s, int)
        assert isinstance(microseconds, int)
        assert abs(s) <= 3 * 24 * 3600
        assert abs(microseconds) < 3.1e6

        seconds, us = divmod(microseconds, 1000000)
        s += seconds
        days, s = divmod(s, 24 * 3600)
        d += days

        assert isinstance(d, int)
        assert isinstance(s, int) and 0 <= s < 24 * 3600
        assert isinstance(us, int) and 0 <= us < 1000000

        if abs(d) > 999999999:
            raise OverflowError("days=%d; must have magnitude <= 999999999" % d)

        self = object.__new__(cls)
        self._days = d
        self._seconds = s
        self._microseconds = us
        self._hashcode = -1
        return self

    def __repr__(self):
        args = []
        if self._days:
            args.append("days=%d" % self._days)
        if self._seconds:
            args.append("seconds=%d" % self._seconds)
        if self._microseconds:
            args.append("microseconds=%d" % self._microseconds)
        if not args:
            args.append('0')
        return "%s.%s(%s)" % (_module_name(self), self.__class__.__qualname__,
                              ', '.join(args))

    def __str__(self):
        mm, ss = divmod(self._seconds, 60)
        hh, mm = divmod(mm, 60)
        s = "%d:%02d:%02d" % (hh, mm, ss)
        if self._days:
            def plural(n):
                return n, abs(n) != 1 and "s" or ""
            s = ("%d day%s, " % plural(self._days)) + s
        if self._microseconds:
            s = s + ".%06d" % self._microseconds
        return s

    def total_seconds(self):
        return ((self.days * 86400 + self.seconds) * 10**6 +
                self.microseconds) / 10**6

    @property
    def days(self):
        return self._days

    @property
    def seconds(self):
        return self._seconds

    @property
    def microseconds(self):
        return self._microseconds

    def __add__(self, other):
        if isinstance(other, timedelta):
            return timedelta(self._days + other._days,
                             self._seconds + other._seconds,
                             self._microseconds + other._microseconds)
        return NotImplemented

    __radd__ = __add__

    def __sub__(self, other):
        if isinstance(other, timedelta):
            return timedelta(self._days - other._days,
                             self._seconds - other._seconds,
                             self._microseconds - other._microseconds)
        return NotImplemented

    def __rsub__(self, other):
        if isinstance(other, timedelta):
            return -self + other
        return NotImplemented

    def __neg__(self):
        return timedelta(-self._days,
                         -self._seconds,
                         -self._microseconds)

    def __pos__(self):
        return self

    def __abs__(self):
        if self._days < 0:
            return -self
        else:
            return self

    def __mul__(self, other):
        if isinstance(other, int):
            return timedelta(self._days * other,
                             self._seconds * other,
                             self._microseconds * other)
        if isinstance(other, float):
            usec = self._to_microseconds()
            a, b = other.as_integer_ratio()
            return timedelta(0, 0, _divide_and_round(usec * a, b))
        return NotImplemented

    __rmul__ = __mul__

    def _to_microseconds(self):
        return ((self._days * (24 * 3600) + self._seconds) * 1000000 +
                self._microseconds)

    def __floordiv__(self, other):
        if not isinstance(other, (int, timedelta)):
            return NotImplemented
        usec = self._to_microseconds()
        if isinstance(other, timedelta):
            return usec // other._to_microseconds()
        if isinstance(other, int):
            return timedelta(0, 0, usec // other)

    def __truediv__(self, other):
        if not isinstance(other, (int, float, timedelta)):
            return NotImplemented
        usec = self._to_microseconds()
        if isinstance(other, timedelta):
            return usec / other._to_microseconds()
        if isinstance(other, int):
            return timedelta(0, 0, _divide_and_round(usec, other))
        if isinstance(other, float):
            a, b = other.as_integer_ratio()
            return timedelta(0, 0, _divide_and_round(b * usec, a))

    def __mod__(self, other):
        if isinstance(other, timedelta):
            r = self._to_microseconds() % other._to_microseconds()
            return timedelta(0, 0, r)
        return NotImplemented

    def __divmod__(self, other):
        if isinstance(other, timedelta):
            q, r = divmod(self._to_microseconds(),
                          other._to_microseconds())
            return q, timedelta(0, 0, r)
        return NotImplemented

    def __eq__(self, other):
        if isinstance(other, timedelta):
            return self._cmp(other) == 0
        else:
            return NotImplemented

    def __le__(self, other):
        if isinstance(other, timedelta):
            return self._cmp(other) <= 0
        else:
            return NotImplemented

    def __lt__(self, other):
        if isinstance(other, timedelta):
            return self._cmp(other) < 0
        else:
            return NotImplemented

    def __ge__(self, other):
        if isinstance(other, timedelta):
            return self._cmp(other) >= 0
        else:
            return NotImplemented

    def __gt__(self, other):
        if isinstance(other, timedelta):
            return self._cmp(other) > 0
        else:
            return NotImplemented

    def _cmp(self, other):
        assert isinstance(other, timedelta)
        return _cmp(self._getstate(), other._getstate())

    def __hash__(self):
        if self._hashcode == -1:
            self._hashcode = hash(self._getstate())
        return self._hashcode

    def __bool__(self):
        return (self._days != 0 or
                self._seconds != 0 or
                self._microseconds != 0)

    def _getstate(self):
        return (self._days, self._seconds, self._microseconds)

    def __reduce__(self):
        return (self.__class__, self._getstate())


timedelta.min = timedelta(-999999999)
timedelta.max = timedelta(days=999999999, hours=23, minutes=59, seconds=59,
                          microseconds=999999)
timedelta.resolution = timedelta(microseconds=1)


def _module_name(obj):
    return 'datetime'


class date:
    """Concrete date type."""
    __nova_type_name__ = 'datetime.date'

    __slots__ = '_year', '_month', '_day', '_hashcode'

    def __new__(cls, year, month=None, day=None):
        year, month, day = _check_date_fields(year, month, day)
        self = object.__new__(cls)
        self._year = year
        self._month = month
        self._day = day
        self._hashcode = -1
        return self

    @classmethod
    def fromtimestamp(cls, t):
        y, m, d, hh, mm, ss, weekday, jday, dst = _time.gmtime(t)
        return cls(y, m, d)

    @classmethod
    def today(cls):
        t = _time.time()
        return cls.fromtimestamp(t)

    @classmethod
    def fromordinal(cls, n):
        y, m, d = _ord2ymd(n)
        return cls(y, m, d)

    @classmethod
    def fromisoformat(cls, date_string):
        if not isinstance(date_string, str):
            raise TypeError('fromisoformat: argument must be str')
        if len(date_string) not in (7, 8, 10):
            raise ValueError(f'Invalid isoformat string: {date_string!r}')
        try:
            return cls(*_parse_isoformat_date(date_string))
        except Exception:
            raise ValueError(f'Invalid isoformat string: {date_string!r}')

    @classmethod
    def fromisocalendar(cls, year, week, day):
        return cls(*_isoweek_to_gregorian(year, week, day))

    def __repr__(self):
        return "%s.%s(%d, %d, %d)" % (_module_name(self),
                                      self.__class__.__qualname__,
                                      self._year, self._month, self._day)

    def ctime(self):
        weekday = self.toordinal() % 7 or 7
        return "%s %s %2d 00:00:00 %04d" % (
            _DAYNAMES[weekday],
            _MONTHNAMES[self._month],
            self._day, self._year)

    def strftime(self, format):
        return _wrap_strftime(self, format, self.timetuple())

    def __format__(self, fmt):
        if not isinstance(fmt, str):
            raise TypeError("must be str, not %s" % type(fmt).__name__)
        if len(fmt) != 0:
            return self.strftime(fmt)
        return str(self)

    def isoformat(self):
        return "%04d-%02d-%02d" % (self._year, self._month, self._day)

    __str__ = isoformat

    @property
    def year(self):
        return self._year

    @property
    def month(self):
        return self._month

    @property
    def day(self):
        return self._day

    def timetuple(self):
        return _build_struct_time(self._year, self._month, self._day,
                                  0, 0, 0, -1)

    def toordinal(self):
        return _ymd2ord(self._year, self._month, self._day)

    def replace(self, year=None, month=None, day=None):
        if year is None:
            year = self._year
        if month is None:
            month = self._month
        if day is None:
            day = self._day
        return type(self)(year, month, day)

    __replace__ = replace

    def __eq__(self, other):
        if isinstance(other, date) and not isinstance(other, datetime):
            return self._cmp(other) == 0
        return NotImplemented

    def __le__(self, other):
        if isinstance(other, date) and not isinstance(other, datetime):
            return self._cmp(other) <= 0
        return NotImplemented

    def __lt__(self, other):
        if isinstance(other, date) and not isinstance(other, datetime):
            return self._cmp(other) < 0
        return NotImplemented

    def __ge__(self, other):
        if isinstance(other, date) and not isinstance(other, datetime):
            return self._cmp(other) >= 0
        return NotImplemented

    def __gt__(self, other):
        if isinstance(other, date) and not isinstance(other, datetime):
            return self._cmp(other) > 0
        return NotImplemented

    def _cmp(self, other):
        assert isinstance(other, date)
        assert not isinstance(other, datetime)
        y, m, d = self._year, self._month, self._day
        y2, m2, d2 = other._year, other._month, other._day
        return _cmp((y, m, d), (y2, m2, d2))

    def __hash__(self):
        if self._hashcode == -1:
            self._hashcode = hash(self._getstate())
        return self._hashcode

    def __add__(self, other):
        if isinstance(other, timedelta):
            o = self.toordinal() + other.days
            if 0 < o <= _MAXORDINAL:
                return type(self).fromordinal(o)
            raise OverflowError("result out of range")
        return NotImplemented

    __radd__ = __add__

    def __sub__(self, other):
        if isinstance(other, timedelta):
            return self + timedelta(-other.days)
        if isinstance(other, date):
            days1 = self.toordinal()
            days2 = other.toordinal()
            return timedelta(days1 - days2)
        return NotImplemented

    def weekday(self):
        return (self.toordinal() + 6) % 7

    def isoweekday(self):
        return self.toordinal() % 7 or 7

    def isocalendar(self):
        return _IsoCalendarDate(*_isocalendar(self._year, self._month, self._day))

    def _getstate(self):
        return (self._year, self._month, self._day)

    def __reduce__(self):
        return (self.__class__, self._getstate())


date.min = date(1, 1, 1)
date.max = date(9999, 12, 31)
date.resolution = timedelta(days=1)


class _IsoCalendarDate(tuple):

    def __new__(cls, year, week, weekday, /):
        return super().__new__(cls, (year, week, weekday))

    @property
    def year(self):
        return self[0]

    @property
    def week(self):
        return self[1]

    @property
    def weekday(self):
        return self[2]

    def __reduce__(self):
        return (tuple, (tuple(self),))

    def __repr__(self):
        return ('datetime.IsoCalendarDate'
                f'(year={self[0]}, week={self[1]}, weekday={self[2]})')


_IsoCalendarDate.__name__ = 'IsoCalendarDate'


class tzinfo:
    """Abstract base class for time zone info classes."""
    __nova_type_name__ = 'datetime.tzinfo'
    __slots__ = ()

    def tzname(self, dt):
        raise NotImplementedError("tzinfo subclass must override tzname()")

    def utcoffset(self, dt):
        raise NotImplementedError("tzinfo subclass must override utcoffset()")

    def dst(self, dt):
        raise NotImplementedError("tzinfo subclass must override dst()")

    def fromutc(self, dt):
        if not isinstance(dt, datetime):
            raise TypeError("fromutc() requires a datetime argument")
        if dt.tzinfo is not self:
            raise ValueError("dt.tzinfo is not self")
        dtoff = dt.utcoffset()
        if dtoff is None:
            raise ValueError("fromutc() requires a non-None utcoffset() "
                             "result")
        dtdst = dt.dst()
        if dtdst is None:
            raise ValueError("fromutc() requires a non-None dst() result")
        delta = dtoff - dtdst
        if delta:
            dt += delta
            dtdst = dt.dst()
            if dtdst is None:
                raise ValueError("fromutc(): dt.dst gave inconsistent "
                                 "results; cannot convert")
        return dt + dtdst


class time:
    """Time with time zone."""
    __nova_type_name__ = 'datetime.time'

    __slots__ = '_hour', '_minute', '_second', '_microsecond', '_tzinfo', '_hashcode', '_fold'

    def __new__(cls, hour=0, minute=0, second=0, microsecond=0, tzinfo=None, *, fold=0):
        hour, minute, second, microsecond, fold = _check_time_fields(
            hour, minute, second, microsecond, fold)
        _check_tzinfo_arg(tzinfo)
        self = object.__new__(cls)
        self._hour = hour
        self._minute = minute
        self._second = second
        self._microsecond = microsecond
        self._tzinfo = tzinfo
        self._hashcode = -1
        self._fold = fold
        return self

    @property
    def hour(self):
        return self._hour

    @property
    def minute(self):
        return self._minute

    @property
    def second(self):
        return self._second

    @property
    def microsecond(self):
        return self._microsecond

    @property
    def tzinfo(self):
        return self._tzinfo

    @property
    def fold(self):
        return self._fold

    def __eq__(self, other):
        if isinstance(other, time):
            return self._cmp(other, allow_mixed=True) == 0
        else:
            return NotImplemented

    def __le__(self, other):
        if isinstance(other, time):
            return self._cmp(other) <= 0
        else:
            return NotImplemented

    def __lt__(self, other):
        if isinstance(other, time):
            return self._cmp(other) < 0
        else:
            return NotImplemented

    def __ge__(self, other):
        if isinstance(other, time):
            return self._cmp(other) >= 0
        else:
            return NotImplemented

    def __gt__(self, other):
        if isinstance(other, time):
            return self._cmp(other) > 0
        else:
            return NotImplemented

    def _cmp(self, other, allow_mixed=False):
        assert isinstance(other, time)
        mytz = self._tzinfo
        ottz = other._tzinfo
        myoff = otoff = None

        if mytz is ottz:
            base_compare = True
        else:
            myoff = self.utcoffset()
            otoff = other.utcoffset()
            base_compare = myoff == otoff

        if base_compare:
            return _cmp((self._hour, self._minute, self._second,
                         self._microsecond),
                        (other._hour, other._minute, other._second,
                         other._microsecond))
        if myoff is None or otoff is None:
            if allow_mixed:
                return 2
            else:
                raise TypeError("cannot compare naive and aware times")
        myhhmm = self._hour * 60 + self._minute - myoff // timedelta(minutes=1)
        othhmm = other._hour * 60 + other._minute - otoff // timedelta(minutes=1)
        return _cmp((myhhmm, self._second, self._microsecond),
                    (othhmm, other._second, other._microsecond))

    def __hash__(self):
        if self._hashcode == -1:
            if self.fold:
                t = self.replace(fold=0)
            else:
                t = self
            tzoff = t.utcoffset()
            if not tzoff:
                self._hashcode = hash(t._getstate()[:4])
            else:
                h, m = divmod(timedelta(hours=self.hour, minutes=self.minute) - tzoff,
                              timedelta(hours=1))
                assert not m % timedelta(minutes=1), "whole minute"
                m //= timedelta(minutes=1)
                if 0 <= h < 24:
                    self._hashcode = hash(time(h, m, self.second, self.microsecond))
                else:
                    self._hashcode = hash((h, m, self.second, self.microsecond))
        return self._hashcode

    def _tzstr(self):
        off = self.utcoffset()
        return _format_offset(off)

    def __repr__(self):
        if self._microsecond != 0:
            s = ", %d, %d" % (self._second, self._microsecond)
        elif self._second != 0:
            s = ", %d" % self._second
        else:
            s = ""
        s = "%s.%s(%d, %d%s)" % (_module_name(self),
                                 self.__class__.__qualname__,
                                 self._hour, self._minute, s)
        if self._tzinfo is not None:
            assert s[-1:] == ")"
            s = s[:-1] + ", tzinfo=%r" % self._tzinfo + ")"
        if self._fold:
            assert s[-1:] == ")"
            s = s[:-1] + ", fold=1)"
        return s

    def isoformat(self, timespec='auto'):
        s = _format_time(self._hour, self._minute, self._second,
                         self._microsecond, timespec)
        tz = self._tzstr()
        if tz:
            s += tz
        return s

    __str__ = isoformat

    @classmethod
    def fromisoformat(cls, time_string):
        if not isinstance(time_string, str):
            raise TypeError('fromisoformat: argument must be str')
        time_string = time_string.removeprefix('T')
        try:
            return cls(*_parse_isoformat_time(time_string))
        except Exception:
            raise ValueError(f'Invalid isoformat string: {time_string!r}')

    def strftime(self, format):
        timetuple = (1900, 1, 1,
                     self._hour, self._minute, self._second,
                     0, 1, -1)
        return _wrap_strftime(self, format, timetuple)

    def __format__(self, fmt):
        if not isinstance(fmt, str):
            raise TypeError("must be str, not %s" % type(fmt).__name__)
        if len(fmt) != 0:
            return self.strftime(fmt)
        return str(self)

    def utcoffset(self):
        if self._tzinfo is None:
            return None
        offset = self._tzinfo.utcoffset(None)
        _check_utc_offset("utcoffset", offset)
        return offset

    def tzname(self):
        if self._tzinfo is None:
            return None
        name = self._tzinfo.tzname(None)
        _check_tzname(name)
        return name

    def dst(self):
        if self._tzinfo is None:
            return None
        offset = self._tzinfo.dst(None)
        _check_utc_offset("dst", offset)
        return offset

    def replace(self, hour=None, minute=None, second=None, microsecond=None,
                tzinfo=True, *, fold=None):
        if hour is None:
            hour = self.hour
        if minute is None:
            minute = self.minute
        if second is None:
            second = self.second
        if microsecond is None:
            microsecond = self.microsecond
        if tzinfo is True:
            tzinfo = self.tzinfo
        if fold is None:
            fold = self._fold
        return type(self)(hour, minute, second, microsecond, tzinfo, fold=fold)

    __replace__ = replace

    def _getstate(self):
        return (self._hour, self._minute, self._second, self._microsecond)

    def __reduce__(self):
        return (self.__class__, self._getstate())


time.min = time(0, 0, 0)
time.max = time(23, 59, 59, 999999)
time.resolution = timedelta(microseconds=1)


class datetime(date):
    """datetime(year, month, day[, hour[, minute[, second[, microsecond[,tzinfo]]]]])"""
    __nova_type_name__ = 'datetime.datetime'
    __slots__ = time.__slots__

    def __new__(cls, year, month=None, day=None, hour=0, minute=0, second=0,
                microsecond=0, tzinfo=None, *, fold=0):
        year, month, day = _check_date_fields(year, month, day)
        hour, minute, second, microsecond, fold = _check_time_fields(
            hour, minute, second, microsecond, fold)
        _check_tzinfo_arg(tzinfo)
        self = object.__new__(cls)
        self._year = year
        self._month = month
        self._day = day
        self._hour = hour
        self._minute = minute
        self._second = second
        self._microsecond = microsecond
        self._tzinfo = tzinfo
        self._hashcode = -1
        self._fold = fold
        return self

    @property
    def hour(self):
        return self._hour

    @property
    def minute(self):
        return self._minute

    @property
    def second(self):
        return self._second

    @property
    def microsecond(self):
        return self._microsecond

    @property
    def tzinfo(self):
        return self._tzinfo

    @property
    def fold(self):
        return self._fold

    @classmethod
    def _fromtimestamp(cls, t, utc, tz):
        frac, t = _math.modf(t)
        us = round(frac * 1e6)
        if us >= 1000000:
            t += 1
            us -= 1000000
        elif us < 0:
            t -= 1
            us += 1000000

        y, m, d, hh, mm, ss, weekday, jday, dst = _time.gmtime(t)
        ss = min(ss, 59)
        result = cls(y, m, d, hh, mm, ss, us, tz)
        return result

    @classmethod
    def fromtimestamp(cls, timestamp, tz=None):
        _check_tzinfo_arg(tz)
        result = cls._fromtimestamp(timestamp, tz is not None, None if tz is None else timezone.utc)
        if tz is not None:
            result = result.replace(tzinfo=None)
            result = tz.fromutc(result.replace(tzinfo=tz))
        return result

    @classmethod
    def utcfromtimestamp(cls, t):
        return cls._fromtimestamp(t, True, None)

    @classmethod
    def now(cls, tz=None):
        t = _time.time()
        return cls.fromtimestamp(t, tz)

    @classmethod
    def utcnow(cls):
        t = _time.time()
        return cls.utcfromtimestamp(t)

    @classmethod
    def combine(cls, date, time, tzinfo=True):
        if not isinstance(date, _date_class):
            raise TypeError("date argument must be a date instance")
        if not isinstance(time, _time_class):
            raise TypeError("time argument must be a time instance")
        if tzinfo is True:
            tzinfo = time.tzinfo
        return cls(date.year, date.month, date.day,
                   time.hour, time.minute, time.second, time.microsecond,
                   tzinfo, fold=time.fold)

    @classmethod
    def fromisoformat(cls, date_string):
        if not isinstance(date_string, str):
            raise TypeError('fromisoformat: argument must be str')

        if len(date_string) < 7:
            raise ValueError(f'Invalid isoformat string: {date_string!r}')

        try:
            separator_location = _find_isoformat_datetime_separator(date_string)
            dstr = date_string[0:separator_location]
            tstr = date_string[(separator_location + 1):]

            date_components = _parse_isoformat_date(dstr)
        except ValueError:
            raise ValueError(
                f'Invalid isoformat string: {date_string!r}') from None

        if tstr:
            try:
                time_components = _parse_isoformat_time(tstr)
            except ValueError:
                raise ValueError(
                    f'Invalid isoformat string: {date_string!r}') from None
        else:
            time_components = [0, 0, 0, 0, None]

        return cls(*(date_components + time_components))

    def timetuple(self):
        dst = self.dst()
        if dst is None:
            dst = -1
        elif dst:
            dst = 1
        else:
            dst = 0
        return _build_struct_time(self.year, self.month, self.day,
                                  self.hour, self.minute, self.second,
                                  dst)

    def timestamp(self):
        if self._tzinfo is None:
            s = (self - _EPOCH_NAIVE).total_seconds()
            return s
        else:
            return (self - _EPOCH).total_seconds()

    def utctimetuple(self):
        offset = self.utcoffset()
        if offset:
            self -= offset
        y, m, d = self.year, self.month, self.day
        hh, mm, ss = self.hour, self.minute, self.second
        return _build_struct_time(y, m, d, hh, mm, ss, 0)

    def date(self):
        return date(self._year, self._month, self._day)

    def time(self):
        return time(self.hour, self.minute, self.second, self.microsecond, fold=self.fold)

    def timetz(self):
        return time(self.hour, self.minute, self.second, self.microsecond,
                    self._tzinfo, fold=self.fold)

    def replace(self, year=None, month=None, day=None, hour=None,
                minute=None, second=None, microsecond=None, tzinfo=True,
                *, fold=None):
        if year is None:
            year = self.year
        if month is None:
            month = self.month
        if day is None:
            day = self.day
        if hour is None:
            hour = self.hour
        if minute is None:
            minute = self.minute
        if second is None:
            second = self.second
        if microsecond is None:
            microsecond = self.microsecond
        if tzinfo is True:
            tzinfo = self.tzinfo
        if fold is None:
            fold = self.fold
        return type(self)(year, month, day, hour, minute, second,
                          microsecond, tzinfo, fold=fold)

    __replace__ = replace

    def astimezone(self, tz=None):
        if tz is None:
            tz = timezone.utc
        elif not isinstance(tz, tzinfo):
            raise TypeError("tz argument must be an instance of tzinfo")

        mytz = self.tzinfo
        if mytz is None:
            mytz = timezone.utc
            myoffset = timedelta(0)
        else:
            myoffset = mytz.utcoffset(self)
            if myoffset is None:
                mytz = timezone.utc
                myoffset = timedelta(0)

        if tz is mytz:
            return self

        utc = (self - myoffset).replace(tzinfo=tz)
        return tz.fromutc(utc)

    def ctime(self):
        weekday = self.toordinal() % 7 or 7
        return "%s %s %2d %02d:%02d:%02d %04d" % (
            _DAYNAMES[weekday],
            _MONTHNAMES[self._month],
            self._day,
            self._hour, self._minute, self._second,
            self._year)

    def isoformat(self, sep='T', timespec='auto'):
        s = ("%04d-%02d-%02d%c" % (self._year, self._month, self._day, sep) +
             _format_time(self._hour, self._minute, self._second,
                          self._microsecond, timespec))

        off = self.utcoffset()
        tz = _format_offset(off)
        if tz:
            s += tz

        return s

    def __repr__(self):
        L = [self._year, self._month, self._day,
             self._hour, self._minute, self._second, self._microsecond]
        if L[-1] == 0:
            del L[-1]
        if L[-1] == 0:
            del L[-1]
        s = "%s.%s(%s)" % (_module_name(self),
                           self.__class__.__qualname__,
                           ", ".join(map(str, L)))
        if self._tzinfo is not None:
            assert s[-1:] == ")"
            s = s[:-1] + ", tzinfo=%r" % self._tzinfo + ")"
        if self._fold:
            assert s[-1:] == ")"
            s = s[:-1] + ", fold=1)"
        return s

    def __str__(self):
        return self.isoformat(sep=' ')

    @classmethod
    def strptime(cls, date_string, format):
        return _strptime_datetime(cls, date_string, format)

    def utcoffset(self):
        if self._tzinfo is None:
            return None
        offset = self._tzinfo.utcoffset(self)
        _check_utc_offset("utcoffset", offset)
        return offset

    def tzname(self):
        if self._tzinfo is None:
            return None
        name = self._tzinfo.tzname(self)
        _check_tzname(name)
        return name

    def dst(self):
        if self._tzinfo is None:
            return None
        offset = self._tzinfo.dst(self)
        _check_utc_offset("dst", offset)
        return offset

    def __eq__(self, other):
        if isinstance(other, datetime):
            return self._cmp(other, allow_mixed=True) == 0
        elif not isinstance(other, date):
            return NotImplemented
        else:
            return False

    def __le__(self, other):
        if isinstance(other, datetime):
            return self._cmp(other) <= 0
        elif not isinstance(other, date):
            return NotImplemented
        else:
            _cmperror(self, other)

    def __lt__(self, other):
        if isinstance(other, datetime):
            return self._cmp(other) < 0
        elif not isinstance(other, date):
            return NotImplemented
        else:
            _cmperror(self, other)

    def __ge__(self, other):
        if isinstance(other, datetime):
            return self._cmp(other) >= 0
        elif not isinstance(other, date):
            return NotImplemented
        else:
            _cmperror(self, other)

    def __gt__(self, other):
        if isinstance(other, datetime):
            return self._cmp(other) > 0
        elif not isinstance(other, date):
            return NotImplemented
        else:
            _cmperror(self, other)

    def _cmp(self, other, allow_mixed=False):
        assert isinstance(other, datetime)
        mytz = self._tzinfo
        ottz = other._tzinfo
        myoff = otoff = None

        if mytz is ottz:
            base_compare = True
        else:
            myoff = self.utcoffset()
            otoff = other.utcoffset()
            if allow_mixed:
                if myoff != self.replace(fold=not self.fold).utcoffset():
                    return 2
                if otoff != other.replace(fold=not other.fold).utcoffset():
                    return 2
            base_compare = myoff == otoff

        if base_compare:
            return _cmp((self._year, self._month, self._day,
                         self._hour, self._minute, self._second,
                         self._microsecond),
                        (other._year, other._month, other._day,
                         other._hour, other._minute, other._second,
                         other._microsecond))
        if myoff is None or otoff is None:
            if allow_mixed:
                return 2
            else:
                raise TypeError("can't compare offset-naive and offset-aware datetimes")
        diff = self - other
        if diff.days < 0:
            return -1
        return diff and 1 or 0

    def __add__(self, other):
        if not isinstance(other, timedelta):
            return NotImplemented
        delta = timedelta(self.toordinal(),
                          hours=self._hour,
                          minutes=self._minute,
                          seconds=self._second,
                          microseconds=self._microsecond)
        delta += other
        hour, rem = divmod(delta.seconds, 3600)
        minute, second = divmod(rem, 60)
        if 0 < delta.days <= _MAXORDINAL:
            return type(self).combine(date.fromordinal(delta.days),
                                      time(hour, minute, second,
                                           delta.microseconds,
                                           tzinfo=self._tzinfo))
        raise OverflowError("result out of range")

    __radd__ = __add__

    def __sub__(self, other):
        if not isinstance(other, datetime):
            if isinstance(other, timedelta):
                return self + -other
            return NotImplemented

        days1 = self.toordinal()
        days2 = other.toordinal()
        secs1 = self._second + self._minute * 60 + self._hour * 3600
        secs2 = other._second + other._minute * 60 + other._hour * 3600
        base = timedelta(days1 - days2,
                         secs1 - secs2,
                         self._microsecond - other._microsecond)
        if self._tzinfo is other._tzinfo:
            return base
        myoff = self.utcoffset()
        otoff = other.utcoffset()
        if myoff == otoff:
            return base
        if myoff is None or otoff is None:
            raise TypeError("can't subtract offset-naive and offset-aware datetimes")
        return base + otoff - myoff

    def __hash__(self):
        if self._hashcode == -1:
            if self.fold:
                t = self.replace(fold=0)
            else:
                t = self
            tzoff = t.utcoffset()
            if tzoff is None:
                self._hashcode = hash(t._getstate())
            else:
                days = _ymd2ord(self.year, self.month, self.day)
                seconds = self.hour * 3600 + self.minute * 60 + self.second
                self._hashcode = hash(timedelta(days, seconds, self.microsecond) - tzoff)
        return self._hashcode

    def _getstate(self):
        return (self._year, self._month, self._day, self._hour, self._minute,
                self._second, self._microsecond)

    def __reduce__(self):
        return (self.__class__, self._getstate())


datetime.min = datetime(1, 1, 1)
datetime.max = datetime(9999, 12, 31, 23, 59, 59, 999999)
datetime.resolution = timedelta(microseconds=1)

_date_class = date
_time_class = time


def _cmperror(x, y):
    raise TypeError("can't compare '%s' to '%s'" % (
                    type(x).__name__, type(y).__name__))


class timezone(tzinfo):
    __nova_type_name__ = 'datetime.timezone'
    __slots__ = '_offset', '_name'

    _Omitted = object()

    def __new__(cls, offset, name=_Omitted):
        if not isinstance(offset, timedelta):
            raise TypeError("offset must be a timedelta")
        if name is cls._Omitted:
            if not offset:
                return cls.utc
            name = None
        elif not isinstance(name, str):
            raise TypeError("name must be a string")
        if not cls._minoffset <= offset <= cls._maxoffset:
            raise ValueError("offset must be a timedelta "
                             "strictly between -timedelta(hours=24) and "
                             "timedelta(hours=24).")
        return cls._create(offset, name)

    @classmethod
    def _create(cls, offset, name=None):
        self = tzinfo.__new__(cls)
        self._offset = offset
        self._name = name
        return self

    def __eq__(self, other):
        if isinstance(other, timezone):
            return self._offset == other._offset
        return NotImplemented

    def __hash__(self):
        return hash(self._offset)

    def __repr__(self):
        if self is self.utc:
            return 'datetime.timezone.utc'
        if self._name is None:
            return "%s.%s(%r)" % (_module_name(self),
                                  self.__class__.__qualname__,
                                  self._offset)
        return "%s.%s(%r, %r)" % (_module_name(self),
                                  self.__class__.__qualname__,
                                  self._offset, self._name)

    def __str__(self):
        return self.tzname(None)

    def utcoffset(self, dt):
        if isinstance(dt, datetime) or dt is None:
            return self._offset
        raise TypeError("utcoffset() argument must be a datetime instance"
                        " or None")

    def tzname(self, dt):
        if isinstance(dt, datetime) or dt is None:
            if self._name is None:
                return self._name_from_offset(self._offset)
            return self._name
        raise TypeError("tzname() argument must be a datetime instance"
                        " or None")

    def dst(self, dt):
        if isinstance(dt, datetime) or dt is None:
            return None
        raise TypeError("dst() argument must be a datetime instance"
                        " or None")

    def fromutc(self, dt):
        if isinstance(dt, datetime):
            if dt.tzinfo is not self:
                raise ValueError("fromutc: dt.tzinfo "
                                 "is not self")
            return dt + self._offset
        raise TypeError("fromutc() argument must be a datetime instance"
                        " or None")

    _maxoffset = timedelta(hours=24, microseconds=-1)
    _minoffset = -_maxoffset

    @staticmethod
    def _name_from_offset(delta):
        if not delta:
            return 'UTC'
        if delta < timedelta(0):
            sign = '-'
            delta = -delta
        else:
            sign = '+'
        hours, rest = divmod(delta, timedelta(hours=1))
        minutes, rest = divmod(rest, timedelta(minutes=1))
        seconds = rest.seconds
        microseconds = rest.microseconds
        if microseconds:
            return (f'UTC{sign}{hours:02d}:{minutes:02d}:{seconds:02d}'
                    f'.{microseconds:06d}')
        if seconds:
            return f'UTC{sign}{hours:02d}:{minutes:02d}:{seconds:02d}'
        return f'UTC{sign}{hours:02d}:{minutes:02d}'


UTC = timezone.utc = timezone._create(timedelta(0))
timezone.min = timezone._create(-timedelta(hours=23, minutes=59))
timezone.max = timezone._create(timedelta(hours=23, minutes=59))
_EPOCH = datetime(1970, 1, 1, tzinfo=timezone.utc)
_EPOCH_NAIVE = datetime(1970, 1, 1)


# ISO 8601 parsing, as CPython's _pydatetime.

def _find_isoformat_datetime_separator(dtstr):
    len_dtstr = len(dtstr)
    if len_dtstr == 7:
        return 7

    assert len_dtstr > 7
    date_separator = "-"
    week_indicator = "W"

    if dtstr[4] == date_separator:
        if dtstr[5] == week_indicator:
            if len_dtstr < 8:
                raise ValueError("Invalid ISO string")
            if len_dtstr > 8 and dtstr[8] == date_separator:
                if len_dtstr == 9:
                    raise ValueError("Invalid ISO string")
                if len_dtstr > 10 and _is_ascii_digit(dtstr[10]):
                    return 8
                return 10
            else:
                return 8
        else:
            return 10
    else:
        if dtstr[4] == week_indicator:
            idx = 7
            while idx < len_dtstr:
                if not _is_ascii_digit(dtstr[idx]):
                    break
                idx += 1

            if idx < 9:
                return idx

            if idx % 2 == 0:
                return 7
            else:
                return 8
        else:
            return 8


def _is_ascii_digit(c):
    return c in "0123456789"


def _parse_isoformat_date(dtstr):
    assert len(dtstr) in (7, 8, 10)
    year = int(dtstr[0:4])
    has_sep = dtstr[4] == '-'

    pos = 4 + has_sep
    if dtstr[pos:pos + 1] == "W":
        pos += 1
        weekno = int(dtstr[pos:pos + 2])
        pos += 2

        dayno = 1
        if len(dtstr) > pos:
            if (dtstr[pos:pos + 1] == '-') != has_sep:
                raise ValueError("Inconsistent use of dash separator")

            pos += has_sep

            dayno = int(dtstr[pos:pos + 1])

        return list(_isoweek_to_gregorian(year, weekno, dayno))
    else:
        month = int(dtstr[pos:pos + 2])
        pos += 2
        if (dtstr[pos:pos + 1] == "-") != has_sep:
            raise ValueError("Inconsistent use of dash separator")

        pos += has_sep
        day = int(dtstr[pos:pos + 2])

        return [year, month, day]


_FRACTION_CORRECTION = [100000, 10000, 1000, 100, 10]


def _parse_hh_mm_ss_ff(tstr):
    len_str = len(tstr)

    time_comps = [0, 0, 0, 0]
    pos = 0
    for comp in range(0, 3):
        if (len_str - pos) < 2:
            raise ValueError("Incomplete time component")

        time_comps[comp] = int(tstr[pos:pos + 2])

        pos += 2
        next_char = tstr[pos:pos + 1]

        if comp == 0:
            has_sep = next_char == ':'

        if not next_char or comp >= 2:
            break

        if has_sep and next_char != ':':
            raise ValueError("Invalid time separator: %c" % next_char)

        pos += has_sep

    if pos < len_str:
        if tstr[pos] not in '.,':
            raise ValueError("Invalid microsecond component")
        else:
            pos += 1

            len_remainder = len_str - pos

            if len_remainder >= 6:
                to_parse = 6
            else:
                to_parse = len_remainder

            time_comps[3] = int(tstr[pos:(pos + to_parse)])
            if to_parse < 6:
                time_comps[3] *= _FRACTION_CORRECTION[to_parse - 1]
            if (len_remainder > to_parse
                    and not all(map(_is_ascii_digit, tstr[(pos + to_parse):]))):
                raise ValueError("Non-digit values in unparsed fraction")

    return time_comps


def _parse_isoformat_time(tstr):
    len_str = len(tstr)
    if len_str < 2:
        raise ValueError("Isoformat time too short")

    tz_pos = (tstr.find('-') + 1 or tstr.find('+') + 1 or tstr.find('Z') + 1)
    timestr = tstr[:tz_pos - 1] if tz_pos > 0 else tstr

    time_comps = _parse_hh_mm_ss_ff(timestr)

    tzi = None
    if tz_pos == len_str and tstr[-1] == 'Z':
        tzi = timezone.utc
    elif tz_pos > 0:
        tzstr = tstr[tz_pos:]

        if len(tzstr) in (0, 1, 3):
            raise ValueError("Malformed time zone string")

        tz_comps = _parse_hh_mm_ss_ff(tzstr)

        if all(x == 0 for x in tz_comps):
            tzi = timezone.utc
        else:
            tzsign = -1 if tstr[tz_pos - 1] == '-' else 1

            td = timedelta(hours=tz_comps[0], minutes=tz_comps[1],
                           seconds=tz_comps[2], microseconds=tz_comps[3])

            tzi = timezone(tzsign * td)

    time_comps.append(tzi)

    return time_comps


# strptime: the directives of C's strptime that CPython supports.

_STRPTIME_NUMERIC = {
    'd': (r'(3[01]|[12]\d|0[1-9]|[1-9]| [1-9])', 'day'),
    'm': (r'(1[0-2]|0[1-9]|[1-9])', 'month'),
    'Y': (r'(\d\d\d\d)', 'year'),
    'y': (r'(\d\d)', 'year2'),
    'H': (r'(2[0-3]|[0-1]\d|\d)', 'hour'),
    'I': (r'(1[0-2]|0[1-9]|[1-9])', 'hour12'),
    'M': (r'([0-5]\d|\d)', 'minute'),
    'S': (r'(6[0-1]|[0-5]\d|\d)', 'second'),
    'f': (r'(\d{1,6})', 'fraction'),
    'j': (r'(36[0-6]|3[0-5]\d|[12]\d\d|0[1-9]\d|00[1-9]|[1-9]\d|0[1-9]|[1-9])', 'julian'),
    'w': (r'([0-6])', 'weekday0'),
    'u': (r'([1-7])', 'weekday1'),
}


def _strptime_datetime(cls, data_string, format):
    import re
    pieces = []
    fields = []
    i = 0
    n = len(format)
    while i < n:
        ch = format[i]
        i += 1
        if ch == '%' and i < n:
            d = format[i]
            i += 1
            if d in _STRPTIME_NUMERIC:
                regex, name = _STRPTIME_NUMERIC[d]
                pieces.append(regex)
                fields.append(name)
            elif d in 'bB' or d == 'h':
                names = _FULLMONTHNAMES[1:] + _MONTHNAMES[1:]
                pieces.append('(' + '|'.join(names) + ')')
                fields.append('monthname')
            elif d in 'aA':
                names = _FULLDAYNAMES[1:] + _DAYNAMES[1:]
                pieces.append('(' + '|'.join(names) + ')')
                fields.append('dayname')
            elif d == 'p':
                pieces.append('(am|pm)')
                fields.append('ampm')
            elif d == 'z':
                pieces.append(r'(z|Z|[+-]\d\d:?[0-5]\d(?::?[0-5]\d(?:\.\d{1,6})?)?)?')
                fields.append('tz')
            elif d == 'Z':
                pieces.append(r'(utc|gmt|UTC|GMT)')
                fields.append('tzname')
            elif d == '%':
                pieces.append('%')
            elif d == 'T':
                format = format[:i] + 'H:%M:%S' + format[i:]
                format = format[:i - 2] + '%' + format[i:]
                n = len(format)
                i -= 2
            elif d == 'F':
                format = format[:i - 2] + '%Y-%m-%d' + format[i:]
                n = len(format)
                i -= 2
            else:
                raise ValueError("'%s' is a bad directive in format '%%%s'" % (d, d))
        elif ch.isspace():
            pieces.append(r'\s+')
        else:
            pieces.append(re.escape(ch))
    regex = re.compile('(?i)' + ''.join(pieces))
    m = regex.match(data_string)
    if not m:
        raise ValueError("time data %r does not match format %r" %
                         (data_string, format))
    if len(data_string) != m.end():
        raise ValueError("unconverted data remains: %s" %
                         data_string[m.end():])
    year = 1900
    month = day = 1
    hour = minute = second = fraction = 0
    tz = None
    ampm = None
    hour12 = None
    julian = None
    for name, value in zip(fields, m.groups()):
        if name == 'year':
            year = int(value)
        elif name == 'year2':
            year = int(value)
            year += 2000 if year <= 68 else 1900
        elif name == 'month':
            month = int(value)
        elif name == 'monthname':
            low = value.lower()
            for k in range(1, 13):
                if low in (_FULLMONTHNAMES[k].lower(), _MONTHNAMES[k].lower()):
                    month = k
        elif name == 'day':
            day = int(value)
        elif name == 'hour':
            hour = int(value)
        elif name == 'hour12':
            hour12 = int(value)
        elif name == 'ampm':
            ampm = value.lower()
        elif name == 'minute':
            minute = int(value)
        elif name == 'second':
            second = int(value)
        elif name == 'fraction':
            fraction = int(value + '0' * (6 - len(value)))
        elif name == 'julian':
            julian = int(value)
        elif name == 'tz' and value:
            if value in ('z', 'Z'):
                tz = timezone.utc
            else:
                sign = -1 if value[0] == '-' else 1
                body = value[1:].replace(':', '')
                hh = int(body[0:2])
                mm = int(body[2:4])
                ss = int(body[4:6]) if len(body) >= 6 else 0
                us = 0
                if '.' in body:
                    frac = body.split('.')[1]
                    us = int(frac + '0' * (6 - len(frac)))
                off = sign * timedelta(hours=hh, minutes=mm, seconds=ss, microseconds=us)
                tz = timezone(off)
        elif name == 'tzname':
            pass
    if hour12 is not None:
        hour = hour12 % 12
        if ampm == 'pm':
            hour += 12
    elif ampm is not None and hour is not None:
        pass
    if julian is not None:
        d = date.fromordinal(julian - 1 + date(year, 1, 1).toordinal())
        month, day = d.month, d.day
    return cls(year, month, day, hour, minute, second, fraction, tz)
