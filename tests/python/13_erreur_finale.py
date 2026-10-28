def a(n):
    return b(n) + 1
def b(n):
    return {"x": 1}[n]
print("avant")
a("y")
print("jamais")
