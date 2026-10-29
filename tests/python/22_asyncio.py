import asyncio

async def worker(name, delay, log):
    log.append(f"{name} start")
    await asyncio.sleep(delay)
    log.append(f"{name} end")
    return name.upper()

async def countdown(n):
    while n > 0:
        print("tick", n)
        await asyncio.sleep(0)
        n -= 1
    return "fin"

async def agen(n):
    for i in range(n):
        await asyncio.sleep(0)
        yield i * i

async def pairs():
    yield 'a', 1
    yield 'b', 2

class Ctx:
    async def __aenter__(self):
        print("enter")
        return self
    async def __aexit__(self, t, v, tb):
        print("exit", t.__name__ if t else None)
        return True

async def slow():
    await asyncio.sleep(1)
    return "trop tard"

async def main():
    log = []
    results = await asyncio.gather(worker("a", 0.3, log), worker("b", 0.1, log), worker("c", 0.2, log))
    print(results)
    print(log)
    print(await countdown(3))
    t1 = asyncio.create_task(countdown(2))
    t2 = asyncio.create_task(countdown(2))
    await t1
    await t2
    print([x async for x in agen(4)], [await asyncio.sleep(0, i) for i in range(3)], {k: v async for k, v in pairs()})
    g = (x + 1 async for x in agen(3))
    print([y async for y in g])
    out = []
    async for v in agen(5):
        out.append(v)
    print(out)
    async with Ctx() as c:
        print("dans", type(c).__name__)
        raise ValueError("avale")
    print("apres")
    try:
        await asyncio.wait_for(slow(), timeout=0.05)
    except TimeoutError:
        print("timeout")
    q = asyncio.Queue()
    async def producer():
        for i in range(3):
            await q.put(i)
            print("put", i)
        await q.put(None)
    async def consumer():
        while (item := await q.get()) is not None:
            print("got", item)
    await asyncio.gather(producer(), consumer())
    ev = asyncio.Event()
    async def waiter():
        print("attend")
        await ev.wait()
        print("libre")
    w = asyncio.create_task(waiter())
    await asyncio.sleep(0)
    ev.set()
    await w
    task = asyncio.create_task(slow())
    await asyncio.sleep(0)
    task.cancel()
    try:
        await task
    except asyncio.CancelledError:
        print("annule", task.cancelled())
    r = await asyncio.gather(asyncio.sleep(0.01, "x"), slow_fail(), return_exceptions=True)
    print(r)
    return 42

async def slow_fail():
    raise KeyError("k")

print(asyncio.run(main()))
c = countdown(1)
print(type(c).__name__)
try:
    c.send(None)
    c.send(None)
except StopIteration as e:
    print("stop", e.value)
