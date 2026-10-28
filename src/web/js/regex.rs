//! Regular expressions: a backtracking virtual machine.
//!
//! JavaScript's regular expressions need backtracking -- backreferences
//! and lookarounds rule out the automaton methods. A recursive matcher
//! would be shortest to write, and would recurse once per character on
//! `.*` over a long text: a page's 100 KB string is 100 000 frames. So the
//! pattern is compiled to instructions, and the machine keeps its choice
//! points on a heap stack of its own. Only lookarounds recurse, as deep as
//! the pattern nests them.
//!
//! **Bounded work.** Some patterns backtrack exponentially (`(a+)+b`
//! against a line of `a`s). The machine counts its steps and gives up --
//! "no match" -- past `STEP_LIMIT`, where a browser would hang the tab.
//!
//! Matching is on Unicode scalar values; positions come back in UTF-16
//! units, which is what `lastIndex` and `index` count in.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

const STEP_LIMIT: usize = 5_000_000;
const MAX_PROGRAM: usize = 50_000;

#[derive(Clone, Copy, Default, Debug)]
pub struct Flags {
    pub global: bool,
    pub ignore_case: bool,
    pub multiline: bool,
    pub dot_all: bool,
    pub unicode: bool,
    pub sticky: bool,
    pub has_indices: bool,
}

impl Flags {
    pub fn parse(text: &str) -> Option<Flags> {
        let mut flags = Flags::default();
        for c in text.chars() {
            let slot = match c {
                'g' => &mut flags.global,
                'i' => &mut flags.ignore_case,
                'm' => &mut flags.multiline,
                's' => &mut flags.dot_all,
                'u' | 'v' => &mut flags.unicode,
                'y' => &mut flags.sticky,
                'd' => &mut flags.has_indices,
                _ => return None,
            };
            if *slot {
                return None;
            }
            *slot = true;
        }
        Some(flags)
    }
}

#[derive(Clone, Debug)]
enum ClassItem {
    Range(char, char),
    Digit(bool),
    Word(bool),
    Space(bool),
    /// `\p{...}`: a coarse Unicode category, negated or not.
    Property(Category, bool),
}

#[derive(Clone, Copy, Debug)]
enum Category {
    Letter,
    Upper,
    Lower,
    Number,
    /// `Nd`: decimal digits of every script.
    Decimal,
    Punctuation,
    Any,
}

#[derive(Clone, Debug)]
struct Class {
    items: Vec<ClassItem>,
    negated: bool,
}

#[derive(Clone, Debug)]
enum Node {
    Empty,
    Char(char),
    Any,
    Class(Class),
    Start,
    End,
    Boundary(bool),
    Group(Box<Node>, Option<usize>),
    Concat(Vec<Node>),
    Alt(Vec<Node>),
    Repeat(Box<Node>, usize, Option<usize>, bool),
    Backref(usize),
    NamedBackref(String),
    Look(Box<Node>, bool, bool),
}

#[derive(Clone, Debug)]
enum Inst {
    Char(char),
    Any,
    Class(usize),
    Start,
    End,
    Boundary(bool),
    /// Try the first target, backtrack into the second.
    Split(usize, usize),
    Jmp(usize),
    Save(usize),
    Backref(usize),
    /// Records the position in a register (loop entry).
    Mark(usize),
    /// Fails if the position has not moved since the mark: an empty
    /// iteration would loop forever.
    Progress(usize),
    /// Lookaround: sub-program [start, end), ahead?, negated?
    Look(usize, usize, bool, bool),
    Match,
}

pub struct Regex {
    pub source: String,
    pub flags: Flags,
    program: Vec<Inst>,
    classes: Vec<Class>,
    /// Capture groups, group 0 excluded.
    pub groups: usize,
    pub names: Vec<(String, usize)>,
    registers: usize,
}

impl core::fmt::Debug for Regex {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "/{}/", self.source)
    }
}

// ============================================================
//  Parsing
// ============================================================

struct Parser<'a> {
    chars: Vec<char>,
    pos: usize,
    groups: usize,
    names: Vec<(String, usize)>,
    unicode: bool,
    depth: usize,
    _source: &'a str,
}

type PResult<T> = Result<T, String>;

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn eat(&mut self, c: char) -> bool {
        if self.peek() == Some(c) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn alternation(&mut self) -> PResult<Node> {
        self.depth += 1;
        if self.depth > 200 {
            return Err(String::from("expression trop imbriquee"));
        }
        let mut branches = alloc::vec![self.sequence()?];
        while self.eat('|') {
            branches.push(self.sequence()?);
        }
        self.depth -= 1;
        Ok(if branches.len() == 1 { branches.pop().unwrap_or(Node::Empty) } else { Node::Alt(branches) })
    }

    fn sequence(&mut self) -> PResult<Node> {
        let mut items = Vec::new();
        while let Some(c) = self.peek() {
            if c == '|' || c == ')' {
                break;
            }
            let atom = self.atom()?;
            let atom = self.quantifier(atom)?;
            items.push(atom);
        }
        Ok(if items.len() == 1 { items.pop().unwrap_or(Node::Empty) } else { Node::Concat(items) })
    }

    fn number(&mut self) -> Option<usize> {
        let start = self.pos;
        let mut value: usize = 0;
        while let Some(d) = self.peek().and_then(|c| c.to_digit(10)) {
            value = value.saturating_mul(10).saturating_add(d as usize);
            self.pos += 1;
        }
        if self.pos == start {
            None
        } else {
            Some(value)
        }
    }

    fn quantifier(&mut self, atom: Node) -> PResult<Node> {
        let (min, max) = match self.peek() {
            Some('*') => {
                self.pos += 1;
                (0, None)
            }
            Some('+') => {
                self.pos += 1;
                (1, None)
            }
            Some('?') => {
                self.pos += 1;
                (0, Some(1))
            }
            Some('{') => {
                let mark = self.pos;
                self.pos += 1;
                let Some(min) = self.number() else {
                    self.pos = mark;
                    return Ok(atom);
                };
                let max = if self.eat(',') { self.number() } else { Some(min) };
                if !self.eat('}') {
                    // Not a quantifier: a literal brace (Annex B).
                    self.pos = mark;
                    return Ok(atom);
                }
                if max.is_some_and(|m| m < min) {
                    return Err(String::from("quantificateur dans le desordre"));
                }
                (min.min(1000), max.map(|m| m.min(1000)))
            }
            _ => return Ok(atom),
        };
        let greedy = !self.eat('?');
        if matches!(atom, Node::Start | Node::End | Node::Boundary(_)) {
            return Ok(atom);
        }
        Ok(Node::Repeat(Box::new(atom), min, max, greedy))
    }

    fn atom(&mut self) -> PResult<Node> {
        let Some(c) = self.peek() else {
            return Ok(Node::Empty);
        };
        self.pos += 1;
        match c {
            '.' => Ok(Node::Any),
            '^' => Ok(Node::Start),
            '$' => Ok(Node::End),
            '(' => {
                if self.eat('?') {
                    if self.eat(':') {
                        let inner = self.alternation()?;
                        self.close()?;
                        return Ok(Node::Group(Box::new(inner), None));
                    }
                    if self.eat('=') || self.eat('!') {
                        let negate = self.chars[self.pos - 1] == '!';
                        let inner = self.alternation()?;
                        self.close()?;
                        return Ok(Node::Look(Box::new(inner), true, negate));
                    }
                    if self.eat('<') {
                        if self.eat('=') || self.eat('!') {
                            let negate = self.chars[self.pos - 1] == '!';
                            let inner = self.alternation()?;
                            self.close()?;
                            return Ok(Node::Look(Box::new(inner), false, negate));
                        }
                        let mut name = String::new();
                        while let Some(c) = self.peek() {
                            self.pos += 1;
                            if c == '>' {
                                break;
                            }
                            name.push(c);
                        }
                        self.groups += 1;
                        let index = self.groups;
                        self.names.push((name, index));
                        let inner = self.alternation()?;
                        self.close()?;
                        return Ok(Node::Group(Box::new(inner), Some(index)));
                    }
                    return Err(String::from("groupe invalide"));
                }
                self.groups += 1;
                let index = self.groups;
                let inner = self.alternation()?;
                self.close()?;
                Ok(Node::Group(Box::new(inner), Some(index)))
            }
            ')' => Err(String::from("parenthese fermante en trop")),
            '[' => Ok(Node::Class(self.class()?)),
            '\\' => self.escape(),
            '*' | '+' | '?' => Err(String::from("rien a repeter")),
            other => Ok(Node::Char(other)),
        }
    }

    fn close(&mut self) -> PResult<()> {
        if self.eat(')') {
            Ok(())
        } else {
            Err(String::from("parenthese non fermee"))
        }
    }

    fn hex(&mut self, count: usize) -> Option<u32> {
        let text: String = self.chars.get(self.pos..self.pos + count)?.iter().collect();
        let value = u32::from_str_radix(&text, 16).ok()?;
        self.pos += count;
        Some(value)
    }

    /// A character escape shared by atoms and classes; `None` when the
    /// escape is a class (\d, \w...), given back as a class item.
    fn char_escape(&mut self) -> PResult<Result<char, ClassItem>> {
        let Some(c) = self.peek() else {
            return Err(String::from("\\ a la fin"));
        };
        self.pos += 1;
        Ok(Ok(match c {
            'd' => return Ok(Err(ClassItem::Digit(false))),
            'D' => return Ok(Err(ClassItem::Digit(true))),
            'w' => return Ok(Err(ClassItem::Word(false))),
            'W' => return Ok(Err(ClassItem::Word(true))),
            's' => return Ok(Err(ClassItem::Space(false))),
            'S' => return Ok(Err(ClassItem::Space(true))),
            'p' | 'P' if self.peek() == Some('{') => {
                let negated = c == 'P';
                let mut name = String::new();
                self.pos += 1;
                while let Some(c) = self.peek() {
                    self.pos += 1;
                    if c == '}' {
                        break;
                    }
                    name.push(c);
                }
                let name = name.rsplit('=').next().unwrap_or("").to_ascii_lowercase();
                let category = match name.as_str() {
                    "l" | "letter" | "alphabetic" | "alpha" => Category::Letter,
                    "lu" | "uppercase_letter" | "uppercase" => Category::Upper,
                    "ll" | "lowercase_letter" | "lowercase" => Category::Lower,
                    "n" | "number" => Category::Number,
                    "nd" | "decimal_number" | "digit" => Category::Decimal,
                    "p" | "punctuation" => Category::Punctuation,
                    _ => Category::Any,
                };
                return Ok(Err(ClassItem::Property(category, negated)));
            }
            't' => '\t',
            'n' => '\n',
            'r' => '\r',
            'v' => '\u{b}',
            'f' => '\u{c}',
            '0' if !self.peek().is_some_and(|d| d.is_ascii_digit()) => '\0',
            'x' => match self.hex(2) {
                Some(code) => char::from_u32(code).unwrap_or('x'),
                None => 'x',
            },
            'u' => {
                if self.peek() == Some('{') {
                    let mark = self.pos;
                    self.pos += 1;
                    let mut text = String::new();
                    while let Some(c) = self.peek() {
                        self.pos += 1;
                        if c == '}' {
                            break;
                        }
                        text.push(c);
                    }
                    match u32::from_str_radix(&text, 16).ok().and_then(char::from_u32) {
                        Some(c) => c,
                        None => {
                            self.pos = mark;
                            'u'
                        }
                    }
                } else {
                    match self.hex(4) {
                        Some(high) if (0xd800..0xdc00).contains(&high) => {
                            // A surrogate pair written as two escapes.
                            if self.chars.get(self.pos) == Some(&'\\') && self.chars.get(self.pos + 1) == Some(&'u') {
                                let mark = self.pos;
                                self.pos += 2;
                                match self.hex(4) {
                                    Some(low) if (0xdc00..0xe000).contains(&low) => {
                                        char::from_u32(0x10000 + ((high - 0xd800) << 10) + (low - 0xdc00)).unwrap_or('\u{fffd}')
                                    }
                                    _ => {
                                        self.pos = mark;
                                        '\u{fffd}'
                                    }
                                }
                            } else {
                                '\u{fffd}'
                            }
                        }
                        Some(code) => char::from_u32(code).unwrap_or('\u{fffd}'),
                        None => 'u',
                    }
                }
            }
            'c' => match self.peek() {
                Some(letter) if letter.is_ascii_alphabetic() => {
                    self.pos += 1;
                    char::from_u32(letter as u32 % 32).unwrap_or('c')
                }
                _ => {
                    self.pos -= 1;
                    '\\'
                }
            },
            other => other,
        }))
    }

    fn escape(&mut self) -> PResult<Node> {
        match self.peek() {
            Some('b') => {
                self.pos += 1;
                Ok(Node::Boundary(true))
            }
            Some('B') => {
                self.pos += 1;
                Ok(Node::Boundary(false))
            }
            Some(d) if d.is_ascii_digit() && d != '0' => {
                let mark = self.pos;
                let number = self.number().unwrap_or(0);
                if number <= self.groups.max(9) {
                    Ok(Node::Backref(number))
                } else {
                    // Annex B: an octal escape.
                    self.pos = mark;
                    let mut value = 0u32;
                    while let Some(d) = self.peek().and_then(|c| c.to_digit(8)) {
                        if value * 8 + d > 255 {
                            break;
                        }
                        value = value * 8 + d;
                        self.pos += 1;
                    }
                    Ok(Node::Char(char::from_u32(value).unwrap_or('\0')))
                }
            }
            Some('k') if self.chars.get(self.pos + 1) == Some(&'<') => {
                self.pos += 2;
                let mut name = String::new();
                while let Some(c) = self.peek() {
                    self.pos += 1;
                    if c == '>' {
                        break;
                    }
                    name.push(c);
                }
                Ok(Node::NamedBackref(name))
            }
            _ => match self.char_escape()? {
                Ok(c) => Ok(Node::Char(c)),
                Err(item) => Ok(Node::Class(Class { items: alloc::vec![item], negated: false })),
            },
        }
    }

    fn class(&mut self) -> PResult<Class> {
        let negated = self.eat('^');
        let mut items = Vec::new();
        let mut first = true;
        loop {
            let Some(c) = self.peek() else {
                return Err(String::from("classe non fermee"));
            };
            if c == ']' && !first {
                self.pos += 1;
                break;
            }
            if c == ']' && first {
                // `[]` matches nothing, `[^]` anything.
                self.pos += 1;
                break;
            }
            first = false;
            self.pos += 1;
            let low = if c == '\\' {
                if self.peek() == Some('b') {
                    self.pos += 1;
                    '\u{8}'
                } else {
                    match self.char_escape()? {
                        Ok(c) => c,
                        Err(item) => {
                            items.push(item);
                            continue;
                        }
                    }
                }
            } else {
                c
            };
            if self.peek() == Some('-') && self.chars.get(self.pos + 1).is_some_and(|c| *c != ']') {
                self.pos += 1;
                let Some(c) = self.peek() else {
                    return Err(String::from("classe non fermee"));
                };
                self.pos += 1;
                let high = if c == '\\' {
                    match self.char_escape()? {
                        Ok(c) => c,
                        Err(item) => {
                            // `[a-\d]`: the dash is literal (Annex B).
                            items.push(ClassItem::Range(low, low));
                            items.push(ClassItem::Range('-', '-'));
                            items.push(item);
                            continue;
                        }
                    }
                } else {
                    c
                };
                if high < low {
                    return Err(String::from("intervalle dans le desordre"));
                }
                items.push(ClassItem::Range(low, high));
            } else {
                items.push(ClassItem::Range(low, low));
            }
        }
        Ok(Class { items, negated })
    }
}

// ============================================================
//  Compilation
// ============================================================

struct Compiler {
    program: Vec<Inst>,
    classes: Vec<Class>,
    registers: usize,
    groups: usize,
    names: Vec<(String, usize)>,
}

impl Compiler {
    fn emit(&mut self, inst: Inst) -> PResult<usize> {
        if self.program.len() >= MAX_PROGRAM {
            return Err(String::from("expression trop grande"));
        }
        self.program.push(inst);
        Ok(self.program.len() - 1)
    }

    fn compile(&mut self, node: &Node) -> PResult<()> {
        match node {
            Node::Empty => {}
            Node::Char(c) => {
                self.emit(Inst::Char(*c))?;
            }
            Node::Any => {
                self.emit(Inst::Any)?;
            }
            Node::Class(class) => {
                self.classes.push(class.clone());
                self.emit(Inst::Class(self.classes.len() - 1))?;
            }
            Node::Start => {
                self.emit(Inst::Start)?;
            }
            Node::End => {
                self.emit(Inst::End)?;
            }
            Node::Boundary(word) => {
                self.emit(Inst::Boundary(*word))?;
            }
            Node::Group(inner, index) => match index {
                Some(index) => {
                    self.emit(Inst::Save(index * 2))?;
                    self.compile(inner)?;
                    self.emit(Inst::Save(index * 2 + 1))?;
                }
                None => self.compile(inner)?,
            },
            Node::Concat(items) => {
                for item in items {
                    self.compile(item)?;
                }
            }
            Node::Alt(branches) => {
                let mut jumps = Vec::new();
                for (index, branch) in branches.iter().enumerate() {
                    if index + 1 < branches.len() {
                        let split = self.emit(Inst::Split(0, 0))?;
                        self.compile(branch)?;
                        jumps.push(self.emit(Inst::Jmp(0))?);
                        let next = self.program.len();
                        self.program[split] = Inst::Split(split + 1, next);
                    } else {
                        self.compile(branch)?;
                    }
                }
                let end = self.program.len();
                for jump in jumps {
                    self.program[jump] = Inst::Jmp(end);
                }
            }
            Node::Repeat(inner, min, max, greedy) => {
                // Captures inside are reset at each iteration.
                for _ in 0..*min {
                    self.compile(inner)?;
                }
                match max {
                    None => {
                        let register = self.registers;
                        self.registers += 1;
                        let split = self.emit(Inst::Split(0, 0))?;
                        self.emit(Inst::Mark(register))?;
                        self.compile(inner)?;
                        self.emit(Inst::Progress(register))?;
                        self.emit(Inst::Jmp(split))?;
                        let end = self.program.len();
                        self.program[split] = if *greedy { Inst::Split(split + 1, end) } else { Inst::Split(end, split + 1) };
                    }
                    Some(max) => {
                        let mut splits = Vec::new();
                        for _ in *min..*max {
                            let split = self.emit(Inst::Split(0, 0))?;
                            splits.push(split);
                            self.compile(inner)?;
                        }
                        let end = self.program.len();
                        for split in splits {
                            self.program[split] = if *greedy { Inst::Split(split + 1, end) } else { Inst::Split(end, split + 1) };
                        }
                    }
                }
            }
            Node::Backref(index) => {
                self.emit(Inst::Backref(*index))?;
            }
            Node::NamedBackref(name) => {
                let index = self.names.iter().find(|(n, _)| n == name).map(|(_, i)| *i).unwrap_or(0);
                self.emit(Inst::Backref(index))?;
            }
            Node::Look(inner, ahead, negate) => {
                let look = self.emit(Inst::Look(0, 0, *ahead, *negate))?;
                let start = self.program.len();
                self.compile(inner)?;
                self.emit(Inst::Match)?;
                let end = self.program.len();
                self.program[look] = Inst::Look(start, end, *ahead, *negate);
            }
        }
        let _ = self.groups;
        Ok(())
    }
}

// ============================================================
//  Matching
// ============================================================

/// A text prepared for matching: its characters and, when it is not all
/// ASCII, the UTF-16 offset of each.
pub struct Input {
    pub chars: Vec<char>,
    units: Option<Vec<usize>>,
}

impl Input {
    pub fn new(text: &str) -> Input {
        let chars: Vec<char> = text.chars().collect();
        let units = if text.is_ascii() {
            None
        } else {
            let mut offsets = Vec::with_capacity(chars.len() + 1);
            let mut position = 0;
            for c in &chars {
                offsets.push(position);
                position += c.len_utf16();
            }
            offsets.push(position);
            Some(offsets)
        };
        Input { chars, units }
    }

    /// UTF-16 offset of character index `index`.
    pub fn unit(&self, index: usize) -> usize {
        match &self.units {
            None => index,
            Some(units) => units[index.min(units.len() - 1)],
        }
    }

    /// Character index at or after UTF-16 offset `unit`.
    pub fn index_of_unit(&self, unit: usize) -> usize {
        match &self.units {
            None => unit.min(self.chars.len()),
            Some(units) => units.partition_point(|u| *u < unit).min(self.chars.len()),
        }
    }

    pub fn len(&self) -> usize {
        self.chars.len()
    }

    pub fn is_empty(&self) -> bool {
        self.chars.is_empty()
    }
}

/// Unicode `Nd`: numeric characters less the other numbers (`No`, `Nl`:
/// superscripts, fractions, roman numerals, circled numbers...).
fn is_decimal(c: char) -> bool {
    if c.is_ascii_digit() {
        return true;
    }
    if !c.is_numeric() || c.is_ascii() {
        return false;
    }
    let u = c as u32;
    let other = matches!(u,
        0xb2 | 0xb3 | 0xb9 | 0xbc..=0xbe | 0x9f4..=0x9f9 | 0xb72..=0xb77 | 0xbf0..=0xbf2 | 0xc78..=0xc7e
        | 0xd58..=0xd5e | 0xd70..=0xd78 | 0xf2a..=0xf33 | 0x1369..=0x137c | 0x16ee..=0x16f0 | 0x17f0..=0x17f9
        | 0x19da | 0x2070..=0x209f | 0x2150..=0x218f | 0x2460..=0x24ff | 0x2776..=0x2793 | 0x2cfd | 0x3007
        | 0x3021..=0x3029 | 0x3038..=0x303a | 0x3192..=0x3195 | 0x3220..=0x3229 | 0x3248..=0x324f
        | 0x3251..=0x325f | 0x3280..=0x3289 | 0x32b1..=0x32bf | 0xa6e6..=0xa6ef | 0xa830..=0xa835
        | 0x10107..=0x10133 | 0x10140..=0x10178 | 0x1018a..=0x1018b | 0x10e60..=0x10e7e | 0x1f100..=0x1f10c);
    !other
}

fn is_word(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

fn is_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r' | '\u{b}' | '\u{c}' | '\u{a0}' | '\u{feff}' | '\u{1680}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}')
        || ('\u{2000}'..='\u{200a}').contains(&c)
}

fn is_line_terminator(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

fn fold(c: char) -> char {
    if c.is_ascii() {
        return c.to_ascii_lowercase();
    }
    c.to_lowercase().next().unwrap_or(c)
}

impl Class {
    fn matches(&self, c: char, ignore_case: bool) -> bool {
        let hit = |c: char| {
            self.items.iter().any(|item| match item {
                ClassItem::Range(low, high) => *low <= c && c <= *high,
                ClassItem::Digit(negated) => c.is_ascii_digit() != *negated,
                ClassItem::Word(negated) => is_word(c) != *negated,
                ClassItem::Space(negated) => is_space(c) != *negated,
                ClassItem::Property(category, negated) => {
                    let yes = match category {
                        Category::Letter => c.is_alphabetic(),
                        Category::Upper => c.is_uppercase(),
                        Category::Lower => c.is_lowercase(),
                        Category::Number => c.is_numeric(),
                        Category::Decimal => is_decimal(c),
                        Category::Punctuation => c.is_ascii_punctuation() || (!c.is_alphanumeric() && !c.is_whitespace() && !c.is_control()),
                        Category::Any => true,
                    };
                    yes != *negated
                }
            })
        };
        let mut found = hit(c);
        if !found && ignore_case {
            let lower = fold(c);
            let upper = c.to_uppercase().next().unwrap_or(c);
            found = hit(lower) || hit(upper);
        }
        found != self.negated
    }
}

enum Backtrack {
    /// Resume at (pc, position).
    Resume(usize, usize),
    /// Restore a slot.
    Undo(usize, Option<usize>),
}

impl Regex {
    pub fn new(pattern: &str, flags: &str) -> Result<Regex, String> {
        let Some(parsed_flags) = Flags::parse(flags) else {
            return Err(alloc::format!("Invalid regular expression flags '{}'", flags));
        };
        let mut parser = Parser {
            chars: pattern.chars().collect(),
            pos: 0,
            groups: 0,
            names: Vec::new(),
            unicode: parsed_flags.unicode,
            depth: 0,
            _source: pattern,
        };
        // Count the groups first, so that `\2` before group 2 is known to
        // be a backreference.
        parser.groups = 0;
        let node = parser.alternation().map_err(|e| alloc::format!("Invalid regular expression: /{}/: {}", pattern, e))?;
        if parser.pos < parser.chars.len() {
            return Err(alloc::format!("Invalid regular expression: /{}/: parenthese fermante en trop", pattern));
        }
        let _ = parser.unicode;
        let mut compiler = Compiler { program: Vec::new(), classes: Vec::new(), registers: 0, groups: parser.groups, names: parser.names.clone() };
        compiler.emit(Inst::Save(0))?;
        compiler.compile(&node)?;
        compiler.emit(Inst::Save(1))?;
        compiler.emit(Inst::Match)?;
        Ok(Regex {
            source: String::from(pattern),
            flags: parsed_flags,
            program: compiler.program,
            classes: compiler.classes,
            groups: parser.groups,
            names: parser.names,
            registers: compiler.registers,
        })
    }

    /// Tries a match starting exactly at character `start`. Returns the
    /// capture slots in character indices.
    fn match_at(&self, input: &Input, start: usize, steps: &mut usize) -> Option<Vec<Option<usize>>> {
        let slot_count = (self.groups + 1) * 2 + self.registers;
        let mut slots: Vec<Option<usize>> = alloc::vec![None; slot_count];
        if self.run(input, 0, self.program.len(), start, &mut slots, steps, None).is_some() {
            Some(slots)
        } else {
            None
        }
    }

    /// Runs program [pc, end) from `position`. `must_end` constrains the
    /// final position (lookbehind). Returns the end position.
    #[allow(clippy::too_many_arguments)]
    fn run(
        &self,
        input: &Input,
        pc: usize,
        _end: usize,
        position: usize,
        slots: &mut [Option<usize>],
        steps: &mut usize,
        must_end: Option<usize>,
    ) -> Option<usize> {
        let text = &input.chars;
        let ignore_case = self.flags.ignore_case;
        let register_base = (self.groups + 1) * 2;
        let mut stack: Vec<Backtrack> = Vec::new();
        let mut pc = pc;
        let mut position = position;
        loop {
            *steps += 1;
            if *steps > STEP_LIMIT {
                return None;
            }
            let ok = match &self.program[pc] {
                Inst::Char(c) => match text.get(position) {
                    Some(actual) if *actual == *c || (ignore_case && fold(*actual) == fold(*c)) => {
                        position += 1;
                        pc += 1;
                        true
                    }
                    _ => false,
                },
                Inst::Any => match text.get(position) {
                    Some(c) if self.flags.dot_all || !is_line_terminator(*c) => {
                        position += 1;
                        pc += 1;
                        true
                    }
                    _ => false,
                },
                Inst::Class(index) => match text.get(position) {
                    Some(c) if self.classes[*index].matches(*c, ignore_case) => {
                        position += 1;
                        pc += 1;
                        true
                    }
                    _ => false,
                },
                Inst::Start => {
                    let ok = position == 0 || (self.flags.multiline && is_line_terminator(text[position - 1]));
                    pc += 1;
                    ok
                }
                Inst::End => {
                    let ok = position == text.len() || (self.flags.multiline && is_line_terminator(text[position]));
                    pc += 1;
                    ok
                }
                Inst::Boundary(want) => {
                    let before = position > 0 && is_word(text[position - 1]);
                    let after = position < text.len() && is_word(text[position]);
                    pc += 1;
                    (before != after) == *want
                }
                Inst::Split(first, second) => {
                    stack.push(Backtrack::Resume(*second, position));
                    pc = *first;
                    true
                }
                Inst::Jmp(target) => {
                    pc = *target;
                    true
                }
                Inst::Save(slot) => {
                    stack.push(Backtrack::Undo(*slot, slots[*slot]));
                    slots[*slot] = Some(position);
                    pc += 1;
                    true
                }
                Inst::Mark(register) => {
                    let slot = register_base + register;
                    stack.push(Backtrack::Undo(slot, slots[slot]));
                    slots[slot] = Some(position);
                    pc += 1;
                    true
                }
                Inst::Progress(register) => {
                    let slot = register_base + register;
                    pc += 1;
                    slots[slot] != Some(position)
                }
                Inst::Backref(group) => {
                    let (start, stop) = (slots.get(group * 2).copied().flatten(), slots.get(group * 2 + 1).copied().flatten());
                    match (start, stop) {
                        (Some(start), Some(stop)) if stop >= start => {
                            let length = stop - start;
                            let fits = position + length <= text.len()
                                && (0..length).all(|i| {
                                    let (a, b) = (text[start + i], text[position + i]);
                                    a == b || (ignore_case && fold(a) == fold(b))
                                });
                            if fits {
                                position += length;
                            }
                            pc += 1;
                            fits
                        }
                        // An unset group matches the empty string.
                        _ => {
                            pc += 1;
                            true
                        }
                    }
                }
                Inst::Look(start, end, ahead, negate) => {
                    let found = if *ahead {
                        let mut inner = slots.to_vec();
                        let hit = self.run(input, *start, *end, position, &mut inner, steps, None).is_some();
                        if hit && !negate {
                            slots.copy_from_slice(&inner);
                        }
                        hit
                    } else {
                        let mut hit = false;
                        for from in (0..=position).rev() {
                            let mut inner = slots.to_vec();
                            if self.run(input, *start, *end, from, &mut inner, steps, Some(position)).is_some() {
                                if !negate {
                                    slots.copy_from_slice(&inner);
                                }
                                hit = true;
                                break;
                            }
                            if *steps > STEP_LIMIT {
                                break;
                            }
                        }
                        hit
                    };
                    pc = *end;
                    found != *negate
                }
                Inst::Match => {
                    if must_end.is_none_or(|e| e == position) {
                        return Some(position);
                    }
                    false
                }
            };
            if ok {
                continue;
            }
            // Backtrack.
            loop {
                match stack.pop() {
                    None => return None,
                    Some(Backtrack::Undo(slot, value)) => slots[slot] = value,
                    Some(Backtrack::Resume(target, at)) => {
                        pc = target;
                        position = at;
                        break;
                    }
                }
            }
        }
    }

    /// Searches from character `start` (only there when sticky). Returns
    /// the captures as character ranges.
    pub fn exec_chars(&self, input: &Input, start: usize) -> Option<Vec<Option<(usize, usize)>>> {
        let mut steps = 0;
        let last = if self.flags.sticky { start } else { input.len() };
        // A literal first character lets most start positions be skipped.
        let first = match self.program.get(1) {
            Some(Inst::Char(c)) if !self.flags.ignore_case => Some(*c),
            _ => None,
        };
        let mut from = start;
        while from <= last {
            if let Some(c) = first {
                match input.chars[from.min(input.len())..].iter().position(|x| *x == c) {
                    Some(offset) => from += offset,
                    None => return None,
                }
                if from > last {
                    return None;
                }
            }
            if let Some(slots) = self.match_at(input, from, &mut steps) {
                let captures = (0..=self.groups)
                    .map(|g| match (slots[g * 2], slots[g * 2 + 1]) {
                        (Some(a), Some(b)) if b >= a => Some((a, b)),
                        _ => None,
                    })
                    .collect();
                return Some(captures);
            }
            if steps > STEP_LIMIT {
                return None;
            }
            from += 1;
        }
        None
    }
}
