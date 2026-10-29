from decimal import *
import decimal
D = Decimal
print(D('1.10') + D('2.20'), D('1.1') * D('3'), D(1) / D(7), D(2) ** D('0.5'), D('10') / D('4'), D('1.00') / D('3'), repr(D('3.14')))
print(D('7') // D('2'), D('-7') // D('2'), D('7') % D('-2'), D('-7') % D('2'), divmod(D('7.5'), D('2')), D(10) ** -2, D(2) ** 100, D('1.5') ** 2)
print(D(0.1), D.from_float(0.5), D('1e+3'), D('1E3') + 0, D('-0'), D('123.4500').normalize(), D('1e5').normalize(), D('0.00001'), D('0.000001'), D('1234567890123456789012345678901'))
print(D('1.2345').quantize(D('0.01')), D('1.2355').quantize(D('0.001')), D('2.5').quantize(D('1'), rounding=ROUND_HALF_UP), D('-2.5').to_integral_value(), D('7.325').quantize(D('.01'), rounding=ROUND_DOWN))
print(round(D('2.5')), round(D('3.14159'), 2), int(D('-3.9')), float(D('0.1')), D('3.7').__floor__(), D('-3.2').__ceil__(), D('1.5').as_integer_ratio(), D('-1.25').as_tuple())
print(D(2).sqrt(), D('16').sqrt(), D('0.0016').sqrt(), D(1).exp(), D(10).ln(), D(1000).log10(), D(2).log10(), D('0.5').ln(), D(-1).exp(), D(100).sqrt())
print(D('1.1') ** D('2.5'), D(4) ** D('0.5'), D(8) ** (D(1) / 3), D('Infinity'), D('-inf'), D('nan'), D('NaN123'), D('sNaN'), D('inf') + 1, -D('inf'))
print(D(1) == 1, D('1.0') == D(1), D('0.1') == 0.1, D('0.5') == 0.5, D(1) < 2, D('1.5') > 1.4, hash(D(1)) == hash(1), hash(D('0.5')) == hash(0.5), sorted([D('1.1'), D('-2'), D('0.05')]))
print(f"{D('1234567.891'):,.2f}", f"{D('0.000123'):.2e}", f"{D('12.5'):%}", f"{D('-3.14159'):+10.3f}", f"{D('42'):010}", f"{D('1.5'):g}", format(D('123.456'), '.4g'), format(D('1e-10'), 'f'), format(D('-0.0'), 'z.1f'))
print(getcontext())
with localcontext() as ctx:
    ctx.prec = 5
    print(D(1) / D(3), D('123456789') + 1, ctx.prec)
    ctx.rounding = ROUND_UP
    print(D(1) / D(3), D(2) / D(3))
print(D(1) / D(3))
with localcontext(prec=50):
    print(D(2).sqrt(), D(1).exp(), D(2).ln())
c = Context(prec=4, rounding=ROUND_HALF_UP)
print(c.add(D('1.2345'), D('2')), c.divide(1, 8), c.power(2, 10), c.sqrt(2), c.create_decimal('3.14159'))
for expr in ['D(1) / 0', 'D(0) / 0', "D('abc')", 'D(1) + 1.5', "D('-1').sqrt()", "D('inf') - D('inf')"]:
    try:
        print(eval(expr))
    except Exception as e:
        print(type(e).__name__, e)
ctx = getcontext()
ctx.traps[DivisionByZero] = False
print(D(1) / 0, D(-1) / 0, ctx.flags[DivisionByZero])
ctx.traps[DivisionByZero] = True
ctx.clear_flags()
x = D(1) / D(3)
print(ctx.flags[Inexact], ctx.flags[Rounded], ctx.flags[DivisionByZero], D('1.5').is_finite(), D('nan').is_nan(), D('-0').is_signed(), D('1e-1000000').number_class(), D(5).number_class())
print(D('123.456').adjusted(), D('1.0').same_quantum(D('2.0')), D('1.23').compare(D('1.230')), D(-3).copy_abs(), D(3).copy_sign(D(-1)), D(5).max(D(7)), D(5).min(D('nan')), D('1.5').fma(2, D('0.25')))
print(D('1.000').next_plus(), D(1).next_minus(), D('12345').scaleb(-2), D('1e2').to_eng_string(), D('123e5').to_eng_string(), D('0.0001234').to_eng_string(), D(7).remainder_near(D(4)), pow(D(3), D(4), D(5)))
print(sum([D('0.1')] * 10), D('0.1') * 3 == D('0.3'), max(D(1), D('1.5')), abs(D('-2.50')), -D('0'), +D('1.23456789012345678901234567890'))
print(DecimalTuple(0, (1, 2), -1), D((1, (3, 1, 4), -2)), issubclass(DivisionByZero, ZeroDivisionError), issubclass(InvalidOperation, ArithmeticError), decimal.MAX_PREC)
import random
random.seed(7)
out = []
for prec in (5, 28, 40):
    with localcontext(prec=prec):
        for i in range(15):
            x = D(random.randint(1, 10**8)).scaleb(random.randint(-12, 6))
            y = D(random.randint(1, 999)).scaleb(-random.randint(0, 3))
            out.append((str(x.exp()) if x.adjusted() < 3 else '-', str(x.ln()), str(x.log10()), str(x.sqrt()), str(x ** y) if x.adjusted() < 3 else '-', str(x / y), str(x - y * 3)))
for o in out:
    print(*o)
print(D(1) + D('1e-30'), D('9e999999') + 1, D('1e-999999') / 10**10)
print(datetime_check := 0)
import datetime
try:
    datetime.date(2020, 1, 1) + 1
except TypeError as e:
    print(e)
