//! Self-test of the JavaScript engine (16.1.3): a corpus in the manner
//! of test262, one expectation per case.
//!
//! Each case is a function body; its return value, turned into a string,
//! must equal the expected text exactly. A case that returns a promise is
//! awaited: the job queue runs, then the timers, the clock jumping to the
//! next one, until the promise settles -- so `setTimeout` and `await`
//! are tested as scripts use them, without waiting for real.
//!
//! The cases are what a page or a Node script leans on: the syntax of
//! ES2022, the standard library, the order of jobs, and the bounds that
//! keep a hostile script from taking the machine down (fuel, stack).
//! Every case runs in its own realm: one case cannot repair another.

use alloc::string::String;
use alloc::vec::Vec;

use super::interp::{Ctrl, Ctx, Host};
use super::value::*;

/// A host with no network, no storage, a fixed clock and a fixed seed:
/// the same answers on every machine.
pub struct TestHost {
    pub now: i64,
    pub seed: u64,
    pub lines: Vec<String>,
    pub offset_minutes: i32,
}

impl TestHost {
    pub fn new() -> TestHost {
        // 2026-10-28T12:00:00Z
        TestHost { now: 1_793_188_800_000, seed: 0x9E37_79B9_7F4A_7C15, lines: Vec::new(), offset_minutes: 60 }
    }
}

impl Default for TestHost {
    fn default() -> Self {
        Self::new()
    }
}

impl Host for TestHost {
    fn now_ms(&mut self) -> i64 {
        self.now
    }

    fn tz_offset_minutes(&mut self) -> i32 {
        self.offset_minutes
    }

    fn random(&mut self) -> u64 {
        // xorshift64*
        self.seed ^= self.seed >> 12;
        self.seed ^= self.seed << 25;
        self.seed ^= self.seed >> 27;
        self.seed.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn fetch(
        &mut self,
        _method: &str,
        _url: &str,
        _headers: &[(String, String)],
        _body: Option<Vec<u8>>,
    ) -> Result<(u16, String, Vec<(String, String)>, Vec<u8>, String), String> {
        Err(String::from("pas de reseau dans l'autotest"))
    }

    fn cookies(&mut self, _url: &str) -> String {
        String::new()
    }

    fn set_cookie(&mut self, _url: &str, _line: &str) {}

    fn storage(&mut self, _origin: &str, _session: bool) -> Vec<(String, String)> {
        Vec::new()
    }

    fn set_storage(&mut self, _origin: &str, _session: bool, _items: &[(String, String)]) {}

    fn navigate(&mut self, _url: &str, _replace: bool) {}

    fn memory_ok(&mut self) -> bool {
        crate::heap::room_for(0)
    }

    fn can_allocate(&mut self, bytes: usize) -> bool {
        crate::heap::room_for(bytes)
    }

    fn log(&mut self, line: &str) {
        if self.lines.len() < 200 {
            self.lines.push(String::from(line));
        }
    }
}

/// Fuel of one case: enough for every legitimate case, little enough
/// that the runaway ones end fast.
const FUEL: u64 = 2_000_000;

/// Runs one case body; the outcome as text, or why it failed.
pub fn run_case(body: &str, headroom: &dyn Fn() -> usize) -> Result<String, String> {
    let mut realm = super::builtins::new_realm();
    let mut host = TestHost::new();
    let mut doc = crate::web::html::parse("");
    let outcome = {
        let mut ctx = Ctx::new(&mut realm, &mut host, &mut doc, headroom, FUEL, String::from("about:test"), (800, 600));
        super::builtins::install(&mut ctx);
        let source = alloc::format!("(function() {{\n{}\n}})()", body);
        let value = ctx.run_script(&source);
        match value {
            Err(error) => Err(error),
            Ok(value) => settle(&mut ctx, value),
        }
    };
    realm.teardown();
    outcome
}

/// The value as text; a promise is awaited first.
fn settle(ctx: &mut Ctx, value: Value) -> Result<String, String> {
    ctx.run_jobs()?;
    let promise = match &value {
        Value::Obj(object) if matches!(object.borrow().kind, Kind::Promise(_)) => Some(object.clone()),
        _ => None,
    };
    let value = match promise {
        None => value,
        Some(promise) => {
            let mut rounds = 0;
            loop {
                let state = match &promise.borrow().kind {
                    Kind::Promise(data) => (data.state, data.value.clone()),
                    _ => (PromiseState::Fulfilled, Value::Undefined),
                };
                match state {
                    (PromiseState::Fulfilled, value) => break value,
                    (PromiseState::Rejected, reason) => {
                        if let Kind::Promise(data) = &mut promise.borrow_mut().kind {
                            data.handled = true;
                        }
                        return Err(alloc::format!("rejetee : {}", ctx.describe_error(&reason)));
                    }
                    (PromiseState::Pending, _) => {}
                }
                rounds += 1;
                if rounds > 1000 {
                    return Err(String::from("la promesse ne se regle pas"));
                }
                // Jump the clock to the next timer.
                let next = ctx.realm.timers.iter().map(|t| t.due).min();
                match next {
                    Some(due) => {
                        let now = ctx.now();
                        if due > now {
                            ctx.realm.clock_skew += due - now;
                        }
                        let now = ctx.now();
                        ctx.run_timers(now)?;
                    }
                    None => return Err(String::from("promesse en attente, aucune minuterie")),
                }
                ctx.run_jobs()?;
            }
        }
    };
    Ok(match &value {
        Value::Str(s) => String::from(s.as_str()),
        other => super::ops::to_display(other),
    })
}

/// (name, body, expected).
pub const CASES: &[(&str, &str, &str)] = &[
    // --- syntax ---
    ("let/const et portee de bloc", "let a = 1; { let a = 2; } const b = a + 1; return a + ',' + b;", "1,2"),
    ("fermeture dans une boucle", "const f = []; for (let i = 0; i < 3; i++) f.push(() => i); return f.map(g => g()).join();", "0,1,2"),
    ("decomposition", "const {a, b: [c, d = 4], ...r} = {a: 1, b: [3], x: 5, y: 6}; return [a, c, d, Object.keys(r)].join('|');", "1|3|4|x,y"),
    ("etalement", "const a = [1, 2]; const o = {...{x: 1}, y: 2}; return [...a, ...'ab'].join() + JSON.stringify(o);", "1,2,a,b{\"x\":1,\"y\":2}"),
    ("parametres par defaut et reste", "function f(a, b = a * 2, ...r) { return [a, b, r.length]; } return f(3).join() + ';' + f(1, 1, 1, 1).join();", "3,6,0;1,1,2"),
    ("gabarits et balises", "const t = (s, ...v) => s.raw.join('/') + v.join('+'); return `a${1 + 1}b` + t`x${1}y${2}z`;", "a2bx/y/z1+2"),
    ("chainage optionnel et ??", "const o = {a: {b: null}}; return [o?.a?.b?.c, o.x?.y, o.a.b ?? 'n', 0 ?? 1, o.f?.()].join();", ",,n,0,"),
    ("affectations logiques", "let a = null, b = 0, c = 1; a ??= 5; b ||= 6; c &&= 7; return [a, b, c].join();", "5,6,7"),
    ("classes, heritage, super", "class A { constructor(x) { this.x = x; } get d() { return this.x * 2; } static s() { return 'S'; } m() { return 'A'; } } class B extends A { m() { return super.m() + 'B'; } } const b = new B(4); return [b.d, b.m(), B.s(), b instanceof A].join();", "8,AB,S,true"),
    ("champs prives et statiques", "class C { #n = 1; static #c = 0; static count() { return C.#c; } inc() { C.#c++; return ++this.#n; } has(o) { return #n in o; } } const c = new C(); c.inc(); return [c.inc(), C.count(), c.has(c), c.has({})].join();", "3,2,true,false"),
    ("accesseurs d'objet", "const o = { _v: 1, get v() { return this._v; }, set v(x) { this._v = x * 10; } }; o.v = 2; return String(o.v);", "20"),
    ("generateurs", "function* g() { yield 1; yield* [2, 3]; return 4; } return [...g()].join() + ';' + Array.from(g()).length;", "1,2,3;3"),
    ("iterateur personnalise", "const it = { from: 1, to: 3, [Symbol.iterator]() { let i = this.from, t = this.to; return { next: () => i <= t ? {value: i++, done: false} : {value: undefined, done: true} }; } }; return [...it].join();", "1,2,3"),
    ("etiquettes et break", "let s = ''; outer: for (let i = 0; i < 3; i++) { for (let j = 0; j < 3; j++) { if (j == 1) continue outer; if (i == 2) break outer; s += i + '' + j + ' '; } } return s.trim();", "00 10"),
    ("exceptions et finally", "const log = []; function f() { try { throw new TypeError('x'); } catch (e) { log.push(e.name, e.message); return 'c'; } finally { log.push('f'); } } log.push(f()); return log.join();", "TypeError,x,f,c"),
    ("switch avec chute", "function f(x) { let r = ''; switch (x) { case 1: r += 'a'; case 2: r += 'b'; break; default: r += 'd'; } return r; } return [f(1), f(2), f(3)].join();", "ab,b,d"),
    ("typeof et operateurs", "return [typeof null, typeof undefined, typeof (() => 1), 2 ** 10, 7 % -3, -7 % 3, '3' * '4', 1 + '2', [] + {}].join();", "object,undefined,function,1024,1,-1,12,12,[object Object]"),
    ("egalites", "return [null == undefined, null === undefined, NaN == NaN, '1' == 1, 0 == '', [1] == 1, Object.is(-0, 0), Object.is(NaN, NaN)].join();", "true,false,false,true,true,true,false,true"),
    ("arguments et call/apply/bind", "function f() { return this.k + arguments.length; } const o = {k: 'k'}; const g = f.bind(o, 1); return [f.call(o, 1, 2), f.apply(o, [1]), g(2, 3), g.name].join();", "k2,k1,k3,bound f"),
    ("fonctions flechees et this", "const o = { v: 1, f() { return [1, 2].map(x => x + this.v); } }; return o.f().join();", "2,3"),
    ("hoisting", "return typeof h + (function () { return x; var x = 1; })() + h(); function h() { return 'H'; }", "functionundefinedH"),
    ("zone morte temporelle", "try { x; let x = 1; return 'non'; } catch (e) { return e.name; }", "ReferenceError"),
    ("eval et new Function", "const add = new Function('a', 'b', 'return a + b'); return add(2, 3) + eval('1 + 1');", "7"),
    ("virgules et void", "let a = (1, 2, 3); return [a, void 0].join();", "3,"),
    ("nombres litteraux", "return [0x1F, 0o17, 0b101, 1_000_000, .5, 5e-7, 1e21, 2e-7].join();", "31,15,5,1000000,0.5,5e-7,1e+21,2e-7"),
    // --- numbers ---
    ("arithmetique flottante exacte", "return [0.1 + 0.2, 0.1 * 3, 1 / 3, 9007199254740993, 2 ** 53 + 1, 1e300 * 1e10, -1 / 0].join();", "0.30000000000000004,0.30000000000000004,0.3333333333333333,9007199254740992,9007199254740992,Infinity,-Infinity"),
    ("toFixed, toPrecision, toExponential", "return [(1.005).toFixed(2), (1.45).toFixed(1), (123.456).toPrecision(4), (0.00001).toPrecision(1), (123456).toExponential(2), (0).toFixed(2), (1e21).toFixed(2)].join();", "1.00,1.4,123.5,0.00001,1.23e+5,0.00,1e+21"),
    ("toString en base", "return [(255).toString(16), (255).toString(2), (-10).toString(36), (0.5).toString(2)].join();", "ff,11111111,-a,0.1"),
    ("parseInt, parseFloat, Number", "return [parseInt('  42px'), parseInt('0x1f'), parseInt('z', 36), parseFloat('3.14abc'), Number(''), Number('12e2'), Number('1_0'), Number(null), parseInt('abc')].join();", "42,31,35,3.14,0,1200,NaN,0,NaN"),
    ("Number statiques", "return [Number.isInteger(5.0), Number.isSafeInteger(2 ** 53), Number.EPSILON > 0, Number.MAX_SAFE_INTEGER, Number.isNaN('x'), isNaN('x')].join();", "true,false,true,9007199254740991,false,true"),
    ("toLocaleString des nombres", "return [(1234567.891).toLocaleString(), (-1000).toLocaleString(), (0.5).toLocaleString()].join(' ');", "1,234,567.891 -1,000 0.5"),
    ("Math", "return [Math.max(1, 3, 2), Math.min(), Math.round(2.5), Math.round(-2.5), Math.sign(-3), Math.trunc(-4.7), Math.hypot(3, 4), Math.cbrt(27), Math.log10(1000), Math.log2(8), Math.clz32(1), Math.imul(3, 4), Math.fround(5.5), Math.abs(-0)].join();", "3,Infinity,3,-2,-1,-4,5,3,3,3,31,12,5.5,0"),
    ("Math.random dans [0, 1)", "for (let i = 0; i < 1000; i++) { const r = Math.random(); if (!(r >= 0 && r < 1)) return 'hors bornes'; } return 'ok';", "ok"),
    ("fonctions transcendantes", "return [Math.sqrt(2), Math.sin(0), Math.cos(Math.PI), Math.exp(1) === Math.E, Math.atan2(1, 1) * 4].join();", "1.4142135623730951,0,-1,true,3.141592653589793"),
    // --- strings ---
    ("methodes de chaines", "const s = 'Bonjour le monde'; return [s.length, s.indexOf('o'), s.lastIndexOf('o'), s.slice(-5), s.substring(3, 0), s.toUpperCase(), s.split(' ').length, s.at(-1), s.includes('le'), s.startsWith('Bon'), s.padStart(18, '*').slice(0, 3), 'ab'.repeat(3), '  x '.trim() + '|'].join();", "16,1,12,monde,Bon,BONJOUR LE MONDE,3,e,true,true,**B,ababab,x|"),
    ("UTF-16 et unicode", "const s = 'a\\u00e9\\u{1F600}b'; return [s.length, s.charCodeAt(1), s.codePointAt(2), [...s].length, String.fromCodePoint(0x1F600).length, s.normalize === undefined].join();", "5,233,128512,4,2,false"),
    ("replace et replaceAll texte", "return ['aXbXc'.replace('X', '-'), 'aXbXc'.replaceAll('X', '-'), 'abc'.replace('b', '[$&$`$\\']'), 'x'.replace('x', () => '$1')].join();", "a-bXc,a-b-c,a[bac]c,$1"),
    ("localeCompare et tri", "return ['b', 'a', 'C'].sort().join() + ';' + ['b', 'a', 'C'].sort((x, y) => x.localeCompare(y)).join();", "C,a,b;a,b,C"),
    // --- regular expressions ---
    ("exec et groupes", "const m = /(\\d+)-(?<m>\\d+)/.exec('date: 2026-10'); return [m[0], m[1], m.groups.m, m.index].join();", "2026-10,2026,10,6"),
    ("remplacement par motif", "return ['2026-10-28'.replace(/(\\d+)-(\\d+)-(\\d+)/, '$3/$2/$1'), 'aaa'.replace(/a/g, 'b'), 'John Smith'.replace(/(?<f>\\w+) (?<l>\\w+)/, '$<l>, $<f>'), 'x1y22'.replace(/\\d+/g, d => d.length)].join();", "28/10/2026,bbb,Smith, John,x1y2"),
    ("split par expression", "return ['a1b22c'.split(/\\d+/).join('|'), 'abc'.split('').join('|'), 'a, b,c'.split(/\\s*,\\s*/).join('|'), 'a1b'.split(/(\\d)/).join('|'), ''.split(/x/).length].join(';');", "a|b|c;a|b|c;a|b|c;a|1|b;1"),
    ("match, matchAll, search", "const s = 'a1b2c3'; return [s.match(/\\d/g).join(''), [...s.matchAll(/([a-z])(\\d)/g)].map(m => m[1] + m.index).join(''), s.search(/c/), s.match(/z/)].join();", "123,a0b2c4,4,"),
    ("drapeaux et lastIndex", "const r = /o/g; const s = 'foo'; const a = [r.test(s), r.lastIndex, r.test(s), r.lastIndex, r.test(s), r.lastIndex]; const y = /a/y; y.lastIndex = 1; return a.join() + ';' + y.test('ba') + ';' + /x/gimsuy.flags + ';' + String(/a\\/b/);", "true,2,true,3,false,0;true;gimsuy;/a\\/b/"),
    ("classes, ancres, assertions", "return [/^\\w+@\\w+\\.\\w{2,}$/.test('a@b.fr'), /(?<=\\$)\\d+/.exec('cout $42')[0], /a(?!b)/.exec('abac').index, /\\bmot\\b/i.test('un MOT ici'), /[^aeiou]+/.exec('aabcde')[0]].join();", "true,42,2,true,bcd"),
    ("expression catastrophique bornee", "return String(/(a+)+b/.test('a'.repeat(40)));", "false"),
    // --- arrays ---
    ("methodes de tableaux", "const a = [3, 1, 2]; return [a.map(x => x * 2).join(), a.filter(x => x > 1).join(), a.reduce((s, x) => s + x, 0), a.find(x => x < 3), a.findLastIndex(x => x > 0), a.some(x => x > 2), a.every(x => x > 0), [...a].sort().join(), a.indexOf(2), a.includes(NaN), [NaN].includes(NaN)].join('|');", "6,2,4|3,2|6|1|2|true|true|1,2,3|2|false|true"),
    ("splice, flat, fill, from", "const a = [1, 2, 3, 4]; const r = a.splice(1, 2, 'x'); return [a.join(), r.join(), [1, [2, [3, [4]]]].flat(2).join(), new Array(3).fill(0).join(), Array.from({length: 3}, (_, i) => i * i).join(), Array.of(7).length].join('|');", "1,x,4|2,3|1,2,3,4|0,0,0|0,1,4|1"),
    ("tri stable et comparateur", "const a = [{k: 1, v: 'a'}, {k: 0, v: 'b'}, {k: 1, v: 'c'}, {k: 0, v: 'd'}]; a.sort((x, y) => x.k - y.k); return a.map(x => x.v).join('');", "bdac"),
    ("toSorted, with, at, copie", "const a = [3, 1, 2]; return [a.toSorted().join(), a.toReversed().join(), a.with(0, 9).join(), a.at(-1), a.join()].join('|');", "1,2,3|2,1,3|9,1,2|2|3,1,2"),
    ("longueur des tableaux", "const a = [1, 2, 3]; a[5] = 6; return [a.length, a[4], (a.length = 2, a.join()), Object.keys(a).join()].join('|');", "6||1,2|0,1"),
    ("Object.groupBy et Map.groupBy", "const g = Object.groupBy([1, 2, 3, 4], x => x % 2 ? 'impair' : 'pair'); const m = Map.groupBy([1, 2, 3], x => x > 1); return JSON.stringify(g) + m.get(true).join();", "{\"impair\":[1,3],\"pair\":[2,4]}2,3"),
    // --- objects ---
    ("Object.keys, entries, assign, freeze", "const o = Object.freeze({b: 1, a: 2, 1: 'x'}); o.c = 3; return [Object.keys(o).join(), Object.entries({x: 1}).join(), JSON.stringify(Object.assign({}, {a: 1}, null, {b: 2})), Object.isFrozen(o), 'c' in o].join('|');", "1,b,a|x,1|{\"a\":1,\"b\":2}|true|false"),
    ("defineProperty et descripteurs", "const o = {}; Object.defineProperty(o, 'x', { value: 1, enumerable: false }); o.x = 2; const d = Object.getOwnPropertyDescriptor(o, 'x'); return [o.x, Object.keys(o).length, d.writable, d.configurable].join();", "1,0,false,false"),
    ("prototypes", "const p = { hi() { return 'hi ' + this.n; } }; const o = Object.create(p, { n: { value: 'o' } }); return [o.hi(), Object.getPrototypeOf(o) === p, p.isPrototypeOf(o), Object.hasOwn(o, 'hi')].join();", "hi o,true,true,false"),
    ("Object.fromEntries et toString", "return [JSON.stringify(Object.fromEntries([['a', 1], ['b', 2]])), Object.prototype.toString.call([]), Object.prototype.toString.call(null), String({}), `${[1, [2, 3]]}`].join('|');", "{\"a\":1,\"b\":2}|[object Array]|[object Null]|[object Object]|1,2,3"),
    ("Reflect", "const o = {a: 1}; return [Reflect.has(o, 'a'), Reflect.ownKeys(o).join(), Reflect.get(o, 'a'), Reflect.apply(Math.max, null, [1, 5])].join();", "true,a,1,5"),
    // --- symbols ---
    ("Symbol", "const s = Symbol('x'); const o = {[s]: 1, y: 2}; return [typeof s, s.description, s.toString(), Object.keys(o).length, o[s], Symbol.for('k') === Symbol.for('k'), Symbol.keyFor(Symbol.for('k')), Symbol() === Symbol(), Symbol().description].join();", "symbol,x,Symbol(x),1,1,true,k,false,"),
    ("Symbol.toPrimitive et toStringTag", "const o = { [Symbol.toPrimitive](h) { return h === 'number' ? 42 : 'str'; }, get [Symbol.toStringTag]() { return 'Moi'; } }; return [+o, `${o}`, Object.prototype.toString.call(o)].join();", "42,str,[object Moi]"),
    // --- JSON ---
    ("JSON.stringify", "return [JSON.stringify({a: [1, 'x', null, undefined, () => 1], b: undefined, c: new Date(0), d: NaN, e: 'q\"\\n'}), JSON.stringify('\\u0001'), JSON.stringify(undefined), JSON.stringify([undefined])].join('|');", "{\"a\":[1,\"x\",null,null,null],\"c\":\"1970-01-01T00:00:00.000Z\",\"d\":null,\"e\":\"q\\\"\\n\"}|\"\\u0001\"||[null]"),
    ("JSON.stringify indente et filtre", "return JSON.stringify({a: 1, b: [1, 2], c: {}}, null, 2) + '|' + JSON.stringify({a: 1, b: 2, c: 3}, ['c', 'a']) + '|' + JSON.stringify({a: 1, b: 'x'}, (k, v) => typeof v === 'number' ? v * 10 : v);", "{\n  \"a\": 1,\n  \"b\": [\n    1,\n    2\n  ],\n  \"c\": {}\n}|{\"c\":3,\"a\":1}|{\"a\":10,\"b\":\"x\"}"),
    ("JSON.parse et reviver", "const o = JSON.parse('{\"a\":[1,2.5e1,-0.5],\"b\":{\"c\":true},\"a\":7}'); const r = JSON.parse('[1,2,3]', (k, v) => typeof v === 'number' ? v + 1 : v); return [o.a, o.b.c, Object.keys(o).join(), r.join(), JSON.parse('\"\\\\u00e9\"')].join('|');", "7|true|a,b|2,3,4|\u{e9}"),
    ("JSON erreurs", "const out = []; try { JSON.parse('{a:1}'); } catch (e) { out.push(e.name); } const c = {}; c.c = c; try { JSON.stringify(c); } catch (e) { out.push(e.name + ':' + e.message.startsWith('Converting circular structure to JSON')); } return out.join();", "SyntaxError,TypeError:true"),
    ("toJSON", "const o = { toJSON(k) { return 'T' + k; } }; return JSON.stringify({x: o, y: [o]});", "{\"x\":\"Tx\",\"y\":[\"T0\"]}"),
    // --- dates ---
    ("Date UTC et ISO", "const d = new Date(Date.UTC(2026, 9, 28, 13, 5, 3, 7)); return [d.toISOString(), d.getUTCDay(), d.getTime(), new Date(0).toISOString(), new Date(-1).toISOString()].join('|');", "2026-10-28T13:05:03.007Z|3|1793192703007|1970-01-01T00:00:00.000Z|1969-12-31T23:59:59.999Z"),
    ("Date analyse", "return [Date.parse('2026-10-28'), Date.parse('2026-10-28T10:00:00Z'), Date.parse('2026-10-28T11:00:00+01:00'), Date.parse('2026-10-28T11:00'), isNaN(Date.parse('pas une date')), Date.parse('Wed, 28 Oct 2026 10:00:00 GMT'), Date.parse('Oct 28, 2026')].join('|');", "1793145600000|1793181600000|1793181600000|1793181600000|true|1793181600000|1793142000000"),
    ("Date heure locale (UTC+1)", "const d = new Date(2026, 0, 31, 23, 30); d.setMonth(1); return [d.getMonth(), d.getDate(), d.getHours(), d.getTimezoneOffset(), new Date(2026, 9, 28, 14).toISOString(), String(new Date(Date.UTC(2026, 9, 28, 12))).slice(0, 33)].join('|');", "2|3|23|-60|2026-10-28T13:00:00.000Z|Wed Oct 28 2026 13:00:00 GMT+0100"),
    ("Date maintenant et invalide", "const d = new Date('x'); return [Date.now(), new Date().getUTCHours(), String(d), isNaN(d), JSON.stringify({d}), new Date(8.64e15 + 1).getTime()].join('|');", "1793188800000|12|Invalid Date|true|{\"d\":null}|NaN"),
    ("Date setters et debordements", "const d = new Date(Date.UTC(2026, 11, 31)); d.setUTCDate(d.getUTCDate() + 1); const e = new Date(Date.UTC(2024, 1, 29)); e.setUTCFullYear(2025); return [d.toISOString().slice(0, 10), e.toISOString().slice(0, 10), new Date(Date.UTC(2026, 13, 1)).getUTCMonth()].join('|');", "2027-01-01|2025-03-01|1"),
    ("Date formes texte", "const d = new Date(Date.UTC(2026, 9, 28, 13, 5, 3)); return [d.toUTCString(), d.toLocaleDateString(), d.toLocaleTimeString(), d.toDateString()].join('|');", "Wed, 28 Oct 2026 13:05:03 GMT|10/28/2026|2:05:03 PM|Wed Oct 28 2026"),
    // --- collections ---
    ("Map", "const m = new Map([[1, 'a'], ['1', 'b']]); m.set(NaN, 'n').set(-0, 'z'); return [m.size, m.get(1), m.get('1'), m.get(NaN), m.get(0), [...m.keys()].join(), m.delete(1), m.has(1), [...m.entries()][0].join()].join('|');", "4|a|b|n|z|1,1,NaN,0|true|false|1,b"),
    ("Set et suppression pendant l'iteration", "const s = new Set([1, 2, 3, 2]); const seen = []; for (const x of s) { seen.push(x); if (x === 1) { s.delete(2); s.add(4); } } return [s.size, seen.join(), [...new Set('abca')].join('')].join('|');", "3|1,3,4|abc"),
    ("algebre des ensembles", "const a = new Set([1, 2, 3]), b = new Set([2, 3, 4]); return [[...a.union(b)].join(), [...a.intersection(b)].join(), [...a.difference(b)].join(), [...a.symmetricDifference(b)].join(), a.isSubsetOf(new Set([1, 2, 3, 4])), a.isDisjointFrom(b)].join('|');", "1,2,3,4|2,3|1|1,4|true|false"),
    ("WeakMap refuse les primitives", "const w = new WeakMap(); const k = {}; w.set(k, 1); try { w.set('x', 1); return 'non'; } catch (e) { return w.get(k) + e.name; }", "1TypeError"),
    ("Map forEach et ordre d'insertion", "const m = new Map(); m.set('b', 1); m.set('a', 2); m.set('b', 3); const out = []; m.forEach((v, k) => out.push(k + v)); return out.join();", "b3,a2"),
    // --- promises, jobs, timers ---
    ("ordre des taches", "const log = []; setTimeout(() => log.push('timeout'), 0); Promise.resolve().then(() => log.push('micro1')).then(() => log.push('micro2')); queueMicrotask(() => log.push('qm')); log.push('sync'); return new Promise(r => setTimeout(() => r(log.join()), 1));", "sync,micro1,qm,micro2,timeout"),
    ("async/await", "async function f(x) { const a = await x; const b = await Promise.resolve(a + 1); return a + b; } return f(10).then(v => 'v' + v);", "v21"),
    ("await d'une minuterie", "const wait = ms => new Promise(r => setTimeout(r, ms)); return (async () => { const t0 = Date.now(); await wait(1000); await wait(500); return Date.now() - t0; })().then(String);", "1500"),
    ("Promise.all et allSettled", "return Promise.all([1, Promise.resolve(2), new Promise(r => setTimeout(() => r(3), 5))]).then(a => Promise.allSettled([Promise.reject(new Error('e')), 4]).then(s => a.join() + '|' + s.map(x => x.status + (x.value ?? x.reason.message)).join()));", "1,2,3|rejectede,fulfilled4"),
    ("Promise.any, race, reject", "return Promise.any([Promise.reject(1), Promise.resolve(2)]).then(v => Promise.race([new Promise(r => setTimeout(() => r('lent'), 10)), 'vite']).then(w => Promise.any([Promise.reject(1)]).catch(e => [v, w, e.constructor.name, e.errors.join()].join())));", "2,vite,AggregateError,1"),
    ("then, catch, finally", "const log = []; return Promise.reject(new Error('x')).then(() => log.push('non')).catch(e => { log.push(e.message); return 5; }).finally(() => log.push('fin')).then(v => log.join() + v);", "x,fin5"),
    ("try/catch autour d'await", "async function f() { try { await Promise.reject(new TypeError('t')); } catch (e) { return 'pris ' + e.name; } } return f();", "pris TypeError"),
    ("setInterval et clearInterval", "let n = 0; return new Promise(r => { const id = setInterval(() => { n++; if (n === 3) { clearInterval(id); r(String(n)); } }, 10); });", "3"),
    ("thenable adopte", "const t = { then(ok) { ok('adopte'); } }; return Promise.resolve(t);", "adopte"),
    ("withResolvers", "const { promise, resolve } = Promise.withResolvers(); setTimeout(() => resolve('pret'), 3); return promise;", "pret"),
    // --- global functions ---
    ("URI", "return [encodeURIComponent('a b&c/\\u00e9'), encodeURI('http://x.fr/a b?q=1#f'), decodeURIComponent('%C3%A9t%C3%A9'), decodeURI('%3F%20')].join('|');", "a%20b%26c%2F%C3%A9|http://x.fr/a%20b?q=1#f|\u{e9}t\u{e9}|%3F "),
    ("URI malforme", "try { decodeURIComponent('%E0%A4%A'); return 'non'; } catch (e) { return e.name; }", "URIError"),
    ("base64", "return [btoa('Nova OS'), atob('Tm92YSBPUw=='), atob(btoa('\\u00ff\\u0000'))].join('|') + '|' + (() => { try { btoa('\\u20ac'); } catch (e) { return e.name; } })();", "Tm92YSBPUw==|Nova OS|\u{ff}\u{0}|InvalidCharacterError"),
    ("structuredClone", "const d = new Date(5); const o = {a: [1, {b: 2}], d, m: new Map([[1, 2]])}; o.self = o; const c = structuredClone(o); return [c !== o, c.a[1].b, c.d.getTime(), c.d !== d, c.m.get(1), c.self === c].join();", "true,2,5,true,2,true"),
    ("console.log et formats", "console.log('%s=%d', 'x', 42.9, {a: 1}); console.count(); console.count(); return 'ok';", "ok"),
    ("globalThis", "globalThis.zz = 3; var vv = 4; return [zz, typeof globalThis.vv, globalThis === this || this === undefined].join();", "3,undefined,true"),
    // --- errors ---
    ("messages d'erreur", "const out = []; try { null.x; } catch (e) { out.push(e instanceof TypeError); } try { undefinedVar; } catch (e) { out.push(e.name); } try { (void 0)(); } catch (e) { out.push(e.name); } try { new Array(-1); } catch (e) { out.push(e.name); } return out.join();", "true,ReferenceError,TypeError,RangeError"),
    ("Error cause et toString", "const e = new Error('haut', { cause: 'bas' }); class MonErreur extends Error { constructor(m) { super(m); this.name = 'MonErreur'; } } const m = new MonErreur('x'); return [e.cause, String(e), String(m), m instanceof Error, m.stack.length > 0].join('|');", "bas|Error: haut|MonErreur: x|true|true"),
    // --- bounds ---
    ("recursion infinie : RangeError rattrapable", "function f() { return f() + 1; } try { f(); return 'non'; } catch (e) { return e.name; }", "RangeError"),
    ("chaine trop longue refusee", "let s = 'x'; try { while (true) s += s; } catch (e) { return e.name + (s.length >= 1 << 20); }", "RangeErrortrue"),
];

/// Runs the corpus. Returns (passed, failed); a failure prints what was
/// expected and what came.
pub fn run(headroom: &dyn Fn() -> usize) -> (usize, usize) {
    let mut passed = 0;
    let mut failed = 0;
    for (name, body, expected) in CASES {
        match run_case(body, headroom) {
            Ok(got) if got == *expected => passed += 1,
            Ok(got) => {
                failed += 1;
                crate::serial_println!("[js] ECHEC {} : attendu {:?}, obtenu {:?}", name, expected, got);
            }
            Err(error) => {
                failed += 1;
                crate::serial_println!("[js] ECHEC {} : {}", name, error);
            }
        }
    }
    // A script that never ends is stopped by its fuel, which nothing in
    // the script can catch.
    match run_case("try { while (true) {} } catch (e) { return 'rattrape'; }", headroom) {
        Err(reason) if reason.contains("budget") => passed += 1,
        other => {
            failed += 1;
            crate::serial_println!("[js] ECHEC boucle infinie non arretee : {:?}", other);
        }
    }
    let _ = Ctrl::Halt(String::new());
    (passed, failed)
}
