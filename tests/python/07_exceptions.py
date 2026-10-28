def risky(n):
    if n == 0: raise ValueError("zero interdit")
    if n == 1: raise KeyError("cle")
    if n == 2: return 1 / 0
    if n == 3: return [][1]
    if n == 4: return {}["x"]
    if n == 5: return int("abc")
    if n == 6: return undefined_name
    if n == 7: return "a" + 1
    if n == 8: return None.attr
    if n == 9: raise RuntimeError
    return "ok"
for i in range(11):
    try:
        print(i, risky(i))
    except (ValueError, KeyError) as e:
        print(i, type(e).__name__, e, repr(e), e.args)
    except ZeroDivisionError as e:
        print(i, "div", e)
    except LookupError as e:
        print(i, "lookup", type(e).__name__, e)
    except Exception as e:
        print(i, "autre", type(e).__name__, e)
    else:
        print(i, "else")
    finally:
        print(i, "finally")
def f():
    try:
        return "try"
    finally:
        print("finally avant le retour")
print(f())
def g():
    for i in range(3):
        try:
            if i == 1: continue
            if i == 2: break
        finally:
            print("tour", i)
    return i
print(g())
class MyError(Exception):
    def __init__(self, code):
        super().__init__(f"code {code}")
        self.code = code
try:
    raise MyError(42)
except MyError as e:
    print(e, e.code, isinstance(e, Exception))
try:
    try:
        raise KeyError("a")
    except KeyError as e:
        raise ValueError("b") from e
except ValueError as e:
    print(repr(e), repr(e.__cause__), e.__context__ is e.__cause__)
try:
    try:
        1 / 0
    except ZeroDivisionError:
        raise TypeError("pendant")
except TypeError as e:
    print(repr(e.__context__))
def reraise():
    try:
        raise IndexError("x")
    except IndexError:
        raise
try:
    reraise()
except IndexError as e:
    print("reraise", e)
try:
    assert 1 == 2, "assertion fausse"
except AssertionError as e:
    print("AssertionError:", e)
e_saved = None
try:
    raise ValueError("v")
except ValueError as e:
    e_saved = e
print(e_saved, "e" in dir())
try:
    raise StopIteration(5)
except StopIteration as e:
    print("stop", e.value)
try:
    import module_inexistant
except ImportError as e:
    print(type(e).__name__, e)
try:
    [].pop()
except IndexError as e:
    print(e)
try:
    x = int("12x")
except ValueError as e:
    print(e)
def nested():
    try:
        raise ValueError("interne")
    except ValueError:
        try:
            raise TypeError("second")
        except TypeError as inner:
            return repr(inner.__context__)
print(nested())
err = ExceptionGroup if False else None
print(issubclass(KeyError, LookupError), issubclass(ZeroDivisionError, ArithmeticError), BaseException.__subclasses__ if False else "-")
try:
    raise ValueError("une", "deux")
except ValueError as e:
    print(str(e), e.args)
try:
    1 / 0
except ArithmeticError:
    print("ArithmeticError rattrape")
def deep(n):
    return deep(n + 1)
try:
    deep(0)
except RecursionError as e:
    print("RecursionError:", e)
raise SystemExit(3)
