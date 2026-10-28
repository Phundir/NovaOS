//! `re`: Python's regular expressions on the JavaScript engine's matcher.
//!
//! The two dialects share nearly everything (classes, quantifiers, lazy
//! repetition, groups, lookarounds, backreferences). What differs is
//! translated before compiling: `(?P<name>...)` and `(?P=name)`, `\A` and
//! `\Z`, Python's `$` (end, or before a final newline), `.` that only
//! stops at `\n`, `^`/`$` of MULTILINE that only know `\n`, the inline
//! flags `(?imsx)` and VERBOSE patterns. Each pattern is compiled three
//! ways: to search, to match at a position (sticky), to match all of the
//! text (`fullmatch`).
//!
//! Iteration follows CPython 3.7+: an empty match is allowed right after a
//! non-empty one, never twice at the same place (`re.sub('x*', '-',
//! 'abxd')` is `'-a-b--d-'`).
//!
//! Limits (former): `\d`, `\w`, `\s` were ASCII (CPython's are Unicode on str
//! patterns); conditional groups `(?(1)...)` are refused.

use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

use super::super::args;
use super::super::dict::Dict;
use super::super::types::{class, method};
use super::super::value::*;
use super::super::vm::{PyErr, PyResult, Vm};
use crate::web::js::regex::{Input, Regex};

const IGNORECASE: i64 = 2;
const MULTILINE: i64 = 8;
const DOTALL: i64 = 16;
const UNICODE: i64 = 32;
const VERBOSE: i64 = 64;
const ASCII: i64 = 256;

/// A pattern compiled the three ways.
pub struct Compiled {
    pub pattern: Rc<str>,
    pub flags: i64,
    search: Regex,
    sticky: Regex,
    full: Regex,
}

/// Python syntax to JavaScript syntax; the flags found inline added.
fn translate(pattern: &str, mut flags: i64) -> Result<(String, i64), String> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut i = 0;
    // Global inline flags at the start: (?aiLmsux).
    while chars.get(i) == Some(&'(') && chars.get(i + 1) == Some(&'?') && chars.get(i + 2).is_some_and(|c| "aiLmsux".contains(*c)) {
        let mut j = i + 2;
        let mut found = 0;
        while j < chars.len() && "aiLmsux".contains(chars[j]) {
            found |= match chars[j] {
                'i' => IGNORECASE,
                'm' => MULTILINE,
                's' => DOTALL,
                'x' => VERBOSE,
                'a' => ASCII,
                'u' => UNICODE,
                _ => 0,
            };
            j += 1;
        }
        // `(?i:...)` is a scoped group, handled below.
        if chars.get(j) != Some(&')') {
            break;
        }
        flags |= found;
        i = j + 1;
    }
    let mut verbose = flags & VERBOSE != 0;
    let mut multiline = flags & MULTILINE != 0;
    let mut dotall = flags & DOTALL != 0;
    // Python's \d \w \s are Unicode on str patterns, JavaScript's ASCII.
    let unicode = flags & ASCII == 0;
    // A case-insensitive scope `(?i:...)` inside a case-sensitive pattern:
    // its letters become [xX] (the engine has no scoped flags).
    let mut local_ignore = false;
    let global_ignore = flags & IGNORECASE != 0;
    // Every open group: the flags to restore at its `)`, for `(?s:...)`.
    let mut scopes: Vec<Option<(bool, bool, bool, bool)>> = Vec::new();
    let mut out = String::with_capacity(pattern.len() + 8);
    let mut in_class = false;
    const WORD: &str = "\\p{L}\\p{N}_";
    const SPACE: &str = "\\t\\n\\x0b\\x0c\\r\\x1c-\\x1f \\x85\\xa0\\u1680\\u2000-\\u200a\\u2028\\u2029\\u202f\\u205f\\u3000";
    while i < chars.len() {
        let c = chars[i];
        if verbose && !in_class {
            if c.is_whitespace() {
                i += 1;
                continue;
            }
            if c == '#' {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
                continue;
            }
        }
        match c {
            '\\' => {
                let Some(&next) = chars.get(i + 1) else {
                    return Err(String::from("bad escape (end of pattern)"));
                };
                match next {
                    'A' if !in_class => out.push_str("(?<![\\s\\S])"),
                    'Z' if !in_class => out.push_str("(?![\\s\\S])"),
                    'd' if unicode => out.push_str("\\p{Nd}"),
                    'w' if unicode => {
                        if in_class { out.push_str(WORD) } else { out.push_str(&alloc::format!("[{}]", WORD)) }
                    }
                    's' if unicode => {
                        if in_class { out.push_str(SPACE) } else { out.push_str(&alloc::format!("[{}]", SPACE)) }
                    }
                    'D' if unicode && !in_class => out.push_str("\\P{Nd}"),
                    'W' if unicode && !in_class => out.push_str(&alloc::format!("[^{}]", WORD)),
                    'S' if unicode && !in_class => out.push_str(&alloc::format!("[^{}]", SPACE)),
                    'b' if unicode && !in_class => {
                        out.push_str(&alloc::format!("(?:(?<=[{w}])(?![{w}])|(?<![{w}])(?=[{w}]))", w = WORD))
                    }
                    'B' if unicode && !in_class => {
                        out.push_str(&alloc::format!("(?:(?<=[{w}])(?=[{w}])|(?<![{w}])(?![{w}]))", w = WORD))
                    }
                    _ => {
                        out.push('\\');
                        out.push(next);
                    }
                }
                i += 2;
                continue;
            }
            '[' if !in_class => {
                in_class = true;
                out.push('[');
                // A ']' right after '[' or '[^' is literal in Python.
                if chars.get(i + 1) == Some(&'^') {
                    out.push('^');
                    i += 1;
                }
                if chars.get(i + 1) == Some(&']') {
                    out.push_str("\\]");
                    i += 1;
                }
                i += 1;
                continue;
            }
            ']' if in_class => {
                in_class = false;
                out.push(']');
            }
            '(' if !in_class && chars.get(i + 1) == Some(&'?') => {
                let rest: String = chars[i + 2..].iter().take(3).collect();
                if rest.starts_with("P<") {
                    out.push_str("(?<");
                    scopes.push(None);
                    i += 4;
                    continue;
                }
                if rest.starts_with("P=") {
                    let end = chars[i..].iter().position(|c| *c == ')').map(|p| i + p).ok_or("missing ), unterminated name")?;
                    let name: String = chars[i + 4..end].iter().collect();
                    out.push_str(&alloc::format!("\\k<{}>", name));
                    i = end + 1;
                    continue;
                }
                if rest.starts_with('#') {
                    while i < chars.len() && chars[i] != ')' {
                        i += 1;
                    }
                    i += 1;
                    continue;
                }
                if rest.starts_with('(') {
                    return Err(String::from("conditional groups are not supported"));
                }
                // A scoped flag group: (?aimsx-imsx:...).
                let mut j = i + 2;
                let mut on = String::new();
                let mut off = String::new();
                let mut negative = false;
                while j < chars.len() && ("aiLmsux".contains(chars[j]) || chars[j] == '-') {
                    if chars[j] == '-' {
                        negative = true;
                    } else if negative {
                        off.push(chars[j]);
                    } else {
                        on.push(chars[j]);
                    }
                    j += 1;
                }
                if j > i + 2 && chars.get(j) == Some(&':') {
                    scopes.push(Some((dotall, multiline, verbose, local_ignore)));
                    for (set, value) in [(&on, true), (&off, false)] {
                        for f in set.chars() {
                            match f {
                                's' => dotall = value,
                                'm' => multiline = value,
                                'x' => verbose = value,
                                'i' => local_ignore = value && !global_ignore,
                                _ => {}
                            }
                        }
                    }
                    out.push_str("(?:");
                    i = j + 1;
                    continue;
                }
                if j > i + 2 && chars.get(j) == Some(&')') {
                    return Err(alloc::format!("global flags not at the start of the expression at position {}", i));
                }
                scopes.push(None);
                out.push('(');
            }
            '(' if !in_class => {
                scopes.push(None);
                out.push('(');
            }
            ')' if !in_class => {
                if let Some(Some((d, m, v, l))) = scopes.pop() {
                    dotall = d;
                    multiline = m;
                    verbose = v;
                    local_ignore = l;
                }
                out.push(')');
            }
            '.' if !in_class => out.push_str(if dotall { "[\\s\\S]" } else { "[^\\n]" }),
            '$' if !in_class => out.push_str(if multiline { "(?=\\n|(?![\\s\\S]))" } else { "(?=\\n?(?![\\s\\S]))" }),
            '^' if !in_class && multiline => out.push_str("(?:(?<![\\s\\S])|(?<=\\n))"),
            letter if local_ignore && letter.is_alphabetic() => {
                let lower: String = letter.to_lowercase().collect();
                let upper: String = letter.to_uppercase().collect();
                if in_class {
                    // A range of letters takes its other case too.
                    if chars.get(i + 1) == Some(&'-') && chars.get(i + 2).is_some_and(|c| c.is_alphabetic()) {
                        let high = chars[i + 2];
                        let other = |c: char| if c.is_lowercase() { c.to_uppercase().next().unwrap_or(c) } else { c.to_lowercase().next().unwrap_or(c) };
                        out.push_str(&alloc::format!("{}-{}{}-{}", letter, high, other(letter), other(high)));
                        i += 3;
                        continue;
                    }
                    out.push_str(&lower);
                    out.push_str(&upper);
                } else {
                    out.push_str(&alloc::format!("[{}{}]", lower, upper));
                }
            }
            other => out.push(other),
        }
        i += 1;
    }
    Ok((out, flags))
}

fn compile_pattern(vm: &mut Vm, pattern: &str, flags: i64) -> PyResult<usize> {
    if let Some(i) = vm.regexes.iter().position(|c| &*c.pattern == pattern && c.flags == flags) {
        return Ok(i);
    }
    let (source, flags_all) = match translate(pattern, flags) {
        Ok(t) => t,
        Err(message) => return Err(re_error(vm, &message)),
    };
    let js_flags = if flags_all & IGNORECASE != 0 { "i" } else { "" };
    let compile = |src: &str, extra: &str| Regex::new(src, &alloc::format!("{}{}", js_flags, extra));
    let full_source = alloc::format!("(?:{})(?![\\s\\S])", source);
    let result = compile(&source, "").and_then(|search| Ok((search, compile(&source, "y")?, compile(&full_source, "y")?)));
    match result {
        Ok((search, sticky, full)) => {
            vm.regexes.push(Compiled { pattern: Rc::from(pattern), flags: flags_all, search, sticky, full });
            Ok(vm.regexes.len() - 1)
        }
        Err(message) => Err(re_error(vm, &message)),
    }
}

fn re_error(vm: &mut Vm, message: &str) -> PyErr {
    let class = match module_class(vm, "error") {
        Some(c) => c,
        None => vm.types.value_error.clone(),
    };
    vm.error(&class, message)
}

fn module_class(vm: &Vm, name: &str) -> Option<Rc<Class>> {
    let (_, m) = vm.modules.iter().find(|(n, _)| &**n == "re")?;
    match m {
        Value::Module(m) => match m.dict.borrow().get_str(name) {
            Some(Value::Class(c)) => Some(c),
            _ => None,
        },
        _ => None,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Search,
    Match,
    Full,
}

type Captures = Vec<Option<(usize, usize)>>;

fn run(vm: &Vm, id: usize, input: &Input, at: usize, kind: Kind) -> Option<Captures> {
    let c = &vm.regexes[id];
    let regex = match kind {
        Kind::Search => &c.search,
        Kind::Match => &c.sticky,
        Kind::Full => &c.full,
    };
    if at > input.len() {
        return None;
    }
    regex.exec_chars(input, at)
}

/// Every match from `at`, CPython's rule for empty matches.
fn all_matches(vm: &Vm, id: usize, input: &Input, at: usize, limit: usize) -> Vec<Captures> {
    let mut out = Vec::new();
    let mut at = at;
    let mut must_advance = false;
    while at <= input.len() && (limit == 0 || out.len() < limit) {
        let mut found = run(vm, id, input, at, Kind::Search);
        if let Some(Some((s, e))) = found.as_ref().map(|c| c[0]) {
            if must_advance && s == e && s == at {
                found = run(vm, id, input, at + 1, Kind::Search);
            }
        }
        let Some(captures) = found else {
            break;
        };
        let Some((s, e)) = captures[0] else {
            break;
        };
        out.push(captures);
        must_advance = s == e;
        at = e;
    }
    out
}

// ------------------------------------------------------------
//  Pattern and Match objects
// ------------------------------------------------------------

fn pattern_id(vm: &mut Vm, value: &Value) -> PyResult<usize> {
    if let Value::Instance(i) = value {
        if let Some(Value::Int(n)) = i.dict.borrow().get_str("_id") {
            return Ok(n.to_i64().unwrap_or(0) as usize);
        }
    }
    vm.type_error("expected a compiled pattern")
}

fn make_pattern(vm: &mut Vm, id: usize) -> PyResult<Value> {
    let Some(class) = module_class(vm, "Pattern") else {
        return vm.type_error("re non initialise");
    };
    let instance = Instance::new(class);
    let (pattern, flags, groups, names) = {
        let c = &vm.regexes[id];
        (c.pattern.clone(), c.flags, c.search.groups, c.search.names.clone())
    };
    let mut groupindex = Dict::new();
    for (name, index) in &names {
        groupindex.set_str(name, Value::int(*index as i64));
    }
    {
        let mut d = instance.dict.borrow_mut();
        d.set_str("_id", Value::int(id as i64));
        d.set_str("pattern", Value::Str(pattern));
        d.set_str("flags", Value::int(flags | UNICODE));
        d.set_str("groups", Value::int(groups as i64));
        d.set_str("groupindex", Value::dict(groupindex));
    }
    Ok(Value::Instance(instance))
}

/// The pattern of a `re.*` call: a compiled one, or a string compiled now.
fn pattern_of(vm: &mut Vm, value: &Value, flags: i64) -> PyResult<usize> {
    match value {
        Value::Str(s) => compile_pattern(vm, s, flags),
        Value::Instance(_) => pattern_id(vm, value),
        other => {
            let name = vm.type_name(other);
            let message = alloc::format!("first argument must be string or compiled pattern, not {}", name);
            vm.type_error(&message)
        }
    }
}

fn text_of(vm: &mut Vm, value: Option<&Value>) -> PyResult<Rc<str>> {
    match value {
        Some(Value::Str(s)) => Ok(s.clone()),
        Some(other) => {
            let inner = vm.unwrap_builtin(other);
            if let Value::Str(s) = inner {
                return Ok(s);
            }
            let name = vm.type_name(other);
            let message = alloc::format!("expected string or bytes-like object, got '{}'", name);
            vm.type_error(&message)
        }
        None => vm.type_error("missing string argument"),
    }
}

fn substring(chars: &[char], span: (usize, usize)) -> Value {
    let s: String = chars[span.0..span.1].iter().collect();
    Value::str(&s)
}

fn make_match(vm: &mut Vm, id: usize, pattern: &Value, text: &Rc<str>, captures: &Captures, pos: usize, endpos: usize) -> PyResult<Value> {
    let Some(class) = module_class(vm, "Match") else {
        return vm.type_error("re non initialise");
    };
    let instance = Instance::new(class);
    let spans: Vec<Value> = captures
        .iter()
        .map(|c| match c {
            Some((s, e)) => Value::tuple(alloc::vec![Value::int(*s as i64), Value::int(*e as i64)]),
            None => Value::None,
        })
        .collect();
    let last = captures.iter().enumerate().skip(1).filter(|(_, c)| c.is_some()).max_by_key(|(_, c)| c.map(|(_, e)| e)).map(|(i, _)| i);
    let lastgroup = last.and_then(|g| vm.regexes[id].search.names.iter().find(|(_, i)| *i == g).map(|(n, _)| Value::str(n)));
    {
        let mut d = instance.dict.borrow_mut();
        d.set_str("_spans", Value::tuple(spans));
        d.set_str("_id", Value::int(id as i64));
        d.set_str("string", Value::Str(text.clone()));
        d.set_str("re", pattern.clone());
        d.set_str("pos", Value::int(pos as i64));
        d.set_str("endpos", Value::int(endpos as i64));
        d.set_str("lastindex", last.map(|l| Value::int(l as i64)).unwrap_or(Value::None));
        d.set_str("lastgroup", lastgroup.unwrap_or(Value::None));
    }
    Ok(Value::Instance(instance))
}

/// The spans of a match object and its text.
fn match_parts(vm: &mut Vm, value: &Value) -> PyResult<(Vec<Option<(usize, usize)>>, Rc<str>, usize)> {
    let Value::Instance(i) = value else {
        return vm.type_error("expected a match object");
    };
    let d = i.dict.borrow();
    let spans = match d.get_str("_spans") {
        Some(Value::Tuple(t)) => t
            .iter()
            .map(|s| match s {
                Value::Tuple(p) => match (&p[0], &p[1]) {
                    (Value::Int(a), Value::Int(b)) => Some((a.to_i64().unwrap_or(0) as usize, b.to_i64().unwrap_or(0) as usize)),
                    _ => None,
                },
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    };
    let text = match d.get_str("string") {
        Some(Value::Str(s)) => s,
        _ => Rc::from(""),
    };
    let id = match d.get_str("_id") {
        Some(Value::Int(n)) => n.to_i64().unwrap_or(0) as usize,
        _ => 0,
    };
    Ok((spans, text, id))
}

/// A group reference (number or name) to its index.
fn group_index(vm: &mut Vm, id: usize, count: usize, which: &Value) -> PyResult<usize> {
    let index = match which {
        Value::Int(n) => n.to_i64().filter(|n| *n >= 0).map(|n| n as usize),
        Value::Bool(b) => Some(*b as usize),
        Value::Str(name) => vm.regexes[id].search.names.iter().find(|(n, _)| n.as_str() == &**name).map(|(_, i)| *i),
        _ => None,
    };
    match index {
        Some(i) if i < count => Ok(i),
        _ => vm.index_error("no such group"),
    }
}

fn group_value(spans: &[Option<(usize, usize)>], chars: &[char], index: usize, default: &Value) -> Value {
    match spans.get(index).copied().flatten() {
        Some(span) => substring(chars, span),
        None => default.clone(),
    }
}

fn m_group(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let (spans, text, id) = match_parts(vm, &a[0])?;
    let chars: Vec<char> = text.chars().collect();
    if a.len() <= 1 {
        return Ok(group_value(&spans, &chars, 0, &Value::None));
    }
    let mut out = Vec::new();
    for which in &a[1..] {
        let g = group_index(vm, id, spans.len(), which)?;
        out.push(group_value(&spans, &chars, g, &Value::None));
    }
    if out.len() == 1 {
        return Ok(out.pop().unwrap_or(Value::None));
    }
    Ok(Value::tuple(out))
}

fn m_getitem(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    m_group(vm, &a[..a.len().min(2)], &[])
}

fn m_groups(vm: &mut Vm, a: &[Value], kw: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let (spans, text, _) = match_parts(vm, &a[0])?;
    let default = args::get(&a[1..], kw, 0, "default").cloned().unwrap_or(Value::None);
    let chars: Vec<char> = text.chars().collect();
    Ok(Value::tuple((1..spans.len()).map(|g| group_value(&spans, &chars, g, &default)).collect()))
}

fn m_groupdict(vm: &mut Vm, a: &[Value], kw: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let (spans, text, id) = match_parts(vm, &a[0])?;
    let default = args::get(&a[1..], kw, 0, "default").cloned().unwrap_or(Value::None);
    let chars: Vec<char> = text.chars().collect();
    let mut names = vm.regexes[id].search.names.clone();
    names.sort_by_key(|(_, i)| *i);
    let mut d = Dict::new();
    for (name, index) in names {
        d.set_str(&name, group_value(&spans, &chars, index, &default));
    }
    Ok(Value::dict(d))
}

fn m_position(vm: &mut Vm, a: &[Value], which: u8) -> PyResult<Value> {
    let (spans, _, id) = match_parts(vm, &a[0])?;
    let g = match a.get(1) {
        Some(v) => group_index(vm, id, spans.len(), v)?,
        None => 0,
    };
    let (s, e) = match spans.get(g).copied().flatten() {
        Some((s, e)) => (s as i64, e as i64),
        None => (-1, -1),
    };
    Ok(match which {
        0 => Value::int(s),
        1 => Value::int(e),
        _ => Value::tuple(alloc::vec![Value::int(s), Value::int(e)]),
    })
}

fn m_start(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    m_position(vm, a, 0)
}

fn m_end(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    m_position(vm, a, 1)
}

fn m_span(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    m_position(vm, a, 2)
}

fn m_repr(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let (spans, text, _) = match_parts(vm, &a[0])?;
    let chars: Vec<char> = text.chars().collect();
    let (s, e) = spans.first().copied().flatten().unwrap_or((0, 0));
    let shown = vm.repr(&substring(&chars, (s, e)))?;
    Ok(Value::str(&alloc::format!("<re.Match object; span=({}, {}), match={}>", s, e, shown)))
}

fn m_expand(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let (spans, text, id) = match_parts(vm, &a[0])?;
    let template = text_of(vm, a.get(1))?;
    let pieces = parse_template(vm, id, &template)?;
    let chars: Vec<char> = text.chars().collect();
    Ok(Value::str(&expand(&pieces, &spans, &chars)))
}

// ------------------------------------------------------------
//  Replacement templates
// ------------------------------------------------------------

enum Piece {
    Text(String),
    Group(usize),
}

fn parse_template(vm: &mut Vm, id: usize, template: &str) -> PyResult<Vec<Piece>> {
    let groups = vm.regexes[id].search.groups;
    let chars: Vec<char> = template.chars().collect();
    let mut pieces = Vec::new();
    let mut text = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c != '\\' {
            text.push(c);
            i += 1;
            continue;
        }
        let Some(&next) = chars.get(i + 1) else {
            return Err(re_error(vm, &alloc::format!("bad escape (end of pattern) at position {}", i)));
        };
        let mut group = None;
        match next {
            'n' => text.push('\n'),
            't' => text.push('\t'),
            'r' => text.push('\r'),
            'f' => text.push('\u{c}'),
            'v' => text.push('\u{b}'),
            'a' => text.push('\u{7}'),
            'b' => text.push('\u{8}'),
            '\\' => text.push('\\'),
            'g' => {
                if chars.get(i + 2) != Some(&'<') {
                    return Err(re_error(vm, &alloc::format!("missing < at position {}", i + 2)));
                }
                let Some(end) = chars[i + 3..].iter().position(|c| *c == '>').map(|p| i + 3 + p) else {
                    return Err(re_error(vm, &alloc::format!("missing >, unterminated name at position {}", i + 3)));
                };
                let name: String = chars[i + 3..end].iter().collect();
                let index = if !name.is_empty() && name.bytes().all(|b| b.is_ascii_digit()) {
                    name.parse::<usize>().ok()
                } else {
                    vm.regexes[id].search.names.iter().find(|(n, _)| *n == name).map(|(_, g)| *g)
                };
                match index {
                    Some(g) if g <= groups => group = Some(g),
                    Some(g) => return Err(re_error(vm, &alloc::format!("invalid group reference {} at position {}", g, i + 3))),
                    None => {
                        let class = vm.types.index_error.clone();
                        return Err(vm.error(&class, &alloc::format!("unknown group name '{}'", name)));
                    }
                }
                pieces.push(Piece::Text(core::mem::take(&mut text)));
                pieces.push(Piece::Group(group.unwrap_or(0)));
                i = end + 1;
                continue;
            }
            '0'..='9' => {
                // Up to two digits of group number (three octal digits
                // are a character).
                let mut j = i + 1;
                let mut digits = String::new();
                while j < chars.len() && chars[j].is_ascii_digit() && digits.len() < 3 {
                    digits.push(chars[j]);
                    j += 1;
                }
                let octal = digits.len() == 3 && digits.bytes().all(|b| (b'0'..=b'7').contains(&b)) || next == '0';
                if octal {
                    let digits: String = digits.chars().take_while(|c| ('0'..='7').contains(c)).take(3).collect();
                    let code = u32::from_str_radix(&digits, 8).unwrap_or(0);
                    text.push(char::from_u32(code).unwrap_or('?'));
                    i += 1 + digits.len();
                    continue;
                }
                let digits: String = digits.chars().take(2).collect();
                let g: usize = digits.parse().unwrap_or(0);
                if g > groups {
                    return Err(re_error(vm, &alloc::format!("invalid group reference {} at position {}", g, i + 1)));
                }
                pieces.push(Piece::Text(core::mem::take(&mut text)));
                pieces.push(Piece::Group(g));
                i += 1 + digits.len();
                continue;
            }
            c if c.is_ascii_alphabetic() => {
                return Err(re_error(vm, &alloc::format!("bad escape \\{} at position {}", c, i)));
            }
            other => {
                text.push('\\');
                text.push(other);
            }
        }
        let _ = group;
        i += 2;
    }
    pieces.push(Piece::Text(text));
    Ok(pieces)
}

fn expand(pieces: &[Piece], spans: &[Option<(usize, usize)>], chars: &[char]) -> String {
    let mut out = String::new();
    for piece in pieces {
        match piece {
            Piece::Text(t) => out.push_str(t),
            Piece::Group(g) => {
                if let Some((s, e)) = spans.get(*g).copied().flatten() {
                    out.extend(chars[s..e].iter());
                }
            }
        }
    }
    out
}

// ------------------------------------------------------------
//  The operations
// ------------------------------------------------------------

struct Call {
    id: usize,
    pattern: Value,
    text: Rc<str>,
    chars: Vec<char>,
    pos: usize,
    endpos: usize,
}

/// The arguments of a method (`self` a Pattern) or of a module function
/// (the pattern first, the flags as a keyword or after the string).
fn prepare(vm: &mut Vm, a: &[Value], kw: &[(Rc<str>, Value)], flags_at: usize) -> PyResult<(Call, Vec<Value>)> {
    let Some(first) = a.first() else {
        return vm.type_error("missing pattern argument");
    };
    let flags = match args::keyword(kw, "flags").or_else(|| a.get(flags_at)) {
        Some(Value::Int(n)) => n.to_i64().unwrap_or(0),
        Some(Value::Instance(i)) => match &*i.inner.borrow() {
            Value::Int(n) => n.to_i64().unwrap_or(0),
            _ => 0,
        },
        _ => 0,
    };
    let id = pattern_of(vm, first, flags)?;
    let pattern = match first {
        Value::Instance(_) => first.clone(),
        _ => make_pattern(vm, id)?,
    };
    let rest: Vec<Value> = a[1..].to_vec();
    Ok((Call { id, pattern, text: Rc::from(""), chars: Vec::new(), pos: 0, endpos: 0 }, rest))
}

fn with_text(vm: &mut Vm, mut call: Call, text: Option<&Value>, pos: Option<&Value>, endpos: Option<&Value>) -> PyResult<Call> {
    let text = text_of(vm, text)?;
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len() as i64;
    let clamp = |v: i64| if v < 0 { 0 } else { v.min(len) } as usize;
    call.pos = clamp(args::opt_int(vm, pos, 0)?);
    call.endpos = clamp(args::opt_int(vm, endpos, len)?);
    call.text = text;
    call.chars = chars;
    Ok(call)
}

fn single(vm: &mut Vm, a: &[Value], kw: &[(Rc<str>, Value)], kind: Kind, method_call: bool) -> PyResult<Value> {
    let (call, rest) = prepare(vm, a, kw, if method_call { usize::MAX } else { 2 })?;
    let (pos, endpos) = if method_call { (args::get(&rest, kw, 1, "pos"), args::get(&rest, kw, 2, "endpos")) } else { (None, None) };
    let call = with_text(vm, call, args::get(&rest, kw, 0, "string"), pos, endpos)?;
    let visible: String = call.chars[..call.endpos.max(call.pos)].iter().collect();
    let input = Input::new(&visible);
    match run(vm, call.id, &input, call.pos, kind) {
        Some(captures) => make_match(vm, call.id, &call.pattern, &call.text, &captures, call.pos, call.endpos),
        None => Ok(Value::None),
    }
}

fn findall_values(vm: &mut Vm, call: &Call) -> Vec<Value> {
    let visible: String = call.chars[..call.endpos.max(call.pos)].iter().collect();
    let input = Input::new(&visible);
    let groups = vm.regexes[call.id].search.groups;
    all_matches(vm, call.id, &input, call.pos, 0)
        .iter()
        .map(|captures| {
            let take = |g: usize| match captures[g] {
                Some(span) => substring(&call.chars, span),
                None => Value::str(""),
            };
            match groups {
                0 => take(0),
                1 => take(1),
                n => Value::tuple((1..=n).map(take).collect()),
            }
        })
        .collect()
}

fn findall(vm: &mut Vm, a: &[Value], kw: &[(Rc<str>, Value)], method_call: bool) -> PyResult<Value> {
    let (call, rest) = prepare(vm, a, kw, if method_call { usize::MAX } else { 2 })?;
    let (pos, endpos) = if method_call { (args::get(&rest, kw, 1, "pos"), args::get(&rest, kw, 2, "endpos")) } else { (None, None) };
    let call = with_text(vm, call, args::get(&rest, kw, 0, "string"), pos, endpos)?;
    Ok(Value::list(findall_values(vm, &call)))
}

fn finditer(vm: &mut Vm, a: &[Value], kw: &[(Rc<str>, Value)], method_call: bool) -> PyResult<Value> {
    let (call, rest) = prepare(vm, a, kw, if method_call { usize::MAX } else { 2 })?;
    let (pos, endpos) = if method_call { (args::get(&rest, kw, 1, "pos"), args::get(&rest, kw, 2, "endpos")) } else { (None, None) };
    let call = with_text(vm, call, args::get(&rest, kw, 0, "string"), pos, endpos)?;
    let visible: String = call.chars[..call.endpos.max(call.pos)].iter().collect();
    let input = Input::new(&visible);
    let found = all_matches(vm, call.id, &input, call.pos, 0);
    let mut matches = Vec::with_capacity(found.len());
    for captures in &found {
        matches.push(make_match(vm, call.id, &call.pattern, &call.text, captures, call.pos, call.endpos)?);
    }
    vm.get_iter(&Value::list(matches))
}

fn substitute(vm: &mut Vm, a: &[Value], kw: &[(Rc<str>, Value)], method_call: bool, with_count: bool) -> PyResult<Value> {
    // Pattern.sub(repl, string, count=0); re.sub(pattern, repl, string, count=0, flags=0)
    let (call, rest) = prepare(vm, a, kw, if method_call { usize::MAX } else { 4 })?;
    let Some(repl) = args::get(&rest, kw, 0, "repl").cloned() else {
        return vm.type_error("sub() missing required argument 'repl' (pos 2)");
    };
    let count = args::opt_int(vm, args::get(&rest, kw, 2, "count"), 0)?.max(0) as usize;
    let call = with_text(vm, call, args::get(&rest, kw, 1, "string"), None, None)?;
    let input = Input::new(&call.text);
    let found = all_matches(vm, call.id, &input, 0, count);
    let template = match &repl {
        Value::Str(s) => Some(parse_template(vm, call.id, s)?),
        _ => None,
    };
    let mut out = String::new();
    let mut last = 0;
    for captures in &found {
        let Some((s, e)) = captures[0] else {
            continue;
        };
        out.extend(call.chars[last..s].iter());
        match &template {
            Some(pieces) => out.push_str(&expand(pieces, captures, &call.chars)),
            None => {
                let m = make_match(vm, call.id, &call.pattern, &call.text, captures, 0, call.chars.len())?;
                let r = vm.call(&repl, &[m], &[])?;
                match r {
                    Value::Str(s) => out.push_str(&s),
                    other => {
                        let name = vm.type_name(&other);
                        let message = alloc::format!("expected str instance, {} found", name);
                        return vm.type_error(&message);
                    }
                }
            }
        }
        last = e;
    }
    out.extend(call.chars[last..].iter());
    let text = Value::str(&out);
    if with_count {
        return Ok(Value::tuple(alloc::vec![text, Value::int(found.len() as i64)]));
    }
    Ok(text)
}

fn split(vm: &mut Vm, a: &[Value], kw: &[(Rc<str>, Value)], method_call: bool) -> PyResult<Value> {
    let (call, rest) = prepare(vm, a, kw, if method_call { usize::MAX } else { 3 })?;
    let maxsplit = args::opt_int(vm, args::get(&rest, kw, 1, "maxsplit"), 0)?.max(0) as usize;
    let call = with_text(vm, call, args::get(&rest, kw, 0, "string"), None, None)?;
    let input = Input::new(&call.text);
    let found = all_matches(vm, call.id, &input, 0, maxsplit);
    let mut out = Vec::new();
    let mut last = 0;
    for captures in &found {
        let Some((s, e)) = captures[0] else {
            continue;
        };
        out.push(substring(&call.chars, (last, s)));
        for group in captures.iter().skip(1) {
            out.push(match group {
                Some(span) => substring(&call.chars, *span),
                None => Value::None,
            });
        }
        last = e;
    }
    out.push(substring(&call.chars, (last, call.chars.len())));
    Ok(Value::list(out))
}

// Module functions.
fn re_compile(vm: &mut Vm, a: &[Value], kw: &[(Rc<str>, Value)]) -> PyResult<Value> {
    if let Some(p @ Value::Instance(_)) = a.first() {
        return Ok(p.clone());
    }
    let (call, _) = prepare(vm, a, kw, 1)?;
    Ok(call.pattern)
}

fn re_match(vm: &mut Vm, a: &[Value], kw: &[(Rc<str>, Value)]) -> PyResult<Value> {
    single(vm, a, kw, Kind::Match, false)
}

fn re_search(vm: &mut Vm, a: &[Value], kw: &[(Rc<str>, Value)]) -> PyResult<Value> {
    single(vm, a, kw, Kind::Search, false)
}

fn re_fullmatch(vm: &mut Vm, a: &[Value], kw: &[(Rc<str>, Value)]) -> PyResult<Value> {
    single(vm, a, kw, Kind::Full, false)
}

fn re_findall(vm: &mut Vm, a: &[Value], kw: &[(Rc<str>, Value)]) -> PyResult<Value> {
    findall(vm, a, kw, false)
}

fn re_finditer(vm: &mut Vm, a: &[Value], kw: &[(Rc<str>, Value)]) -> PyResult<Value> {
    finditer(vm, a, kw, false)
}

fn re_sub(vm: &mut Vm, a: &[Value], kw: &[(Rc<str>, Value)]) -> PyResult<Value> {
    substitute(vm, a, kw, false, false)
}

fn re_subn(vm: &mut Vm, a: &[Value], kw: &[(Rc<str>, Value)]) -> PyResult<Value> {
    substitute(vm, a, kw, false, true)
}

fn re_split(vm: &mut Vm, a: &[Value], kw: &[(Rc<str>, Value)]) -> PyResult<Value> {
    split(vm, a, kw, false)
}

fn re_escape(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let text = text_of(vm, a.first())?;
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if "()[]{}?*+-|^$\\.&~# \t\n\r\u{b}\u{c}".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    Ok(Value::str(&out))
}

fn re_purge(vm: &mut Vm, _: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let _ = vm;
    Ok(Value::None)
}

// Pattern methods.
fn p_match(vm: &mut Vm, a: &[Value], kw: &[(Rc<str>, Value)]) -> PyResult<Value> {
    single(vm, a, kw, Kind::Match, true)
}

fn p_search(vm: &mut Vm, a: &[Value], kw: &[(Rc<str>, Value)]) -> PyResult<Value> {
    single(vm, a, kw, Kind::Search, true)
}

fn p_fullmatch(vm: &mut Vm, a: &[Value], kw: &[(Rc<str>, Value)]) -> PyResult<Value> {
    single(vm, a, kw, Kind::Full, true)
}

fn p_findall(vm: &mut Vm, a: &[Value], kw: &[(Rc<str>, Value)]) -> PyResult<Value> {
    findall(vm, a, kw, true)
}

fn p_finditer(vm: &mut Vm, a: &[Value], kw: &[(Rc<str>, Value)]) -> PyResult<Value> {
    finditer(vm, a, kw, true)
}

fn p_sub(vm: &mut Vm, a: &[Value], kw: &[(Rc<str>, Value)]) -> PyResult<Value> {
    substitute(vm, a, kw, true, false)
}

fn p_subn(vm: &mut Vm, a: &[Value], kw: &[(Rc<str>, Value)]) -> PyResult<Value> {
    substitute(vm, a, kw, true, true)
}

fn p_split(vm: &mut Vm, a: &[Value], kw: &[(Rc<str>, Value)]) -> PyResult<Value> {
    split(vm, a, kw, true)
}

fn p_repr(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let id = pattern_id(vm, &a[0])?;
    let (pattern, flags) = {
        let c = &vm.regexes[id];
        (c.pattern.clone(), c.flags)
    };
    let shown = vm.repr(&Value::Str(pattern))?;
    let names: Vec<&str> = [(IGNORECASE, "re.IGNORECASE"), (MULTILINE, "re.MULTILINE"), (DOTALL, "re.DOTALL"), (VERBOSE, "re.VERBOSE"), (ASCII, "re.ASCII")]
        .iter()
        .filter(|(bit, _)| flags & bit != 0)
        .map(|(_, n)| *n)
        .collect();
    if names.is_empty() {
        Ok(Value::str(&alloc::format!("re.compile({})", shown)))
    } else {
        Ok(Value::str(&alloc::format!("re.compile({}, {})", shown, names.join("|"))))
    }
}

pub fn module(vm: &mut Vm) -> Rc<Module> {
    let functions: &[(&'static str, NativeFn)] = &[
        ("compile", re_compile), ("match", re_match), ("search", re_search), ("fullmatch", re_fullmatch),
        ("findall", re_findall), ("finditer", re_finditer), ("sub", re_sub), ("subn", re_subn), ("split", re_split),
        ("escape", re_escape), ("purge", re_purge),
    ];
    let module = super::native_module(vm, "re", functions);
    let object = vm.types.object.clone();
    let pattern = class("Pattern", alloc::vec![object.clone()], Base::Object);
    let methods: &[(&'static str, NativeFn)] = &[
        ("match", p_match), ("search", p_search), ("fullmatch", p_fullmatch), ("findall", p_findall),
        ("finditer", p_finditer), ("sub", p_sub), ("subn", p_subn), ("split", p_split), ("__repr__", p_repr),
    ];
    for (name, f) in methods {
        method(&pattern, name, *f);
    }
    let matched = class("Match", alloc::vec![object], Base::Object);
    let methods: &[(&'static str, NativeFn)] = &[
        ("group", m_group), ("__getitem__", m_getitem), ("groups", m_groups), ("groupdict", m_groupdict), ("start", m_start),
        ("end", m_end), ("span", m_span), ("__repr__", m_repr), ("expand", m_expand),
    ];
    for (name, f) in methods {
        method(&matched, name, *f);
    }
    for c in [&pattern, &matched] {
        c.dict.borrow_mut().set_str("__module__", Value::str("re"));
    }
    let error = class("error", alloc::vec![vm.types.exception.clone()], Base::Exception);
    error.dict.borrow_mut().set_str("__module__", Value::str("re"));
    super::set(&module, "Pattern", Value::Class(pattern));
    super::set(&module, "Match", Value::Class(matched));
    super::set(&module, "error", Value::Class(error));
    for (name, value) in [
        ("I", IGNORECASE), ("IGNORECASE", IGNORECASE), ("M", MULTILINE), ("MULTILINE", MULTILINE), ("S", DOTALL),
        ("DOTALL", DOTALL), ("X", VERBOSE), ("VERBOSE", VERBOSE), ("A", ASCII), ("ASCII", ASCII), ("U", UNICODE),
        ("UNICODE", UNICODE), ("NOFLAG", 0),
    ] {
        super::set(&module, name, Value::int(value));
    }
    module
}
