//! `random`: CPython's generator, bit for bit.
//!
//! The Mersenne Twister (MT19937) seeded as `_randommodule.c` seeds it
//! (`init_by_array` over the 32-bit words of the seed), `random()` from
//! two outputs as 53 bits, and the Python-level algorithms of
//! `Lib/random.py` (`_randbelow` by rejection on `getrandbits`, `shuffle`,
//! `sample`, `choices`...) unchanged: `random.seed(42)` then
//! `random.randint(1, 6)` gives the number CPython gives.
//!
//! The module functions are the methods of a hidden `Random` instance
//! whose generator is the VM's first one; `random.Random(seed)` makes
//! instances with generators of their own.

use alloc::rc::Rc;
use alloc::vec::Vec;

use super::super::args;
use super::super::ast::BinOp;
use super::super::int::Int;
use super::super::types::{class, method, num::float_of};
use super::super::value::*;
use super::super::vm::{PyResult, Vm};
use crate::web::js::num::Num;

const N: usize = 624;
const M: usize = 397;

/// MT19937.
#[derive(Clone)]
pub struct Mt {
    state: [u32; N],
    index: usize,
    /// `gauss` makes two numbers at a time.
    gauss_next: Option<Num>,
}

impl Mt {
    fn init_genrand(seed: u32) -> Mt {
        let mut state = [0u32; N];
        state[0] = seed;
        for i in 1..N {
            state[i] = 1_812_433_253u32.wrapping_mul(state[i - 1] ^ (state[i - 1] >> 30)).wrapping_add(i as u32);
        }
        Mt { state, index: N, gauss_next: None }
    }

    /// `init_by_array`: CPython's seeding.
    pub fn from_key(key: &[u32]) -> Mt {
        let mut mt = Mt::init_genrand(19_650_218);
        let s = &mut mt.state;
        let (mut i, mut j) = (1usize, 0usize);
        let length = key.len().max(1);
        for _ in 0..N.max(length) {
            let previous = s[i - 1] ^ (s[i - 1] >> 30);
            s[i] = (s[i] ^ previous.wrapping_mul(1_664_525)).wrapping_add(key.get(j).copied().unwrap_or(0)).wrapping_add(j as u32);
            i += 1;
            j += 1;
            if i >= N {
                s[0] = s[N - 1];
                i = 1;
            }
            if j >= length {
                j = 0;
            }
        }
        for _ in 0..N - 1 {
            let previous = s[i - 1] ^ (s[i - 1] >> 30);
            s[i] = (s[i] ^ previous.wrapping_mul(1_566_083_941)).wrapping_sub(i as u32);
            i += 1;
            if i >= N {
                s[0] = s[N - 1];
                i = 1;
            }
        }
        s[0] = 0x8000_0000;
        mt
    }

    /// Seeded from an int, as `random.seed(n)`: its absolute value's
    /// 32-bit words, least significant first.
    pub fn from_int(seed: &Int) -> Mt {
        let mut n = seed.abs();
        let mut key = Vec::new();
        let word = Int::Small(1 << 32);
        while !n.is_zero() {
            let (q, r) = n.divmod(&word).unwrap_or((Int::zero(), Int::zero()));
            key.push(r.to_i64().unwrap_or(0) as u32);
            n = q;
        }
        if key.is_empty() {
            key.push(0);
        }
        Mt::from_key(&key)
    }

    pub fn from_u64(seed: u64) -> Mt {
        Mt::from_key(&[seed as u32, (seed >> 32) as u32])
    }

    pub fn next_u32(&mut self) -> u32 {
        const UPPER: u32 = 0x8000_0000;
        const LOWER: u32 = 0x7fff_ffff;
        const MATRIX: u32 = 0x9908_b0df;
        if self.index >= N {
            let s = &mut self.state;
            for k in 0..N {
                let y = (s[k] & UPPER) | (s[(k + 1) % N] & LOWER);
                let mag = if y & 1 == 1 { MATRIX } else { 0 };
                s[k] = s[(k + M) % N] ^ (y >> 1) ^ mag;
            }
            self.index = 0;
        }
        let mut y = self.state[self.index];
        self.index += 1;
        y ^= y >> 11;
        y ^= (y << 7) & 0x9d2c_5680;
        y ^= (y << 15) & 0xefc6_0000;
        y ^ (y >> 18)
    }

    /// A double in [0, 1) with 53 random bits.
    pub fn random(&mut self) -> Num {
        let a = (self.next_u32() >> 5) as u64;
        let b = (self.next_u32() >> 6) as u64;
        Num::from_u64((a << 26) | b).scale2(-53)
    }

    /// `getrandbits(k)`: words least significant first, the last one
    /// cut to the bits left.
    pub fn getrandbits(&mut self, k: u64) -> Int {
        if k <= 32 {
            return Int::Small((self.next_u32() >> (32 - k)) as i64);
        }
        let mut words = Vec::new();
        let mut left = k;
        while left > 0 {
            let mut r = self.next_u32();
            if left < 32 {
                r >>= 32 - left;
            }
            words.push(r);
            left = left.saturating_sub(32);
        }
        let mut value = Int::zero();
        for w in words.iter().rev() {
            value = value.shl(32).unwrap_or(Int::zero()).add(&Int::Small(*w as i64));
        }
        value
    }

    /// `_randbelow(n)` for n > 0: rejection sampling on bit_length bits.
    pub fn below(&mut self, n: &Int) -> Int {
        let k = n.bit_length() as u64;
        loop {
            let r = self.getrandbits(k);
            if r.cmp(n) == core::cmp::Ordering::Less {
                return r;
            }
        }
    }

    pub fn below_usize(&mut self, n: usize) -> usize {
        self.below(&Int::Small(n as i64)).to_i64().unwrap_or(0) as usize
    }
}

/// The generator of a `Random` instance.
fn rng(vm: &mut Vm, a: &[Value]) -> PyResult<usize> {
    if let Some(Value::Instance(i)) = a.first() {
        if let Some(Value::Int(Int::Small(id))) = i.dict.borrow().get_str("_generator") {
            return Ok(id as usize);
        }
    }
    vm.type_error("descriptor requires a 'Random' object")
}

fn seed_from(vm: &mut Vm, value: Option<&Value>) -> PyResult<Mt> {
    Ok(match value {
        None | Some(Value::None) => {
            let t = vm.host.time_ns() ^ vm.host.monotonic_ns().rotate_left(29);
            Mt::from_u64(t)
        }
        Some(Value::Int(i)) => Mt::from_int(i),
        Some(Value::Bool(b)) => Mt::from_int(&Int::Small(*b as i64)),
        Some(v @ (Value::Float(_) | Value::Str(_) | Value::Bytes(_))) => {
            // CPython hashes strings with SHA-512 here: the sequence of a
            // string seed differs from CPython's (a documented limit).
            let h = vm.hash(v)?;
            Mt::from_int(&Int::Small(h))
        }
        Some(other) => {
            let _ = other;
            return vm.type_error("The only supported seed types are: None, int, float, str, bytes, and bytearray.");
        }
    })
}

fn init(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let Some(Value::Instance(i)) = a.first() else {
        return Ok(Value::None);
    };
    let mt = seed_from(vm, a.get(1))?;
    vm.random.push(mt);
    let id = vm.random.len() - 1;
    i.dict.borrow_mut().set_str("_generator", Value::int(id as i64));
    Ok(Value::None)
}

fn seed(vm: &mut Vm, a: &[Value], kw: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let id = rng(vm, a)?;
    let value = args::get(&a[1..], kw, 0, "a").cloned();
    let mt = seed_from(vm, value.as_ref())?;
    vm.random[id] = mt;
    Ok(Value::None)
}

fn random(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let id = rng(vm, a)?;
    Ok(Value::Float(vm.random[id].random()))
}

fn getrandbits(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let id = rng(vm, a)?;
    let k = args::int(vm, a.get(1).unwrap_or(&Value::None))?;
    if k < 0 {
        return vm.value_error("number of bits must be non-negative");
    }
    if k == 0 {
        return Ok(Value::int(0));
    }
    Ok(Value::Int(vm.random[id].getrandbits(k as u64)))
}

fn index(vm: &mut Vm, value: &Value) -> PyResult<Int> {
    match value {
        Value::Int(i) => Ok(i.clone()),
        Value::Bool(b) => Ok(Int::Small(*b as i64)),
        other => {
            let name = vm.type_name(other);
            let message = alloc::format!("'{}' object cannot be interpreted as an integer", name);
            vm.type_error(&message)
        }
    }
}

fn randrange_values(vm: &mut Vm, id: usize, start: &Value, stop: &Value, step: &Value) -> PyResult<Value> {
    let istart = index(vm, start)?;
    if stop.is_none() {
        if !step.is_none() && !matches!(step, Value::Int(Int::Small(1))) {
            return vm.type_error("Missing a non-None stop argument");
        }
        if !istart.is_negative() && !istart.is_zero() {
            return Ok(Value::Int(vm.random[id].below(&istart)));
        }
        return vm.value_error("empty range for randrange()");
    }
    let istop = index(vm, stop)?;
    let width = istop.sub(&istart);
    let istep = if step.is_none() { Int::Small(1) } else { index(vm, step)? };
    if istep == Int::Small(1) {
        if !width.is_negative() && !width.is_zero() {
            return Ok(Value::Int(istart.add(&vm.random[id].below(&width))));
        }
        let (a, b) = (vm.repr(start)?, vm.repr(stop)?);
        let message = alloc::format!("empty range in randrange({}, {})", a, b);
        return vm.value_error(&message);
    }
    let n = if istep.is_zero() {
        return vm.value_error("zero step for randrange()");
    } else if !istep.is_negative() {
        width.add(&istep).sub(&Int::Small(1)).divmod(&istep).map(|(q, _)| q).unwrap_or(Int::zero())
    } else {
        width.add(&istep).add(&Int::Small(1)).divmod(&istep).map(|(q, _)| q).unwrap_or(Int::zero())
    };
    if n.is_negative() || n.is_zero() {
        let (a, b, c) = (vm.repr(start)?, vm.repr(stop)?, vm.repr(step)?);
        let message = alloc::format!("empty range in randrange({}, {}, {})", a, b, c);
        return vm.value_error(&message);
    }
    let r = vm.random[id].below(&n);
    Ok(Value::Int(istart.add(&istep.mul(&r).unwrap_or(Int::zero()))))
}

fn randrange(vm: &mut Vm, a: &[Value], kw: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let id = rng(vm, a)?;
    let rest = &a[1..];
    let Some(start) = args::get(rest, kw, 0, "start").cloned() else {
        return vm.type_error("randrange() missing 1 required positional argument: 'start'");
    };
    let stop = args::get(rest, kw, 1, "stop").cloned().unwrap_or(Value::None);
    let step = args::get(rest, kw, 2, "step").cloned().unwrap_or(Value::None);
    randrange_values(vm, id, &start, &stop, &step)
}

fn randint(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let id = rng(vm, a)?;
    let (Some(low), Some(high)) = (a.get(1), a.get(2)) else {
        return vm.type_error("randint() missing 2 required positional arguments: 'a' and 'b'");
    };
    let high = index(vm, high)?.add(&Int::Small(1));
    randrange_values(vm, id, low, &Value::Int(high), &Value::None)
}

fn choice(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let id = rng(vm, a)?;
    let Some(seq) = a.get(1) else {
        return vm.type_error("choice() missing 1 required positional argument: 'seq'");
    };
    let n = vm.len(seq)?;
    if n == 0 {
        return vm.index_error("Cannot choose from an empty sequence");
    }
    let i = vm.random[id].below_usize(n);
    vm.get_item(seq, &Value::int(i as i64))
}

fn shuffle(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let id = rng(vm, a)?;
    let Some(x) = a.get(1).cloned() else {
        return vm.type_error("shuffle() missing 1 required positional argument: 'x'");
    };
    let n = vm.len(&x)?;
    for i in (1..n).rev() {
        let j = vm.random[id].below_usize(i + 1);
        let (vi, vj) = (vm.get_item(&x, &Value::int(i as i64))?, vm.get_item(&x, &Value::int(j as i64))?);
        vm.set_item(&x, &Value::int(i as i64), vj)?;
        vm.set_item(&x, &Value::int(j as i64), vi)?;
    }
    Ok(Value::None)
}

fn sample(vm: &mut Vm, a: &[Value], kw: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let id = rng(vm, a)?;
    let rest = &a[1..];
    let Some(population) = args::get(rest, kw, 0, "population").cloned() else {
        return vm.type_error("sample() missing 2 required positional arguments: 'population' and 'k'");
    };
    if matches!(population, Value::Set(_) | Value::FrozenSet(_) | Value::Dict(_)) {
        return vm.type_error("Population must be a sequence.  For dicts or sets, use sorted(d).");
    }
    let k = match args::get(rest, kw, 1, "k") {
        Some(v) => args::int(vm, v)?,
        None => return vm.type_error("sample() missing 1 required positional argument: 'k'"),
    };
    let items = vm.collect(&population)?;
    let n = items.len();
    if k < 0 || k as usize > n {
        return vm.value_error("Sample larger than population or is negative");
    }
    let k = k as usize;
    let mut setsize = 21usize;
    if k > 5 {
        // 4 ** ceil(log4(3k)): 3k is never a power of 4.
        let mut e = 0;
        let mut p = 1usize;
        while p < k * 3 {
            p *= 4;
            e += 1;
        }
        let _ = e;
        setsize += p;
    }
    let mut result = Vec::with_capacity(k);
    if n <= setsize {
        let mut pool = items.clone();
        for i in 0..k {
            let j = vm.random[id].below_usize(n - i);
            result.push(pool[j].clone());
            pool[j] = pool[n - i - 1].clone();
        }
    } else {
        let mut selected: Vec<usize> = Vec::new();
        for _ in 0..k {
            let mut j = vm.random[id].below_usize(n);
            while selected.contains(&j) {
                j = vm.random[id].below_usize(n);
            }
            selected.push(j);
            result.push(items[j].clone());
        }
    }
    Ok(Value::list(result))
}

fn choices(vm: &mut Vm, a: &[Value], kw: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let id = rng(vm, a)?;
    let rest = &a[1..];
    let Some(population) = args::get(rest, kw, 0, "population").cloned() else {
        return vm.type_error("choices() missing 1 required positional argument: 'population'");
    };
    let weights = args::get(rest, kw, 1, "weights").cloned().unwrap_or(Value::None);
    let cum = args::keyword(kw, "cum_weights").cloned().unwrap_or(Value::None);
    let k = match args::keyword(kw, "k") {
        Some(v) => args::int(vm, v)?.max(0) as usize,
        None => 1,
    };
    let items = vm.collect(&population)?;
    let n = items.len();
    let mut out = Vec::with_capacity(k);
    if cum.is_none() && weights.is_none() {
        let nf = Num::from_u64(n as u64);
        for _ in 0..k {
            let i = vm.random[id].random().mul(nf).floor().to_i64_exact().unwrap_or(0) as usize;
            match items.get(i) {
                Some(v) => out.push(v.clone()),
                None => return vm.index_error("list index out of range"),
            }
        }
        return Ok(Value::list(out));
    }
    if !cum.is_none() && !weights.is_none() {
        return vm.type_error("Cannot specify both weights and cumulative weights");
    }
    let cumulative: Vec<Value> = if cum.is_none() {
        let ws = vm.collect(&weights)?;
        let mut acc: Option<Value> = None;
        let mut out = Vec::with_capacity(ws.len());
        for w in ws {
            let next = match &acc {
                None => w,
                Some(previous) => vm.binary(BinOp::Add, previous, &w, false)?,
            };
            out.push(next.clone());
            acc = Some(next);
        }
        out
    } else {
        vm.collect(&cum)?
    };
    if cumulative.len() != n {
        return vm.value_error("The number of weights does not match the population");
    }
    let Some(last) = cumulative.last() else {
        return vm.index_error("list index out of range");
    };
    let total = float_of(vm, last)?;
    if total.is_zero() || total.is_negative() {
        return vm.value_error("Total of weights must be greater than zero");
    }
    if !total.is_finite() {
        return vm.value_error("Total of weights must be finite");
    }
    let mut cumf = Vec::with_capacity(n);
    for c in &cumulative {
        cumf.push(float_of(vm, c)?);
    }
    let hi = n - 1;
    for _ in 0..k {
        let x = vm.random[id].random().mul(total);
        // bisect_right(cum_weights, x, 0, hi)
        let (mut lo, mut high) = (0usize, hi);
        while lo < high {
            let mid = (lo + high) / 2;
            if x.lt(cumf[mid]) {
                high = mid;
            } else {
                lo = mid + 1;
            }
        }
        out.push(items[lo].clone());
    }
    Ok(Value::list(out))
}

fn uniform(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let id = rng(vm, a)?;
    let low = float_of(vm, a.get(1).unwrap_or(&Value::None))?;
    let high = float_of(vm, a.get(2).unwrap_or(&Value::None))?;
    let r = vm.random[id].random();
    Ok(Value::Float(low.add(high.sub(low).mul(r))))
}

fn triangular(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let id = rng(vm, a)?;
    let mut low = match a.get(1) {
        Some(v) => float_of(vm, v)?,
        None => Num::ZERO,
    };
    let mut high = match a.get(2) {
        Some(v) => float_of(vm, v)?,
        None => Num::ONE,
    };
    let mut c = match a.get(3) {
        None | Some(Value::None) => Num::HALF,
        Some(mode) => {
            let mode = float_of(vm, mode)?;
            if high.equals(low) {
                return Ok(Value::Float(low));
            }
            mode.sub(low).div(high.sub(low))
        }
    };
    let mut u = vm.random[id].random();
    if c.lt(u) {
        u = Num::ONE.sub(u);
        c = Num::ONE.sub(c);
        core::mem::swap(&mut low, &mut high);
    }
    Ok(Value::Float(low.add(high.sub(low).mul(u.mul(c).sqrt()))))
}

fn gauss(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let id = rng(vm, a)?;
    let mu = match a.get(1) {
        Some(v) => float_of(vm, v)?,
        None => Num::ZERO,
    };
    let sigma = match a.get(2) {
        Some(v) => float_of(vm, v)?,
        None => Num::ONE,
    };
    let z = match vm.random[id].gauss_next.take() {
        Some(z) => z,
        None => {
            let x2pi = vm.random[id].random().mul(Num::PI.mul(Num::TWO));
            let g2rad = Num::TWO.neg().mul(Num::ONE.sub(vm.random[id].random()).ln()).sqrt();
            vm.random[id].gauss_next = Some(x2pi.sin().mul(g2rad));
            x2pi.cos().mul(g2rad)
        }
    };
    Ok(Value::Float(mu.add(z.mul(sigma))))
}

fn normalvariate(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let id = rng(vm, a)?;
    let mu = match a.get(1) {
        Some(v) => float_of(vm, v)?,
        None => Num::ZERO,
    };
    let sigma = match a.get(2) {
        Some(v) => float_of(vm, v)?,
        None => Num::ONE,
    };
    // NV_MAGICCONST = 4 * exp(-0.5) / sqrt(2.0)
    let magic = Num::from_i64(4).mul(Num::HALF.neg().exp()).div(Num::TWO.sqrt());
    let z = loop {
        let u1 = vm.random[id].random();
        let u2 = Num::ONE.sub(vm.random[id].random());
        let z = magic.mul(u1.sub(Num::HALF)).div(u2);
        let zz = z.mul(z).div(Num::from_i64(4));
        if !u2.ln().neg().lt(zz) {
            break z;
        }
    };
    Ok(Value::Float(mu.add(z.mul(sigma))))
}

fn expovariate(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let id = rng(vm, a)?;
    let lambd = match a.get(1) {
        Some(v) => float_of(vm, v)?,
        None => Num::ONE,
    };
    let r = vm.random[id].random();
    Ok(Value::Float(Num::ONE.sub(r).ln().neg().div(lambd)))
}

fn betavariate_unsupported(vm: &mut Vm, _: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let class = vm.types.not_implemented_error.clone();
    Err(vm.error(&class, "fonction de random non prise en charge"))
}

pub fn module(vm: &mut Vm) -> Rc<Module> {
    let object = vm.types.object.clone();
    let random_class = class("Random", alloc::vec![object], Base::Object);
    random_class.dict.borrow_mut().set_str("__module__", Value::str("random"));
    let methods: &[(&'static str, NativeFn)] = &[
        ("__init__", init), ("seed", seed), ("random", random), ("getrandbits", getrandbits), ("randrange", randrange),
        ("randint", randint), ("choice", choice), ("shuffle", shuffle), ("sample", sample), ("choices", choices),
        ("uniform", uniform), ("triangular", triangular), ("gauss", gauss), ("normalvariate", normalvariate),
        ("expovariate", expovariate), ("betavariate", betavariate_unsupported), ("gammavariate", betavariate_unsupported),
    ];
    for (name, f) in methods {
        method(&random_class, name, *f);
    }
    let module = super::native_module(vm, "random", &[]);
    // The hidden instance: generator 0, the VM's own.
    let instance = Instance::new(random_class.clone());
    instance.dict.borrow_mut().set_str("_generator", Value::int(0));
    let instance = Value::Instance(instance);
    for (name, _) in methods.iter().skip(1) {
        if let Ok(bound) = vm.get_attr(&instance, name) {
            super::set(&module, name, bound);
        }
    }
    super::set(&module, "Random", Value::Class(random_class));
    super::set(&module, "_inst", instance);
    module
}
