import math
print(math.remainder(10, 3), math.remainder(11, 3), math.remainder(-7.5, 2), math.remainder(5, 2))
print(math.nextafter(1.0, 2.0), math.nextafter(1.0, 0.0), math.nextafter(0.0, -1.0), math.nextafter(1.0, 2.0, steps=5))
print(math.ulp(1.0), math.ulp(0.0), math.ulp(1e300), math.ulp(-3.5))
print(math.fma(2.0, 3.0, 4.0), math.fma(0.1, 10, -1), math.fma(1e308, 10, -1e308) if False else '')
print(math.sumprod([1, 2, 3], [4, 5, 6]), math.sumprod([0.1, 0.2, 0.3], [1, 1, 1]), math.sumprod([0.1]*10, [0.1]*10))
print(math.erf(0.5), math.erfc(2), math.gamma(5.5), math.lgamma(100), math.cbrt(27), math.cbrt(-2))
print(math.asinh(1), math.acosh(2), math.atanh(0.5))
for bad in [(math.gamma, 0), (math.gamma, -1), (math.lgamma, -2), (math.atanh, 1), (math.acosh, 0.5)]:
    try:
        bad[0](bad[1])
    except ValueError as e:
        print(bad[0].__name__, e)
try:
    math.gamma(200)
except OverflowError as e:
    print('ovf', e)
from abc import ABC, ABCMeta, abstractmethod
class Shape(ABC):
    @abstractmethod
    def area(self): ...
    @abstractmethod
    def perimeter(self): ...
class Sq(Shape):
    def __init__(self, s): self.s = s
    def area(self): return self.s ** 2
    def perimeter(self): return 4 * self.s
try:
    Shape()
except TypeError as e:
    print(e)
class Half(Shape):
    def area(self): return 1
try:
    Half()
except TypeError as e:
    print(e)
print(Sq(3).area(), isinstance(Sq(2), Shape), issubclass(Sq, Shape))
class Duck: pass
Shape.register(Duck)
print(isinstance(Duck(), Shape), issubclass(Duck, Shape), Shape.__abstractmethods__ == frozenset({'area', 'perimeter'}))
import collections.abc as cabc
from collections.abc import Iterable, Mapping, Sequence, Sized, Hashable, Callable, Iterator, MutableMapping, Generator
print(isinstance([1], Iterable), isinstance({}, Mapping), isinstance((), Sequence), isinstance("x", Sequence))
print(isinstance(3, Hashable), isinstance([], Hashable), isinstance(len, Callable), isinstance(iter([]), Iterator))
print(isinstance((x for x in []), Generator), isinstance({}, MutableMapping), isinstance(5, Iterable), isinstance(set(), cabc.Set))
class MyMap(Mapping):
    def __init__(self, d): self._d = d
    def __getitem__(self, k): return self._d[k]
    def __len__(self): return len(self._d)
    def __iter__(self): return iter(self._d)
m = MyMap({'a': 1, 'b': 2})
print(len(m), list(m.keys()), list(m.items()), m.get('z', 0), 'a' in m, m == {'a': 1, 'b': 2})
class Seq(Sequence):
    def __getitem__(self, i):
        if i >= 3: raise IndexError
        return i * 10
    def __len__(self): return 3
s = Seq()
print(list(s), 20 in s, s.index(20), s.count(10), list(reversed(s)))
class It:
    def __iter__(self): return self
    def __next__(self): raise StopIteration
print(isinstance(It(), Iterator), issubclass(It, Iterable), issubclass(int, Sized))
import numbers
print(isinstance(5, numbers.Integral), isinstance(5.0, numbers.Real), isinstance(5.0, numbers.Integral), isinstance(1j, numbers.Complex), isinstance(True, numbers.Number))
from fractions import Fraction as Fr
a = Fr(3, 4); b = Fr('1/3'); c = Fr(0.5); d = Fr('-1.25e-1')
print(a, b, c, d, repr(a), a + b, a - b, a * b, a / b, a // b, a % b, -a, abs(d), a ** 2, a ** -2, 2 ** Fr(1, 2))
print(a + 1, 1 + a, a + 0.5, a == 0.75, a < 1, a > b, hash(a) == hash(0.75), hash(Fr(5)) == hash(5), sorted([a, b, c, d]))
print(Fr(355, 113).limit_denominator(10), Fr(3.141592653589793).limit_denominator(1000), round(Fr(7, 2)), round(Fr(5, 2)), round(Fr(22, 7), 2))
print(math.floor(Fr(-7, 2)), math.ceil(Fr(-7, 2)), int(Fr(-7, 2)), float(Fr(1, 3)), Fr(1, 3).as_integer_ratio(), Fr(6, 4).numerator)
print(f"{Fr(1, 3):.5f}", f"{Fr(2, 3):10.2%}", format(Fr(1, 7), '.3e'), format(Fr(1234567, 10), ',.2f'))
try:
    Fr(1, 0)
except ZeroDivisionError as e:
    print(e)
try:
    Fr('abc')
except ValueError as e:
    print(e)
import statistics as st
data = [2.75, 1.75, 1.25, 0.25, 0.5, 1.25, 3.5]
print(st.mean(data), st.fmean(data), st.median(data), st.median_low([1, 3, 5, 7]), st.median_high([1, 3, 5, 7]), st.median_grouped([52, 52, 53, 54]))
print(st.mode([1, 1, 2, 3, 3, 3]), st.multimode('aabbbbccddddeeffffgg'), st.mean([1, 2, 3, 4, 4]), st.mean([Fr(1, 3), Fr(2, 3)]))
print(st.variance(data), st.stdev(data), st.pvariance(data), st.pstdev(data), st.variance([1, 2, 3, 4]))
print(st.geometric_mean([54, 24, 36]), st.harmonic_mean([40, 60]), st.harmonic_mean([40, 60], weights=[5, 30]))
print(st.quantiles([105, 129, 87, 86, 111, 111, 89, 81, 108, 92, 110, 100, 75, 105, 103, 109, 76, 119, 99, 91, 103, 129, 106, 101, 84, 111, 74, 87, 86, 103, 103, 106, 86, 111, 75, 87, 102, 121, 111, 88, 89, 101, 106, 95, 103, 107, 101, 81, 109, 104], n=10))
print(st.quantiles([1, 2, 3, 4, 5, 6, 7, 8, 9, 10], n=4, method='inclusive'))
x = [1, 2, 3, 4, 5, 6, 7, 8, 9]; y = [1, 2, 3, 1, 2, 3, 1, 2, 3]
print(st.covariance(x, y), st.correlation(x, y), st.correlation(x, [9 - v for v in x], method='ranked'), st.linear_regression(x, [2 * v + 1 for v in x]))
nd = st.NormalDist(100, 15)
print(nd, nd.cdf(130), nd.pdf(100), nd.inv_cdf(0.975), nd.quantiles(), nd.zscore(85), nd.overlap(st.NormalDist(110, 10)))
print(st.NormalDist.from_samples([2.5, 3.1, 2.1, 2.4, 2.7, 3.5]), nd + 5, nd * 2, nd - st.NormalDist(10, 20))
f = st.kde([-2.1, -1.3, -0.4, 1.9, 5.1, 6.2], h=1.5)
print(round(f(2.5), 12), round(st.kde([1, 2, 3], h=1, kernel='triangular', cumulative=True)(2.2), 12))
try:
    st.mean([])
except st.StatisticsError as e:
    print('StatisticsError', e)
