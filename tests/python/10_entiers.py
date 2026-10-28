a = 2 ** 200
b = 3 ** 150
print(a, b, a * b, a // b, a % b, -a // b, -a % b, b // -a)
print(a - b, a + b, (a + b) - a == b, a ** 2 == a * a, abs(-a), a.bit_length(), b.bit_length())
f = 1
for i in range(1, 60):
    f *= i
print(f, len(str(f)), f % 1000000007, f // 10 ** 70)
print(int("9" * 50) + 1, int("-123456789012345678901234567890"), int("ff" * 20, 16))
print(hex(2 ** 100), oct(2 ** 70), bin(2 ** 65 - 1), str(-2 ** 64), repr(10 ** 30))
print(2 ** 64 // 3, 2 ** 64 % 3, divmod(2 ** 70, 12345), divmod(-(2 ** 70), 12345))
print((2 ** 100) >> 50, (2 ** 100) << 3, ~(2 ** 70), (2 ** 70) & (2 ** 70 - 1), (2 ** 70) | 1, (2 ** 70) ^ (2 ** 69))
print(-(2 ** 70) >> 3, -(2 ** 70) & 0xFF, (-1) & (2 ** 70), -(2 ** 70) | 5)
print(pow(3, 100, 10 ** 9 + 7), pow(2, 10 ** 6, 1000), pow(12345678901234567890, 3), pow(-2, 63))
print(10 ** 20 / 3, 2 ** 1000 / 2 ** 990, (10 ** 30) / (10 ** 10), 1 / (2 ** 60), float(2 ** 70), float(10 ** 300))
print(int(1e20), int(1.5e30), int(-2.7), round(10 ** 20 + 0.5), 2 ** 63, -2 ** 63, 2 ** 63 - 1)
print(sum(range(10 ** 6)), sum(i * i for i in range(1000)), max(2 ** 70, 2 ** 69 * 3), sorted([2 ** 70, -1, 2 ** 64]))
print(hash(2 ** 70), hash(-(2 ** 70)), hash(2 ** 61 - 1), hash(2 ** 61), hash(12345678901234567890))
print((2 ** 70) == 2.0 ** 70, 2 ** 70 < 2.0 ** 71, 2 ** 53 + 1 == float(2 ** 53 + 1), 10 ** 400 > 1e308)
import math
print(math.factorial(30), math.gcd(2 ** 40, 6 ** 20), math.isqrt(10 ** 30), math.comb(50, 25), math.perm(10, 3), math.lcm(12, 18, 30))
n = 1
for _ in range(100):
    n = n * 31 + 7
print(n % 999983, n.bit_length(), str(n)[:20], len(str(n)))
print(int.from_bytes((2 ** 70).to_bytes(9, "little"), "little") == 2 ** 70, (-5).to_bytes(2, "big", signed=True))
print(f"{2**70:,}", f"{2**70:x}", f"{-2**70:+_}", format(10**25, "e"), f"{10**20:.3e}")
