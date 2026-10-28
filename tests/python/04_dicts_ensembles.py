d = {"pomme": 3, "kiwi": 1}
d["banane"] = 5; d.update({"kiwi": 2}, fig=7)
print(d, len(d), "kiwi" in d, d.get("x"), d.get("x", 0), d.setdefault("y", 9), d)
print(list(d.keys()), list(d.values()), list(d.items()), d.pop("y"), d.pop("zz", "absent"))
print(sorted(d.items(), key=lambda kv: kv[1]), {v: k for k, v in d.items()}, dict(zip("abc", range(3))))
print(dict.fromkeys(["a", "b"], 0), dict(a=1, b=2), dict([("x", 1)]), d.popitem(), d)
counts = {}
for w in "le chat et le chien et le rat".split():
    counts[w] = counts.get(w, 0) + 1
print(counts, max(counts, key=counts.get))
s = {3, 1, 2}
s.add(10); s.discard(1); s |= {20}
print(s, len(s), 2 in s, s & {2, 3, 99}, s - {2}, s ^ {3, 4}, {1, 2} <= {1, 2, 3}, {1, 2} < {1, 2})
print(set("hello"), frozenset([1, 2]) | {3}, set(), {1, 2}.union([5], (6,)), {1, 2, 3}.intersection({2, 3, 4}, [3]))
print(sorted({"b", "a"}), {(1, 2), (1, 2)}, {1.0, 1, True}, {0, False}, set([1, 2, 3]) == {3, 2, 1})
print({i * 7 % 13 for i in range(20)}, {"x": 1} == {"x": 1.0}, {1: "a"} | {2: "b"})
e = {}
print(e, bool(e), dict(), list({}.items()))
dd = {(1, 2): "tuple", 3.5: "float", None: "none", True: "bool"}
print(dd[(1, 2)], dd[3.5], dd[None], dd[1], dd)
keys = d.keys(); d["nouveau"] = 1; print(keys, len(keys), "nouveau" in keys)
print(d.keys() & {"kiwi", "zzz"}, d.items() >= {("kiwi", 2)} if False else "-", list(reversed(d)))
try:
    for k in d:
        d[k + "x"] = 1
except RuntimeError as err:
    print("RuntimeError:", err)
