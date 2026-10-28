print(repr(1j), repr(-1j), repr(complex(0.0,-0.0)), repr(complex(-0.0,1)), repr(1+2j), repr(complex(1e16,2.5)), repr(complex(float('inf'),float('nan'))))
try: 1j/0
except ZeroDivisionError as e: print(e)
try: 0j**-1
except ZeroDivisionError as e: print(e)
print(hash(1+2j), hash(1.5j), hash(complex(3,0)))
print(f'{1+2j:.2f}', format(1+2j), f'{3j:>10}', str(2+0j), complex('1+2j'), complex(' 3.5 '), complex(2), complex(1,2)*1j, (1+2j)**2, (1+2j)**0.5, 2**(1j))
print((1+2j)/(3-4j), abs(3+4j), (1+2j).conjugate(), (2+3j).real, (2+3j).imag)
try: (1j) < (2j)
except TypeError as e: print(e)
try: int(1j)
except TypeError as e: print(e)
print(1j == 1j, 2+0j == 2, {1+0j: 'a'}[1], isinstance(1j, complex), type(1j).__name__, complex('-1.5e3-2j'), sum([1j, 2, 3.5]))
z = 3 - 4j
z *= 2
print(z, -z, +z, bool(0j), bool(1j), abs(-5j))
import cmath
print(cmath.sqrt(-1), cmath.sqrt(2+3j), cmath.exp(1j*cmath.pi), cmath.phase(-1), cmath.polar(1+1j), cmath.log(1j), cmath.rect(2, 0.5))
