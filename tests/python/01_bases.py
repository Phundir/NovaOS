# Entiers, flottants, chaines : les bases.
print(1 + 2 * 3, (1 + 2) * 3, 7 / 2, 7 // 2, -7 // 2, 7 % 3, -7 % 3, 2 ** 10, 2 ** -1)
print(10 ** 30, -(10 ** 30) // 7, (10 ** 30) % 97, divmod(-17, 5), divmod(17.5, -4))
print(0.1 + 0.2, 1 / 3, 2 / 3, 1e16, 1e15, 1.5e-7, 123456789.123456789, 5e-324, 1.7976931348623157e308)
print(float("inf"), float("-inf"), float("nan"), -0.0, 0.0 == -0.0, float("1_000.5"), int("0x1f", 16), int("  42  "))
print(int(3.99), int(-3.99), round(2.5), round(3.5), round(-0.5), round(2.675, 2), round(1234.5678, -2), round(15, -1))
print(abs(-5), abs(-2.5), max(3, 7, 1), min([4, 2, 8]), sum([1, 2, 3]), sum([0.1] * 10), sum([], 10))
print(bin(10), oct(64), hex(255), hex(-255), int("1010", 2), int("ff", 16), int("0o17", 0), chr(65), ord("z"))
print(True + True, True * 10, False == 0, isinstance(True, int), bool(""), bool("0"), bool([]), bool(0.0))
print(3 < 5 < 7, 3 < 5 > 7, 1 == 1.0, "a" < "b", [1, 2] < [1, 3], (1, 2) == (1, 2), None is None)
print(hash(1) == hash(1.0), hash(-1), hash(2**61), hash(-2**61), hash(0.5), hash(1e100) == hash(int(1e100)))
x = 5
x += 3; x *= 2; x -= 1; x //= 3; x **= 2; x %= 7
print(x, ~5, 5 & 3, 5 | 3, 5 ^ 3, 1 << 70, -1 >> 3, (2**100) >> 90)
print(int.bit_length(1023), (255).to_bytes(2, "big"), int.from_bytes(b"\x01\x00", "little"), (3.5).as_integer_ratio())
print(1_000_000, 0b1010, 0o777, 0xDEAD_BEEF, 1e3, 1_0.5e-1_0)
print(float.is_integer(3.0), (2.5).is_integer(), (10).bit_count(), pow(3, 4, 5), pow(2, -1, 7), pow(2, 0.5))
