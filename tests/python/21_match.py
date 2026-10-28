from dataclasses import dataclass

@dataclass
class Point:
    x: int
    y: int

def where(p):
    match p:
        case Point(x=0, y=0):
            return "origine"
        case Point(x=0, y=y):
            return f"axe y a {y}"
        case Point(x, 0):
            return f"axe x a {x}"
        case Point(x=x, y=y) if x == y:
            return f"diagonale {x}"
        case Point():
            return "ailleurs"
        case _:
            return "pas un point"

for p in [Point(0, 0), Point(0, 5), Point(3, 0), Point(2, 2), Point(1, 7), 42]:
    print(where(p))

def cmd(c):
    match c.split():
        case ["quit"]:
            return "bye"
        case ["go", ("north" | "south") as d]:
            return "vertical " + d
        case ["go", d]:
            return "go " + d
        case ["drop", *objs] if objs:
            return f"drop {objs}"
        case [first, *rest]:
            return f"other {first} {rest}"
        case []:
            return "vide"

for c in ["quit", "go north", "go east", "drop a b c", "drop", "x y z", ""]:
    print(cmd(c))

def value(v):
    match v:
        case None:
            return "none"
        case True:
            return "true"
        case 0 | 1 | 2:
            return "petit"
        case int(n) if n < 0:
            return f"negatif {n}"
        case int() | float():
            return "nombre"
        case str() as s:
            return "chaine " + s
        case {"type": "point", "x": x, **rest}:
            return f"dict x={x} reste={rest}"
        case {"type": t}:
            return "type " + t
        case (a, b):
            return f"paire {a} {b}"
        case [a, b, *_]:
            return f"liste {a} {b}"
        case _:
            return "autre"

for v in [None, True, 1, -3, 3.5, "abc", {"type": "point", "x": 1, "y": 2}, {"type": "carre"}, (1, 2), [1, 2, 3, 4], {1, 2}]:
    print(value(v))

x = 5
match x:
    case 1:
        print("un")
    case n:
        print("capture", n)
print(n)
