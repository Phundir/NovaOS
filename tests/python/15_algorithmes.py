def crible(n):
    est_premier = [True] * (n + 1)
    est_premier[0] = est_premier[1] = False
    for i in range(2, int(n ** 0.5) + 1):
        if est_premier[i]:
            for j in range(i * i, n + 1, i):
                est_premier[j] = False
    return [i for i, p in enumerate(est_premier) if p]
p = crible(200000)
print(len(p), p[:10], p[-3:], sum(p))
def tri_fusion(t):
    if len(t) <= 1:
        return t
    m = len(t) // 2
    a, b = tri_fusion(t[:m]), tri_fusion(t[m:])
    out = []
    i = j = 0
    while i < len(a) and j < len(b):
        if a[i] <= b[j]:
            out.append(a[i]); i += 1
        else:
            out.append(b[j]); j += 1
    return out + a[i:] + b[j:]
seed = 12345
data = []
for _ in range(20000):
    seed = (seed * 1103515245 + 12345) % 2 ** 31
    data.append(seed % 100000)
s = tri_fusion(data)
print(s[:5], s[-5:], s == sorted(data))
memo = {}
def fib(n):
    if n < 2:
        return n
    if n in memo:
        return memo[n]
    memo[n] = fib(n - 1) + fib(n - 2)
    return memo[n]
print(fib(300))
def lcs(a, b):
    dp = [[0] * (len(b) + 1) for _ in range(len(a) + 1)]
    for i in range(1, len(a) + 1):
        for j in range(1, len(b) + 1):
            if a[i - 1] == b[j - 1]:
                dp[i][j] = dp[i - 1][j - 1] + 1
            else:
                dp[i][j] = max(dp[i - 1][j], dp[i][j - 1])
    return dp[-1][-1]
print(lcs("ACCGGTCGAGTGCGCGGAAGCCGGCCGAA" * 4, "GTCGTTCGGAATGCCGTTGCTCTGTAAA" * 4))
graph = {i: [(i * 7 + 3) % 500, (i * 13 + 1) % 500, (i + 1) % 500] for i in range(500)}
def bfs(start):
    seen = {start: 0}
    queue = [start]
    k = 0
    while k < len(queue):
        node = queue[k]; k += 1
        for nxt in graph[node]:
            if nxt not in seen:
                seen[nxt] = seen[node] + 1
                queue.append(nxt)
    return seen
d = bfs(0)
print(len(d), max(d.values()), sum(d.values()))
words = ("le chat mange la souris et la souris mange le fromage " * 200).split()
freq = {}
for w in words:
    freq[w] = freq.get(w, 0) + 1
print(sorted(freq.items(), key=lambda kv: (-kv[1], kv[0])))
class Noeud:
    def __init__(self, v):
        self.v, self.g, self.d = v, None, None
def inserer(r, v):
    if r is None:
        return Noeud(v)
    if v < r.v:
        r.g = inserer(r.g, v)
    else:
        r.d = inserer(r.d, v)
    return r
def parcours(r, out):
    if r:
        parcours(r.g, out); out.append(r.v); parcours(r.d, out)
racine = None
for x in data[:3000]:
    racine = inserer(racine, x)
o = []; parcours(racine, o)
print(o[:5], len(o), o == sorted(data[:3000]))
m = [[(i * j) % 7 for j in range(60)] for i in range(60)]
prod = [[sum(m[i][k] * m[k][j] for k in range(60)) for j in range(60)] for i in range(60)]
print(prod[5][:6], sum(map(sum, prod)))
