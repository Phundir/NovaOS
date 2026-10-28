from datetime import date, datetime, time, timedelta, timezone, UTC
import datetime as dtm
d = date(2024, 2, 29)
print(d, repr(d), d.year, d.weekday(), d.isoweekday(), d.isocalendar(), d.toordinal(), d.ctime())
print(d + timedelta(days=1), d - date(2000, 1, 1), d.replace(year=2023, day=28), date.fromordinal(738000), date.fromisoformat('2023-W05-3'))
print(d.strftime('%A %d %B %Y, jour %j, semaine %U/%W, %a %b %y %m/%d %%'), format(d, '%Y'), f'{d:%d.%m.%Y}')
print(date.fromisocalendar(2020, 53, 7), date(2021, 1, 3).isocalendar(), date.min, date.max, date.resolution)
td = timedelta(days=1, hours=25, minutes=-3, seconds=0.5, microseconds=12)
print(td, repr(td), td.total_seconds(), -td, abs(-td), td * 2, td / 3, td // timedelta(hours=1), td % timedelta(hours=1), td * 1.5)
print(timedelta(0), repr(timedelta(0)), timedelta(weeks=2), timedelta(milliseconds=1500), timedelta(days=-1, seconds=5), repr(timedelta(days=-1, seconds=5)), divmod(timedelta(hours=5), timedelta(hours=2)))
print(timedelta.max, timedelta.min, timedelta(hours=1) < timedelta(days=1), sorted([timedelta(1), timedelta(0, 5), -timedelta(1)]))
t = time(13, 5, 9, 1234)
print(t, repr(t), t.isoformat('milliseconds'), t.isoformat('minutes'), t.strftime('%I:%M %p'), time(7), repr(time(7, 30)), time.fromisoformat('04:23:01.000384'))
dt = datetime(2024, 3, 10, 14, 30, 15, 250000)
print(dt, repr(dt), dt.date(), dt.time(), dt.isoformat(), dt.isoformat(' ', 'seconds'), dt.timestamp(), dt.weekday())
print(dt + timedelta(hours=12), dt - timedelta(days=400), dt - datetime(2024, 1, 1), dt.replace(microsecond=0), dt.strftime('%Y-%m-%dT%H:%M:%S.%f'))
print(datetime.fromisoformat('2011-11-04T00:05:23+04:00'), repr(datetime.fromisoformat('2011-11-04 00:05:23.283Z')), datetime.fromisoformat('20111104T000523'))
print(datetime.fromtimestamp(1700000000, tz=timezone.utc), datetime.utcfromtimestamp(0) if False else '', datetime(2024, 1, 1, tzinfo=UTC).timestamp())
aw = datetime(2024, 6, 1, 12, tzinfo=timezone(timedelta(hours=2)))
print(aw, repr(aw), aw.utcoffset(), aw.tzname(), aw.astimezone(UTC), aw == datetime(2024, 6, 1, 10, tzinfo=UTC), hash(aw) == hash(datetime(2024, 6, 1, 10, tzinfo=UTC)))
print(timezone(timedelta(hours=-5), 'EST'), repr(timezone(timedelta(hours=-5, minutes=-30))), repr(UTC), timezone.utc is UTC, str(timezone(timedelta(0))))
print(aw.strftime('%z %Z %:z'), datetime.combine(date(2020, 1, 1), time(5, 6)), datetime.strptime('2023-07-14 08:09:10', '%Y-%m-%d %H:%M:%S'))
print(datetime.strptime('14/Jul/2023:08:09:10 +0200', '%d/%b/%Y:%H:%M:%S %z'), datetime.strptime('03:04 PM', '%I:%M %p'), datetime.strptime('2024-060', '%Y-%j'))
print(datetime.min, datetime.max, dt.timetuple()[:9], dt.ctime(), sorted([dt, datetime(2000, 1, 1), datetime(2030, 5, 5)])[0])
try:
    datetime(2024, 1, 1) < aw
except TypeError as e:
    print(e)
try:
    date(2023, 2, 29)
except ValueError as e:
    print(e)
try:
    datetime.fromisoformat('2024-13-01')
except ValueError as e:
    print(e)
try:
    datetime.strptime('2024', '%Y-%m')
except ValueError as e:
    print(e)
print(date(2024, 1, 1) == datetime(2024, 1, 1), {d: 1}[date(2024, 2, 29)], isinstance(dt, date), dtm.MAXYEAR)
