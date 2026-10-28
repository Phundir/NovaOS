//! The preprocessor: translation phase 4.
//!
//! Directives (`#include`, `#define`, `#undef`, `#if` and its family,
//! `#error`, `#pragma once`) and macro expansion, as C11 6.10 describes
//! them. Expansion follows Prosser's algorithm -- the one GCC and Clang
//! implement -- through a **hide set** on every token: the names of the
//! macros it came out of, which it may not expand again. That is what
//! makes `#define foo foo` terminate and `#define f(x) x f` expand the
//! way the standard's examples say.
//!
//! The token stream is a stack (`pending`, top at the end): an expansion
//! is pushed back to be read again, which is how rescanning works, and an
//! `#include` pushes the tokens of its file.

use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

use super::CcError;
use super::lex::{self, Kind, Loc, Token};

/// Where `#include` finds its files.
pub trait Sources {
    /// A file of the program (`#include "x.h"`, the `.c` files).
    fn read(&self, path: &str) -> Option<String>;
    /// A header of the library (`#include <stdio.h>`).
    fn system(&self, name: &str) -> Option<&'static str> {
        super::lib::header(name)
    }
}

struct Macro {
    /// `None`: an object-like macro.
    params: Option<Vec<Rc<str>>>,
    variadic: bool,
    body: Vec<Token>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Branch {
    Then,
    Elif,
    Else,
}

struct Cond {
    branch: Branch,
    /// Has one branch of this `#if` been taken already?
    taken: bool,
    loc: Loc,
}

pub struct Preprocessor<'a> {
    sources: &'a dyn Sources,
    /// File names, by index: `Loc::file` refers to them.
    pub names: Vec<String>,
    macros: BTreeMap<Rc<str>, Rc<Macro>>,
    once: Vec<String>,
    pending: Vec<Token>,
    conds: Vec<Cond>,
    counter: i64,
    includes: usize,
}

/// Most files one compilation may include: a header that includes itself
/// without a guard stops here rather than filling memory.
const MAX_INCLUDES: usize = 4096;

fn number_token(value: i64, like: &Token) -> Token {
    Token { kind: Kind::Number, text: Rc::from(alloc::format!("{}", value).as_str()), ..like.clone() }
}

impl<'a> Preprocessor<'a> {
    pub fn new(sources: &'a dyn Sources, predefined: &[(&str, &str)]) -> Preprocessor<'a> {
        let mut pp = Preprocessor {
            sources,
            names: alloc::vec![String::from("<predefini>")],
            macros: BTreeMap::new(),
            once: Vec::new(),
            pending: Vec::new(),
            conds: Vec::new(),
            counter: 0,
            includes: 0,
        };
        for (name, value) in predefined {
            if let Ok(body) = lex::tokenize(value, 0) {
                pp.macros.insert(Rc::from(*name), Rc::new(Macro { params: None, variadic: false, body }));
            }
        }
        pp
    }

    /// Preprocesses the file `path` (already read as `source`) and
    /// returns the tokens the parser will read.
    pub fn run(&mut self, path: &str, source: &str) -> Result<Vec<Token>, CcError> {
        let file = self.names.len() as u32;
        self.names.push(String::from(path));
        let mut tokens = lex::tokenize(source, file)?;
        tokens.reverse();
        self.pending = tokens;
        let mut out = Vec::new();
        while let Some(token) = self.pending.pop() {
            if self.expand(&token)? {
                continue;
            }
            if token.is("#") && token.bol {
                self.directive(token)?;
                continue;
            }
            out.push(token);
        }
        if let Some(cond) = self.conds.last() {
            return Err(CcError::at(cond.loc, "#if sans #endif"));
        }
        Ok(out)
    }

    /// The rest of the current line, raw.
    fn line(&mut self) -> Vec<Token> {
        let mut out = Vec::new();
        while self.pending.last().is_some_and(|t| !t.bol) {
            out.push(self.pending.pop().unwrap_or_else(|| unreachable!()));
        }
        out
    }

    fn directive(&mut self, hash: Token) -> Result<(), CcError> {
        if self.pending.last().is_none_or(|t| t.bol) {
            return Ok(()); // the null directive
        }
        let name = self.pending.pop().unwrap_or_else(|| unreachable!());
        let loc = name.loc;
        match &*name.text {
            "include" => {
                let line = self.line();
                self.include(line, loc)
            }
            "define" => {
                let line = self.line();
                self.define(line, loc)
            }
            "undef" => {
                let line = self.line();
                if let Some(target) = line.first().filter(|t| t.kind == Kind::Ident) {
                    self.macros.remove(&target.text);
                }
                Ok(())
            }
            "if" => {
                let line = self.line();
                let value = self.condition(line, loc)?;
                self.conds.push(Cond { branch: Branch::Then, taken: value, loc });
                if !value {
                    self.skip()?;
                }
                Ok(())
            }
            "ifdef" | "ifndef" => {
                let line = self.line();
                let Some(target) = line.first().filter(|t| t.kind == Kind::Ident) else {
                    return Err(CcError::at(loc, "#ifdef sans nom de macro"));
                };
                let defined = self.macros.contains_key(&target.text) || is_dynamic(&target.text);
                let value = defined == (&*name.text == "ifdef");
                self.conds.push(Cond { branch: Branch::Then, taken: value, loc });
                if !value {
                    self.skip()?;
                }
                Ok(())
            }
            "elif" => {
                let line = self.line();
                let Some(cond) = self.conds.last() else {
                    return Err(CcError::at(loc, "#elif sans #if"));
                };
                if cond.branch == Branch::Else {
                    return Err(CcError::at(loc, "#elif apres #else"));
                }
                let taken = cond.taken;
                if let Some(cond) = self.conds.last_mut() {
                    cond.branch = Branch::Elif;
                }
                if taken || !self.condition(line, loc)? {
                    self.skip()
                } else {
                    if let Some(cond) = self.conds.last_mut() {
                        cond.taken = true;
                    }
                    Ok(())
                }
            }
            "else" => {
                self.line();
                let Some(cond) = self.conds.last_mut() else {
                    return Err(CcError::at(loc, "#else sans #if"));
                };
                if cond.branch == Branch::Else {
                    return Err(CcError::at(loc, "#else en double"));
                }
                cond.branch = Branch::Else;
                if cond.taken {
                    self.skip()
                } else {
                    cond.taken = true;
                    Ok(())
                }
            }
            "endif" => {
                self.line();
                if self.conds.pop().is_none() {
                    return Err(CcError::at(loc, "#endif sans #if"));
                }
                Ok(())
            }
            "error" => {
                let line = self.line();
                Err(CcError::at(loc, &alloc::format!("#error {}", lex::spell(&line))))
            }
            "warning" | "line" | "ident" => {
                self.line();
                Ok(())
            }
            "pragma" => {
                let line = self.line();
                if line.first().is_some_and(|t| t.is("once")) {
                    let file = self.names[loc.file as usize].clone();
                    if !self.once.contains(&file) {
                        self.once.push(file);
                    }
                }
                Ok(())
            }
            _ if name.kind == Kind::Number => {
                // A line marker (`# 12 "file"`), as `cpp` writes them.
                self.line();
                Ok(())
            }
            other => {
                let _ = hash;
                Err(CcError::at(loc, &alloc::format!("directive inconnue #{}", other)))
            }
        }
    }

    /// Skips a branch not taken, up to the `#elif`, `#else` or `#endif`
    /// that ends it (which is left to be read).
    fn skip(&mut self) -> Result<(), CcError> {
        let mut depth = 0;
        while let Some(token) = self.pending.pop() {
            if !(token.is("#") && token.bol) {
                continue;
            }
            let Some(name) = self.pending.last() else { break };
            if name.bol {
                continue;
            }
            match &*name.text {
                "if" | "ifdef" | "ifndef" => depth += 1,
                "endif" if depth > 0 => depth -= 1,
                "elif" | "else" | "endif" if depth == 0 => {
                    self.pending.push(token);
                    return Ok(());
                }
                _ => {}
            }
        }
        let loc = self.conds.last().map(|c| c.loc).unwrap_or_default();
        Err(CcError::at(loc, "#if sans #endif"))
    }

    fn include(&mut self, line: Vec<Token>, loc: Loc) -> Result<(), CcError> {
        let line = if line.first().is_some_and(|t| t.kind == Kind::Str || t.is("<")) { line } else { self.expand_list(line)? };
        let Some(first) = line.first() else {
            return Err(CcError::at(loc, "#include sans fichier"));
        };
        let (name, quoted) = if first.kind == Kind::Str {
            (String::from(&first.text[1..first.text.len() - 1]), true)
        } else if first.is("<") {
            let mut name = String::new();
            let mut closed = false;
            for token in &line[1..] {
                if token.is(">") {
                    closed = true;
                    break;
                }
                name.push_str(&token.text);
            }
            if !closed {
                return Err(CcError::at(loc, "#include <...> sans >"));
            }
            (name, false)
        } else {
            return Err(CcError::at(loc, "#include attend \"fichier\" ou <fichier>"));
        };

        self.includes += 1;
        if self.includes > MAX_INCLUDES {
            return Err(CcError::at(loc, "trop d'inclusions (un en-tete s'inclut-il lui-meme ?)"));
        }
        let (path, source) = if quoted {
            let including = &self.names[loc.file as usize];
            let base = match including.rfind('/') {
                Some(index) => &including[..=index],
                None => "",
            };
            let path = alloc::format!("{}{}", base, name);
            match self.sources.read(&path) {
                Some(source) => (path, source),
                None => match self.sources.system(&name) {
                    Some(source) => (alloc::format!("<{}>", name), String::from(source)),
                    None => return Err(CcError::at(loc, &alloc::format!("fichier introuvable : \"{}\"", name))),
                },
            }
        } else {
            match self.sources.system(&name) {
                Some(source) => (alloc::format!("<{}>", name), String::from(source)),
                None => {
                    return Err(CcError::at(loc, &alloc::format!("en-tete <{}> absent de la bibliotheque C de Nova", name)));
                }
            }
        };
        if self.once.contains(&path) {
            return Ok(());
        }
        let file = self.names.len() as u32;
        self.names.push(path);
        let mut tokens = lex::tokenize(&source, file)?;
        // The included file starts on a line of its own, and what follows
        // the directive too.
        if let Some(first) = tokens.first_mut() {
            first.bol = true;
        }
        if let Some(next) = self.pending.last_mut() {
            next.bol = true;
        }
        tokens.reverse();
        self.pending.extend(tokens);
        Ok(())
    }

    fn define(&mut self, line: Vec<Token>, loc: Loc) -> Result<(), CcError> {
        let Some(name) = line.first().filter(|t| t.kind == Kind::Ident) else {
            return Err(CcError::at(loc, "#define attend un nom"));
        };
        let mut rest = &line[1..];
        let mut params = None;
        let mut variadic = false;
        if rest.first().is_some_and(|t| t.is("(") && !t.space) {
            let mut list = Vec::new();
            let mut index = 1;
            loop {
                let Some(token) = rest.get(index) else {
                    return Err(CcError::at(loc, "parametres de macro non fermes"));
                };
                if token.is(")") && list.is_empty() && !variadic {
                    index += 1;
                    break;
                }
                if token.is("...") {
                    variadic = true;
                    list.push(Rc::from("__VA_ARGS__"));
                } else if token.kind == Kind::Ident {
                    list.push(token.text.clone());
                } else {
                    return Err(CcError::at(loc, "parametre de macro invalide"));
                }
                index += 1;
                match rest.get(index) {
                    Some(t) if t.is(",") && !variadic => index += 1,
                    Some(t) if t.is(")") => {
                        index += 1;
                        break;
                    }
                    _ => return Err(CcError::at(loc, "parametres de macro invalides")),
                }
            }
            rest = &rest[index..];
            params = Some(list);
        }
        let mut body: Vec<Token> = rest.to_vec();
        if let Some(first) = body.first_mut() {
            first.space = false;
        }
        self.macros.insert(name.text.clone(), Rc::new(Macro { params, variadic, body }));
        Ok(())
    }

    /// Expands `tokens` on their own: the operand of `#if` and of a
    /// computed `#include`, and the arguments of a macro.
    fn expand_list(&mut self, tokens: Vec<Token>) -> Result<Vec<Token>, CcError> {
        let saved = core::mem::take(&mut self.pending);
        self.pending = tokens;
        self.pending.reverse();
        let mut out = Vec::new();
        let result = (|| {
            while let Some(token) = self.pending.pop() {
                if !self.expand(&token)? {
                    out.push(token);
                }
            }
            Ok(())
        })();
        self.pending = saved;
        result.map(|_| out)
    }

    /// Expands `token` if it names a macro, pushing the result back to be
    /// rescanned. False when it is an ordinary token.
    fn expand(&mut self, token: &Token) -> Result<bool, CcError> {
        if token.kind != Kind::Ident || token.hidden(&token.text) {
            return Ok(false);
        }
        match &*token.text {
            "__LINE__" => {
                self.pending.push(number_token(token.loc.line as i64, token));
                return Ok(true);
            }
            "__FILE__" => {
                let name = self.names[token.loc.file as usize].clone();
                self.pending.push(lex::string_token(&name, token));
                return Ok(true);
            }
            "__COUNTER__" => {
                self.counter += 1;
                self.pending.push(number_token(self.counter - 1, token));
                return Ok(true);
            }
            _ => {}
        }
        let Some(definition) = self.macros.get(&token.text).cloned() else {
            return Ok(false);
        };

        let Some(params) = &definition.params else {
            let mut hide = (*token.hide).clone();
            hide.push(token.text.clone());
            let hide = Rc::new(hide);
            let mut body: Vec<Token> = definition
                .body
                .iter()
                .map(|t| Token { loc: token.loc, bol: false, hide: hide.clone(), ..t.clone() })
                .collect();
            if let Some(first) = body.first_mut() {
                first.space = token.space;
            }
            body.reverse();
            self.pending.extend(body);
            return Ok(true);
        };

        // A function-like macro without `(` is a plain identifier.
        if !self.pending.last().is_some_and(|t| t.is("(")) {
            return Ok(false);
        }
        self.pending.pop();
        let (args, close) = self.arguments(params.len(), definition.variadic, token)?;
        let mut hide: Vec<Rc<str>> = token.hide.iter().filter(|n| close.hidden(n)).cloned().collect();
        hide.push(token.text.clone());
        let hide = Rc::new(hide);
        let mut body = self.substitute(&definition, params, &args, token)?;
        for t in body.iter_mut() {
            t.hide = hide.clone();
            t.loc = token.loc;
            t.bol = false;
        }
        if let Some(first) = body.first_mut() {
            first.space = token.space;
        }
        body.reverse();
        self.pending.extend(body);
        Ok(true)
    }

    /// Reads the arguments of a macro call, the `(` already consumed.
    fn arguments(&mut self, count: usize, variadic: bool, name: &Token) -> Result<(Vec<Vec<Token>>, Token), CcError> {
        let mut args: Vec<Vec<Token>> = alloc::vec![Vec::new()];
        let mut depth = 0;
        loop {
            let Some(token) = self.pending.pop() else {
                return Err(CcError::at(name.loc, &alloc::format!("appel de la macro {} non ferme", name.text)));
            };
            if depth == 0 && token.is(")") {
                if count == 0 && args.len() == 1 && args[0].is_empty() {
                    args.clear();
                }
                if args.len() < count {
                    if variadic && args.len() == count - 1 {
                        args.push(Vec::new());
                    } else {
                        return Err(CcError::at(name.loc, &alloc::format!("macro {} : trop peu d'arguments", name.text)));
                    }
                }
                if args.len() > count {
                    return Err(CcError::at(name.loc, &alloc::format!("macro {} : trop d'arguments", name.text)));
                }
                return Ok((args, token));
            }
            if depth == 0 && token.is(",") && !(variadic && args.len() == count) {
                args.push(Vec::new());
                continue;
            }
            if token.is("(") {
                depth += 1;
            } else if token.is(")") {
                depth -= 1;
            }
            let mut token = token;
            token.bol = false;
            if let Some(last) = args.last_mut() {
                last.push(token);
            }
        }
    }

    fn substitute(&mut self, definition: &Macro, params: &[Rc<str>], args: &[Vec<Token>], at: &Token) -> Result<Vec<Token>, CcError> {
        let find = |t: &Token| if t.kind == Kind::Ident { params.iter().position(|p| *p == t.text) } else { None };
        let body = &definition.body;
        let mut out: Vec<Token> = Vec::new();
        let mut i = 0;
        while i < body.len() {
            let token = &body[i];

            // `#param`: the argument, spelt.
            if token.is("#") {
                let Some(index) = body.get(i + 1).and_then(find) else {
                    return Err(CcError::at(at.loc, "# doit preceder un parametre de la macro"));
                };
                let mut string = lex::string_token(&lex::spell(&args[index]), token);
                string.space = token.space;
                out.push(string);
                i += 2;
                continue;
            }

            // `, ## __VA_ARGS__`: the comma disappears with an empty
            // variadic argument (a GNU extension every C library uses).
            if token.is(",") && body.get(i + 1).is_some_and(|t| t.is("##")) {
                if let Some(index) = body.get(i + 2).and_then(find) {
                    if definition.variadic && index == params.len() - 1 {
                        if !args[index].is_empty() {
                            out.push(token.clone());
                            out.extend(args[index].iter().cloned());
                        }
                        i += 3;
                        continue;
                    }
                }
            }

            // `x ## y`: paste with the left operand already in `out`.
            if token.is("##") {
                if out.is_empty() || i + 1 >= body.len() {
                    return Err(CcError::at(at.loc, "## en bord de macro"));
                }
                let right = &body[i + 1];
                i += 2;
                if let Some(index) = find(right) {
                    let arg = &args[index];
                    if let Some((first, rest)) = arg.split_first() {
                        let left = out.pop().unwrap_or_else(|| unreachable!());
                        out.push(paste(&left, first, at)?);
                        out.extend(rest.iter().cloned());
                    }
                    continue;
                }
                let left = out.pop().unwrap_or_else(|| unreachable!());
                out.push(paste(&left, right, at)?);
                continue;
            }

            if let Some(index) = find(token) {
                let arg = &args[index];
                // Operand of `##`: the argument as written.
                if body.get(i + 1).is_some_and(|t| t.is("##")) {
                    if arg.is_empty() {
                        // An empty left operand: `##` pastes onto nothing,
                        // the right operand stands alone.
                        if let Some(right) = body.get(i + 2) {
                            if let Some(right_index) = find(right) {
                                out.extend(args[right_index].iter().cloned());
                            } else {
                                out.push(right.clone());
                            }
                        }
                        i += 3;
                        continue;
                    }
                    out.extend(arg.iter().cloned());
                    i += 1;
                    continue;
                }
                let mut expanded = self.expand_list(arg.clone())?;
                if let Some(first) = expanded.first_mut() {
                    first.space = token.space;
                }
                out.extend(expanded);
                i += 1;
                continue;
            }

            out.push(token.clone());
            i += 1;
        }
        Ok(out)
    }

    /// The value of a `#if` line.
    fn condition(&mut self, line: Vec<Token>, loc: Loc) -> Result<bool, CcError> {
        // `defined X` and `defined(X)` are replaced before expansion, so
        // that X is never expanded.
        let mut replaced = Vec::new();
        let mut i = 0;
        while i < line.len() {
            let token = &line[i];
            if token.is("defined") {
                let (name, used) = if line.get(i + 1).is_some_and(|t| t.is("(")) {
                    (line.get(i + 2), 4)
                } else {
                    (line.get(i + 1), 2)
                };
                let Some(name) = name.filter(|t| t.kind == Kind::Ident) else {
                    return Err(CcError::at(loc, "defined attend un nom de macro"));
                };
                let value = self.macros.contains_key(&name.text) || is_dynamic(&name.text);
                replaced.push(number_token(value as i64, token));
                i += used;
                continue;
            }
            replaced.push(token.clone());
            i += 1;
        }
        let expanded = self.expand_list(replaced)?;
        let mut evaluator = Evaluator { tokens: &expanded, at: 0, loc };
        let value = evaluator.ternary()?;
        if evaluator.at != expanded.len() {
            return Err(CcError::at(loc, "expression de #if mal formee"));
        }
        Ok(value.0 != 0)
    }
}

fn is_dynamic(name: &str) -> bool {
    matches!(name, "__LINE__" | "__FILE__" | "__COUNTER__")
}

fn paste(left: &Token, right: &Token, at: &Token) -> Result<Token, CcError> {
    let text = alloc::format!("{}{}", left.text, right.text);
    let tokens = lex::tokenize(&text, at.loc.file)?;
    if tokens.len() != 1 {
        return Err(CcError::at(at.loc, &alloc::format!("## ne forme pas un jeton : {}", text)));
    }
    let mut token = tokens.into_iter().next().unwrap_or_else(|| unreachable!());
    token.loc = left.loc;
    token.space = left.space;
    token.bol = false;
    token.hide = left.hide.clone();
    Ok(token)
}

/// Value of an integer constant of `#if`: the value and its
/// unsignedness, which decides `<`, `/` and `>>`.
type Value = (i64, bool);

/// An integer constant, decoded.
pub struct IntLiteral {
    pub value: u64,
    /// `u` suffix.
    pub unsigned: bool,
    /// Number of `l` in the suffix.
    pub longs: u8,
    /// Written in decimal: the candidate types differ (C11 6.4.4.1).
    pub decimal: bool,
}

/// Integer constant: decimal, octal, hexadecimal, binary, with suffixes.
pub fn integer_literal(text: &str) -> Option<IntLiteral> {
    let lower = text.to_ascii_lowercase();
    let (digits, radix) = if let Some(rest) = lower.strip_prefix("0x") {
        (rest, 16)
    } else if let Some(rest) = lower.strip_prefix("0b") {
        (rest, 2)
    } else if lower.len() > 1 && lower.starts_with('0') {
        (&lower[1..], 8)
    } else {
        (lower.as_str(), 10)
    };
    let end = digits.find(|c: char| !c.is_digit(radix)).unwrap_or(digits.len());
    let (number, suffix) = digits.split_at(end);
    let mut value: u64 = 0;
    for c in number.chars() {
        value = value.checked_mul(radix as u64)?.checked_add(c.to_digit(radix)? as u64)?;
    }
    if number.is_empty() && radix != 8 {
        return None;
    }
    let (unsigned, longs) = match suffix {
        "" => (false, 0),
        "u" => (true, 0),
        "l" => (false, 1),
        "ul" | "lu" => (true, 1),
        "ll" => (false, 2),
        "ull" | "llu" => (true, 2),
        _ => return None,
    };
    Some(IntLiteral { value, unsigned, longs, decimal: radix == 10 })
}

/// Value of a character constant (`'a'`, `'\n'`): its byte as a signed
/// `char`, as GCC gives it on x86_64.
pub fn char_literal(token: &Token) -> Result<i64, CcError> {
    let text = &token.text;
    let Some(start) = text.find('\'') else {
        return Err(CcError::at(token.loc, "caractere invalide"));
    };
    if start > 0 {
        return Err(CcError::at(token.loc, "les caracteres larges (L'x') ne font pas partie du C de Nova"));
    }
    let bytes = lex::unescape(&text[1..text.len() - 1], token.loc)?;
    match bytes.as_slice() {
        [] => Err(CcError::at(token.loc, "caractere vide")),
        [single] => Ok(*single as i8 as i64),
        many => Ok(many.iter().fold(0i64, |acc, b| (acc << 8) | *b as i64) as i32 as i64),
    }
}

struct Evaluator<'t> {
    tokens: &'t [Token],
    at: usize,
    loc: Loc,
}

impl Evaluator<'_> {
    fn peek(&self, text: &str) -> bool {
        self.tokens.get(self.at).is_some_and(|t| t.is(text))
    }

    fn eat(&mut self, text: &str) -> bool {
        if self.peek(text) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    fn fail(&self) -> CcError {
        CcError::at(self.loc, "expression de #if invalide")
    }

    fn ternary(&mut self) -> Result<Value, CcError> {
        let condition = self.binary(0)?;
        if !self.eat("?") {
            return Ok(condition);
        }
        let then = self.ternary()?;
        if !self.eat(":") {
            return Err(self.fail());
        }
        let other = self.ternary()?;
        let unsigned = then.1 || other.1;
        Ok(if condition.0 != 0 { (then.0, unsigned) } else { (other.0, unsigned) })
    }

    fn binary(&mut self, min: u8) -> Result<Value, CcError> {
        let mut left = self.unary()?;
        loop {
            let Some(token) = self.tokens.get(self.at) else { break };
            let precedence = match &*token.text {
                "||" => 1,
                "&&" => 2,
                "|" => 3,
                "^" => 4,
                "&" => 5,
                "==" | "!=" => 6,
                "<" | ">" | "<=" | ">=" => 7,
                "<<" | ">>" => 8,
                "+" | "-" => 9,
                "*" | "/" | "%" => 10,
                _ => break,
            };
            if token.kind != Kind::Punct || precedence <= min {
                break;
            }
            let op = token.text.clone();
            self.at += 1;
            let right = self.binary(precedence)?;
            let unsigned = left.1 || right.1;
            let (a, b) = (left.0, right.0);
            let value = match &*op {
                "||" => ((a != 0 || b != 0) as i64, false),
                "&&" => ((a != 0 && b != 0) as i64, false),
                "|" => (a | b, unsigned),
                "^" => (a ^ b, unsigned),
                "&" => (a & b, unsigned),
                "==" => ((a == b) as i64, false),
                "!=" => ((a != b) as i64, false),
                "<" => ((if unsigned { (a as u64) < (b as u64) } else { a < b }) as i64, false),
                ">" => ((if unsigned { (a as u64) > (b as u64) } else { a > b }) as i64, false),
                "<=" => ((if unsigned { (a as u64) <= (b as u64) } else { a <= b }) as i64, false),
                ">=" => ((if unsigned { (a as u64) >= (b as u64) } else { a >= b }) as i64, false),
                "<<" => (a.wrapping_shl(b as u32), left.1),
                ">>" => (if left.1 { ((a as u64) >> (b as u32 & 63)) as i64 } else { a.wrapping_shr(b as u32) }, left.1),
                "+" => (a.wrapping_add(b), unsigned),
                "-" => (a.wrapping_sub(b), unsigned),
                "*" => (a.wrapping_mul(b), unsigned),
                "/" | "%" => {
                    if b == 0 {
                        return Err(CcError::at(self.loc, "division par zero dans #if"));
                    }
                    let value = match (&*op, unsigned) {
                        ("/", true) => ((a as u64) / (b as u64)) as i64,
                        ("/", false) => a.wrapping_div(b),
                        (_, true) => ((a as u64) % (b as u64)) as i64,
                        (_, false) => a.wrapping_rem(b),
                    };
                    (value, unsigned)
                }
                _ => return Err(self.fail()),
            };
            left = value;
        }
        Ok(left)
    }

    fn unary(&mut self) -> Result<Value, CcError> {
        if self.eat("!") {
            let v = self.unary()?;
            return Ok(((v.0 == 0) as i64, false));
        }
        if self.eat("~") {
            let v = self.unary()?;
            return Ok((!v.0, v.1));
        }
        if self.eat("-") {
            let v = self.unary()?;
            return Ok((v.0.wrapping_neg(), v.1));
        }
        if self.eat("+") {
            return self.unary();
        }
        if self.eat("(") {
            let v = self.ternary()?;
            if !self.eat(")") {
                return Err(self.fail());
            }
            return Ok(v);
        }
        let Some(token) = self.tokens.get(self.at) else {
            return Err(self.fail());
        };
        self.at += 1;
        match token.kind {
            // An identifier left after expansion is 0 (C11 6.10.1p4).
            Kind::Ident => Ok((0, false)),
            Kind::Number => {
                let Some(literal) = integer_literal(&token.text) else {
                    return Err(CcError::at(token.loc, &alloc::format!("constante invalide dans #if : {}", token.text)));
                };
                Ok((literal.value as i64, literal.unsigned || literal.value > i64::MAX as u64))
            }
            Kind::Char => Ok((char_literal(token)?, false)),
            _ => Err(self.fail()),
        }
    }
}
