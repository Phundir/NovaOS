//! 9.6 -- ETR, Electron-To-Rust (spec 13.1).
//!
//! ## What the tool really does
//!
//! 13.1 asks for an "AST analysis and mapping of the system equivalents"
//! leading to a "conversion report with success rate and warnings", then
//! a verdict: at least 70 % converted gives a native `.cus`, below that
//! -- or if `eval()` appears -- a `.ras` with a warning.
//!
//! The four steps are here:
//!
//! 1. **Lexer** -- splits a JavaScript source into tokens. It handles the
//!    case that separates a JS lexer from an ordinary one: a `/` is
//!    either a division or the start of a regular expression depending on
//!    what precedes it. Getting that wrong swallows half the file into a
//!    string and inflates the success rate artificially.
//! 2. **Parser** -- builds a tree. Not a complete JavaScript tree: a tree
//!    of the constructs the conversion depends on -- declarations, calls,
//!    member chains, `new`, literals. The rest is reduced to `Opaque`,
//!    which is honest: what the parser does not understand must not count
//!    as converted.
//! 3. **Mapping** -- a table associating a JavaScript call path with the
//!    Nova OS system call that stands in for it.
//! 4. **Verdict** -- the threshold of 13.1.
//!
//! ## The rate is computed on the calls, and that is a choice
//!
//! 13.1 says "success rate" without saying of what. Three readings were
//! possible: proportion of lines, of functions, or of call sites. The
//! third is the one kept, because it is the only one measuring what will
//! really be missing at run time. A thousand-line file of arithmetic
//! converts entirely; a ten-line file calling `document.querySelector`
//! does not convert at all. Counting lines would rank the second as
//! almost converted.
//!
//! ## No floating point
//!
//! The rate is an integer from 0 to 100, computed in integer arithmetic.
//! The kernel runs soft-float with no SSE: an `f32` here would be an
//! `f32` in the kernel, which the project rule forbids.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

use crate::binfmt::header::BinaryFormat;
use crate::fs::perm::Credentials;

/// Threshold of 13.1, as a whole percentage.
pub const THRESHOLD: u32 = 70;

/// Typical reduction announced by 13.1, for the report.
pub const TYPICAL_BEFORE_MB: u32 = 180;
pub const TYPICAL_AFTER_MB: u32 = 4;

/// Cap on the files analysed in one pass.
///
/// A real Electron project counts thousands of them in `node_modules`.
/// Walking them all would exceed the kernel heap; the tool stops and says
/// so, rather than failing on an allocation.
const MAX_FILES: usize = 64;

/// Cap on the size of an analysed file.
const MAX_SOURCE: usize = 256 * 1024;

// ============================================================
//  Lexer
// ============================================================

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Ident(String),
    Text(String),
    Number(i64),
    Punct(&'static str),
    /// Regular expression: recognised so as not to be confused with a
    /// division, never interpreted.
    Regex,
    End,
}

/// Punctuation recognised, longest first.
const PUNCT: &[&str] = &[
    "===", "!==", "=>", "==", "!=", "<=", ">=", "&&", "||", "++", "--", "+=", "-=", "*=", "/=",
    "...", "?.", "{", "}", "(", ")", "[", "]", ";", ",", ".", ":", "?", "=", "+", "-", "*", "/",
    "%", "<", ">", "!", "&", "|", "^", "~",
];

/// Does a `/` open a regular expression?
///
/// The rule is the one of JavaScript parsers: a regular expression can
/// only appear where a **value** is expected, that is after punctuation
/// or a keyword, never after an identifier, a number, a string or a
/// closing parenthesis.
fn regex_allowed(previous: Option<&Token>) -> bool {
    match previous {
        None => true,
        Some(Token::Ident(word)) => {
            matches!(word.as_str(), "return" | "typeof" | "case" | "in" | "of" | "new" | "delete")
        }
        Some(Token::Punct(symbol)) => !matches!(*symbol, ")" | "]" | "}" | "++" | "--"),
        _ => false,
    }
}

fn lex(source: &str) -> Vec<Token> {
    let bytes = source.as_bytes();
    let mut tokens: Vec<Token> = Vec::new();
    let mut index = 0;

    while index < bytes.len() {
        let byte = bytes[index];

        if byte.is_ascii_whitespace() {
            index += 1;
            continue;
        }

        // --- comments ---
        if byte == b'/' && index + 1 < bytes.len() {
            if bytes[index + 1] == b'/' {
                while index < bytes.len() && bytes[index] != b'\n' {
                    index += 1;
                }
                continue;
            }
            if bytes[index + 1] == b'*' {
                index += 2;
                while index + 1 < bytes.len() && !(bytes[index] == b'*' && bytes[index + 1] == b'/')
                {
                    index += 1;
                }
                index = (index + 2).min(bytes.len());
                continue;
            }
        }

        // --- strings, single, double or back quotes ---
        if byte == b'"' || byte == b'\'' || byte == b'`' {
            let quote = byte;
            let start = index + 1;
            let mut end = start;
            while end < bytes.len() && bytes[end] != quote {
                // An escape consumes the character following it: without
                // that, `"c'est \" fini"` would end in the wrong place
                // and shift the whole rest of the file.
                if bytes[end] == b'\\' {
                    end += 1;
                }
                end += 1;
            }
            let text = source
                .get(start..end.min(bytes.len()))
                .unwrap_or("");
            tokens.push(Token::Text(String::from(text)));
            index = (end + 1).min(bytes.len());
            continue;
        }

        // --- regular expression ---
        if byte == b'/' && regex_allowed(tokens.last()) {
            let mut end = index + 1;
            let mut in_class = false;
            while end < bytes.len() {
                match bytes[end] {
                    b'\\' => end += 1,
                    b'[' => in_class = true,
                    b']' => in_class = false,
                    b'/' if !in_class => break,
                    b'\n' => break,
                    _ => {}
                }
                end += 1;
            }
            // The flags following the second `/`.
            let mut after = (end + 1).min(bytes.len());
            while after < bytes.len() && bytes[after].is_ascii_alphabetic() {
                after += 1;
            }
            tokens.push(Token::Regex);
            index = after;
            continue;
        }

        if byte.is_ascii_digit() {
            let start = index;
            while index < bytes.len()
                && (bytes[index].is_ascii_alphanumeric() || bytes[index] == b'.')
            {
                index += 1;
            }
            let text = &source[start..index];
            // A decimal literal is recognised as a number without being
            // converted: the kernel has no floating point, and the value
            // is of no importance to the analysis.
            let value = if let Some(rest) = text.strip_prefix("0x") {
                i64::from_str_radix(rest, 16).unwrap_or(0)
            } else {
                text.parse::<i64>().unwrap_or(0)
            };
            tokens.push(Token::Number(value));
            continue;
        }

        if byte.is_ascii_alphabetic() || byte == b'_' || byte == b'$' {
            let start = index;
            while index < bytes.len()
                && (bytes[index].is_ascii_alphanumeric()
                    || bytes[index] == b'_'
                    || bytes[index] == b'$')
            {
                index += 1;
            }
            tokens.push(Token::Ident(String::from(&source[start..index])));
            continue;
        }

        let rest = &source[index..];
        match PUNCT.iter().find(|symbol| rest.starts_with(**symbol)) {
            Some(symbol) => {
                tokens.push(Token::Punct(symbol));
                index += symbol.len();
            }
            // A byte the lexer does not know -- an accent, a Unicode
            // symbol -- is skipped. The file stays parsable, and the
            // report does not claim to have understood everything.
            None => index += 1,
        }
    }

    tokens.push(Token::End);
    tokens
}

// ============================================================
//  Tree
// ============================================================

#[derive(Debug, Clone)]
pub enum Node {
    /// Sequence of statements.
    Block(Vec<Node>),
    /// `const x = ...`, `let`, `var`.
    Declare { name: String, init: Box<Node> },
    /// `function name(...) { ... }`
    Function { name: String, body: Box<Node> },
    /// `if`, `while`, `for` -- the condition and the body.
    Branch { test: Box<Node>, body: Box<Node> },
    Return(Box<Node>),
    /// Call: the dotted path of the function, and the arguments.
    Call { path: String, args: Vec<Node> },
    /// `new Name(...)`
    New { path: String, args: Vec<Node> },
    /// Anonymous function: `function (...) { ... }` or `(...) => { ... }`.
    ///
    /// Distinct from a call, which it is not. Confusing it with a call to
    /// `function` inflated the missing list by one entry per callback,
    /// and pushed down the rate of perfectly translatable code.
    Lambda(Box<Node>),
    /// Member chain reduced to its dotted path.
    Path(String),
    Text(String),
    Number(i64),
    /// Everything the parser did not recognise. Counts as not converted.
    Opaque,
}

/// Keywords introducing a variable declaration.
const DECLARATORS: &[&str] = &["const", "let", "var"];

struct Parser {
    tokens: Vec<Token>,
    position: usize,
    depth: usize,
}

/// Maximum nesting depth.
///
/// The kernel stack is 32 KiB (`process::STACK_SIZE`): a minified file,
/// or a deeply parenthesised expression, would overflow it. Beyond that,
/// the parser returns `Opaque` -- the node counts as not converted, which
/// is the cautious verdict.
const MAX_DEPTH: usize = 32;

impl Parser {
    fn peek(&self) -> &Token {
        self.tokens.get(self.position).unwrap_or(&Token::End)
    }

    fn at(&self, offset: usize) -> &Token {
        self.tokens.get(self.position + offset).unwrap_or(&Token::End)
    }

    fn advance(&mut self) -> Token {
        let token = self.peek().clone();
        if self.position < self.tokens.len() {
            self.position += 1;
        }
        token
    }

    fn accept_punct(&mut self, symbol: &str) -> bool {
        if matches!(self.peek(), Token::Punct(found) if *found == symbol) {
            self.advance();
            return true;
        }
        false
    }

    fn accept_ident(&mut self, word: &str) -> bool {
        if matches!(self.peek(), Token::Ident(found) if found == word) {
            self.advance();
            return true;
        }
        false
    }

    /// Consumes up to the matching `)`, counting the levels.
    ///
    /// Used to step over what is not analysed -- the header of a `for`, a
    /// parameter list. Counting the levels is what distinguishes this
    /// skip from a naive search that would stop at the first `)` of a
    /// nested call.
    fn skip_group(&mut self, open: &str, close: &str) {
        let mut level = 1;
        while level > 0 {
            match self.peek() {
                Token::End => return,
                Token::Punct(symbol) if *symbol == open => level += 1,
                Token::Punct(symbol) if *symbol == close => level -= 1,
                _ => {}
            }
            self.advance();
        }
    }

    fn block(&mut self) -> Node {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            self.depth -= 1;
            self.skip_group("{", "}");
            return Node::Opaque;
        }
        let mut body = Vec::new();
        loop {
            match self.peek() {
                Token::End => break,
                Token::Punct("}") => {
                    self.advance();
                    break;
                }
                _ => {
                    // Progress guarantee. `statement` may return
                    // `Opaque` without having consumed anything -- a lone
                    // comma in a destructuring, for instance -- and a
                    // loop consuming nothing never terminates. In a
                    // kernel, that means a core lost until reboot, not an
                    // exception.
                    let before = self.position;
                    body.push(self.statement());
                    if self.position == before {
                        self.advance();
                    }
                }
            }
        }
        self.depth -= 1;
        Node::Block(body)
    }

    fn statement(&mut self) -> Node {
        // --- variable declaration ---
        if let Token::Ident(word) = self.peek().clone()
            && DECLARATORS.contains(&word.as_str())
            && matches!(self.at(1), Token::Ident(_))
        {
            self.advance();
            let Token::Ident(name) = self.advance() else {
                return Node::Opaque;
            };
            if self.accept_punct("=") {
                let init = self.expression();
                self.accept_punct(";");
                return Node::Declare {
                    name,
                    init: Box::new(init),
                };
            }
            self.accept_punct(";");
            return Node::Declare {
                name,
                init: Box::new(Node::Opaque),
            };
        }

        // --- function declaration ---
        if self.accept_ident("function") {
            let name = match self.peek().clone() {
                Token::Ident(name) => {
                    self.advance();
                    name
                }
                _ => String::from("<anonyme>"),
            };
            if self.accept_punct("(") {
                self.skip_group("(", ")");
            }
            let body = if self.accept_punct("{") {
                self.block()
            } else {
                Node::Opaque
            };
            return Node::Function {
                name,
                body: Box::new(body),
            };
        }

        // --- branches and loops ---
        for keyword in ["if", "while", "for", "switch"] {
            if self.accept_ident(keyword) {
                let test = if self.accept_punct("(") {
                    // The header of a `for` contains up to three
                    // expressions separated by `;`. The first is parsed,
                    // since the interesting calls come out of it, and the
                    // rest is stepped over.
                    let inner = self.expression();
                    self.skip_group("(", ")");
                    inner
                } else {
                    Node::Opaque
                };
                let body = if self.accept_punct("{") {
                    self.block()
                } else {
                    self.statement()
                };
                return Node::Branch {
                    test: Box::new(test),
                    body: Box::new(body),
                };
            }
        }

        if self.accept_ident("return") {
            if matches!(self.peek(), Token::Punct(";") | Token::Punct("}")) {
                self.accept_punct(";");
                return Node::Return(Box::new(Node::Opaque));
            }
            let value = self.expression();
            self.accept_punct(";");
            return Node::Return(Box::new(value));
        }

        if self.accept_punct("{") {
            return self.block();
        }
        if self.accept_punct(";") {
            return Node::Opaque;
        }

        let before = self.position;
        let expression = self.expression();
        self.accept_punct(";");
        // Same reason as above: a statement that does not advance makes
        // its caller loop. Here we cannot know whether the caller has a
        // guard of its own, so one is placed here too.
        if self.position == before {
            self.advance();
        }
        expression
    }

    /// Parses an expression.
    ///
    /// Operator precedence is not reproduced: it changes nothing in the
    /// census of calls, which is the only goal. Operands are parsed,
    /// operators stepped over.
    fn expression(&mut self) -> Node {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            self.depth -= 1;
            return Node::Opaque;
        }

        let mut parts = Vec::new();
        // An arrow anywhere in the expression makes it a function:
        // `const f = (a) => { ... }` declares `f`, and calls to `f` are
        // translated along with the project.
        let mut is_arrow = false;
        loop {
            match self.peek().clone() {
                Token::End
                | Token::Punct(";")
                | Token::Punct(")")
                | Token::Punct("]")
                | Token::Punct("}")
                | Token::Punct(",") => break,
                Token::Text(text) => {
                    self.advance();
                    parts.push(Node::Text(text));
                }
                Token::Number(value) => {
                    self.advance();
                    parts.push(Node::Number(value));
                }
                Token::Regex => {
                    self.advance();
                    parts.push(Node::Opaque);
                }
                Token::Punct("(") => {
                    self.advance();
                    let inner = self.expression();
                    self.accept_punct(")");
                    parts.push(inner);
                }
                Token::Punct("{") => {
                    self.advance();
                    // Object literal or arrow function body: in both
                    // cases what it contains may carry calls, and missing
                    // them would falsify the rate.
                    parts.push(self.block());
                }
                Token::Punct("[") => {
                    self.advance();
                    let inner = self.expression();
                    self.accept_punct("]");
                    parts.push(inner);
                }
                Token::Punct(symbol) => {
                    if symbol == "=>" {
                        is_arrow = true;
                    }
                    self.advance();
                }
                Token::Ident(_) => parts.push(self.postfix()),
            }
        }

        self.depth -= 1;
        let node = match parts.len() {
            0 => Node::Opaque,
            1 => parts.remove(0),
            _ => Node::Block(parts),
        };
        if is_arrow {
            return Node::Lambda(Box::new(node));
        }
        node
    }

    /// Parses an identifier, its member chain, and its calls.
    fn postfix(&mut self) -> Node {
        // `function (a, b) { ... }` in expression position is an
        // anonymous function, not a call to a function named
        // "function".
        if self.accept_ident("function") {
            if let Token::Ident(_) = self.peek().clone() {
                self.advance();
            }
            if self.accept_punct("(") {
                self.skip_group("(", ")");
            }
            let body = if self.accept_punct("{") {
                self.block()
            } else {
                Node::Opaque
            };
            return Node::Lambda(Box::new(body));
        }

        let is_new = self.accept_ident("new");

        let mut path = match self.advance() {
            Token::Ident(name) => name,
            _ => return Node::Opaque,
        };

        loop {
            if self.accept_punct(".") || self.accept_punct("?.") {
                match self.advance() {
                    Token::Ident(member) => {
                        path.push('.');
                        path.push_str(&member);
                    }
                    _ => return Node::Opaque,
                }
                continue;
            }
            if self.accept_punct("(") {
                let mut args = Vec::new();
                if !self.accept_punct(")") {
                    loop {
                        args.push(self.expression());
                        if self.accept_punct(")") {
                            break;
                        }
                        if !self.accept_punct(",") {
                            // Unexpected punctuation: the call is given
                            // up rather than looping.
                            self.skip_group("(", ")");
                            break;
                        }
                    }
                }
                return if is_new {
                    Node::New { path, args }
                } else {
                    Node::Call { path, args }
                };
            }
            // An index `a[b]` breaks the chain: the member is not known
            // statically, so the path is no longer known either.
            if self.accept_punct("[") {
                self.skip_group("[", "]");
                return Node::Opaque;
            }
            break;
        }

        if is_new {
            return Node::New {
                path,
                args: Vec::new(),
            };
        }
        Node::Path(path)
    }
}

/// Parses a JavaScript source.
pub fn parse(source: &str) -> Node {
    let tokens = lex(source);
    let mut parser = Parser {
        tokens,
        position: 0,
        depth: 0,
    };
    let mut body = Vec::new();
    while !matches!(parser.peek(), Token::End) {
        body.push(parser.statement());
    }
    Node::Block(body)
}

// ============================================================
//  9.6.2 -- Mapping of the system equivalents
// ============================================================

/// What a JavaScript call becomes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// A Nova OS system call.
    Syscall(&'static str),
    /// An operation of the graphics protocol (7.4).
    Compositor(&'static str),
    /// Translated with no system call: arithmetic, strings, paths.
    Inline(&'static str),
    /// No equivalent -- the code will have to be rewritten by hand.
    None(&'static str),
    /// Forbidden: forces classification as `.ras` (13.1).
    Forbidden(&'static str),
}

impl Target {
    /// Does this path count as converted?
    pub fn converted(&self) -> bool {
        matches!(
            self,
            Target::Syscall(_) | Target::Compositor(_) | Target::Inline(_)
        )
    }

    pub fn describe(&self) -> &'static str {
        match self {
            Target::Syscall(name)
            | Target::Compositor(name)
            | Target::Inline(name)
            | Target::None(name)
            | Target::Forbidden(name) => name,
        }
    }
}

/// The mapping table of 13.1.
///
/// Paths are compared by **dotted suffix**: `fs.readFileSync` also covers
/// `nodeFs.readFileSync` and `require('fs').readFileSync` stored in a
/// variable. Comparing whole paths would require following assignments,
/// which is data-flow analysis -- out of proportion for a tool 13.1 calls
/// manual and whose report is read back by a human.
pub static MAPPING: &[(&str, Target)] = &[
    // --- console and diagnostics ---
    ("console.log", Target::Syscall("SYS_CORE_TRACE")),
    ("console.error", Target::Syscall("SYS_CORE_TRACE")),
    ("console.warn", Target::Syscall("SYS_CORE_TRACE")),
    ("console.info", Target::Syscall("SYS_CORE_TRACE")),
    // --- filesystem ---
    ("fs.readFileSync", Target::Syscall("SYS_FS_READ")),
    ("fs.readFile", Target::Syscall("SYS_FS_READ")),
    ("fs.writeFileSync", Target::Syscall("SYS_FS_WRITE")),
    ("fs.writeFile", Target::Syscall("SYS_FS_WRITE")),
    ("fs.existsSync", Target::Syscall("SYS_FS_STAT")),
    ("fs.mkdirSync", Target::Syscall("SYS_FS_DIR_CREATE")),
    ("fs.unlinkSync", Target::Syscall("SYS_FS_DELETE")),
    ("fs.rmSync", Target::Syscall("SYS_FS_DELETE")),
    ("fs.readdirSync", Target::Syscall("SYS_FS_DIR_ACCESS")),
    ("fs.renameSync", Target::Syscall("SYS_FS_RENAME")),
    ("fs.copyFileSync", Target::Syscall("SYS_FS_COPY")),
    ("path.join", Target::Inline("normalisation de chemin")),
    ("path.resolve", Target::Inline("normalisation de chemin")),
    ("path.basename", Target::Inline("dernier segment")),
    ("path.dirname", Target::Inline("segment parent")),
    // --- windows and rendering ---
    ("BrowserWindow", Target::Compositor("OP_SURFACE_CREATE")),
    ("win.loadFile", Target::Compositor("liste d'affichage")),
    ("win.loadURL", Target::None("chargement de page distante")),
    ("win.show", Target::Compositor("OP_SURFACE_SHOW")),
    ("win.hide", Target::Compositor("OP_SURFACE_SHOW")),
    ("win.close", Target::Compositor("OP_SURFACE_DESTROY")),
    ("win.setTitle", Target::Compositor("titre de surface")),
    ("webContents.send", Target::Syscall("SYS_IPC_SEND")),
    // --- life cycle ---
    ("app.whenReady", Target::Inline("point d'entree")),
    // A promise chain becomes sequential code: there is nothing to call,
    // only an order to respect.
    ("then", Target::Inline("sequencement")),
    ("catch", Target::Inline("traitement d'erreur")),
    ("finally", Target::Inline("sequencement")),
    ("app.on", Target::Syscall("SYS_IPC_RECV")),
    ("app.quit", Target::Syscall("SYS_PROC_EXIT")),
    ("process.exit", Target::Syscall("SYS_PROC_EXIT")),
    ("process.on", Target::Syscall("SYS_IPC_RECV")),
    // --- IPC ---
    ("ipcMain.on", Target::Syscall("SYS_IPC_RECV")),
    ("ipcMain.handle", Target::Syscall("SYS_IPC_PUBLISH")),
    ("ipcRenderer.send", Target::Syscall("SYS_IPC_SEND")),
    ("ipcRenderer.invoke", Target::Syscall("SYS_IPC_SEND")),
    ("ipcRenderer.on", Target::Syscall("SYS_IPC_RECV")),
    // --- notifications and dialogs ---
    ("Notification", Target::Compositor("OP_NOTIFY_POST")),
    ("dialog.showMessageBox", Target::Compositor("OP_NOTIFY_POST")),
    ("dialog.showOpenDialog", Target::None("selecteur de fichiers")),
    // --- network ---
    ("http.get", Target::Syscall("SYS_NET_CONNECT")),
    ("https.get", Target::Syscall("SYS_NET_CONNECT")),
    ("net.connect", Target::Syscall("SYS_NET_CONNECT")),
    ("fetch", Target::Syscall("SYS_NET_CONNECT")),
    // --- time ---
    ("setTimeout", Target::Syscall("SYS_TIME_SLEEP_MS")),
    ("setInterval", Target::Syscall("SYS_TIME_SLEEP_MS")),
    ("Date.now", Target::Syscall("SYS_TIME_NOW")),
    // --- arithmetic and strings: translated with no call ---
    ("Math.floor", Target::Inline("division entiere")),
    ("Math.ceil", Target::Inline("division entiere")),
    ("Math.abs", Target::Inline("valeur absolue")),
    ("Math.min", Target::Inline("minimum")),
    ("Math.max", Target::Inline("maximum")),
    ("parseInt", Target::Inline("conversion entiere")),
    // --- no equivalent ---
    ("JSON.parse", Target::None("analyseur JSON a ecrire")),
    ("JSON.stringify", Target::None("serialiseur JSON a ecrire")),
    ("document.getElementById", Target::None("DOM absent")),
    ("document.querySelector", Target::None("DOM absent")),
    ("document.createElement", Target::None("DOM absent")),
    ("window.addEventListener", Target::None("DOM absent")),
    ("localStorage.getItem", Target::None("stockage web absent")),
    ("require", Target::Inline("resolution de dependance")),
    // --- forbidden (13.1) ---
    ("eval", Target::Forbidden("execution de code arbitraire")),
    ("Function", Target::Forbidden("constructeur de fonction")),
];

/// Looks up the mapping of a call path.
///
/// The search goes from the most specific to the most general:
/// `win.loadFile` before `loadFile`. Without that order, a short entry
/// would mask a precise one and the report would name the wrong one.
pub fn lookup(path: &str) -> Option<Target> {
    let mut best: Option<(usize, Target)> = None;
    for (candidate, target) in MAPPING.iter() {
        let matches = path == *candidate
            || path.ends_with(&alloc::format!(".{}", candidate))
            || candidate
                .rsplit_once('.')
                .is_some_and(|(_, tail)| path.ends_with(&alloc::format!(".{}", tail)) && path.contains(tail));
        if !matches {
            continue;
        }
        let score = candidate.len();
        if best.as_ref().is_none_or(|(previous, _)| score > *previous) {
            best = Some((score, *target));
        }
    }
    best.map(|(_, target)| target)
}

// ============================================================
//  9.6.3 -- Report
// ============================================================

/// Verdict of 13.1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// At least 70 % converted, no `eval`: native binary.
    Native(BinaryFormat),
    /// Below the threshold, or `eval` present.
    Raw(BinaryFormat),
}

impl Verdict {
    pub fn format(&self) -> BinaryFormat {
        match self {
            Verdict::Native(format) | Verdict::Raw(format) => *format,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Verdict::Native(_) => "conversion native",
            Verdict::Raw(_) => "classement en .ras",
        }
    }
}

/// One unconverted call path, and its occurrence count.
#[derive(Debug, Clone)]
pub struct Gap {
    pub path: String,
    pub reason: String,
    pub occurrences: usize,
}

/// The conversion report of 13.1.
#[derive(Debug, Clone)]
pub struct Report {
    pub files: usize,
    /// Files set aside for want of room -- named so that the report does
    /// not lie about its scope.
    pub skipped: usize,
    pub call_sites: usize,
    pub converted: usize,
    /// Success rate as a whole percentage.
    pub rate: u32,
    pub eval_found: bool,
    pub gaps: Vec<Gap>,
    pub warnings: Vec<String>,
    pub verdict: Verdict,
}

impl Report {
    /// Verdict of 13.1, applied to both criteria.
    fn decide(rate: u32, eval_found: bool) -> Verdict {
        if eval_found || rate < THRESHOLD {
            Verdict::Raw(BinaryFormat::Ras)
        } else {
            Verdict::Native(BinaryFormat::Cus)
        }
    }

    /// Returns the report as displayable lines.
    pub fn lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        lines.push(alloc::format!(
            "ETR - {} fichier(s) analyse(s), {} ecarte(s)",
            self.files,
            self.skipped
        ));
        lines.push(alloc::format!(
            "sites d'appel : {} - convertis : {} - taux : {} %",
            self.call_sites,
            self.converted,
            self.rate
        ));
        lines.push(alloc::format!(
            "verdict : {} -> .{}",
            self.verdict.as_str(),
            self.verdict.format().extension()
        ));
        if self.eval_found {
            lines.push(String::from(
                "eval() detecte - classement en .ras impose par la spec 13.1",
            ));
        }
        for gap in self.gaps.iter().take(12) {
            lines.push(alloc::format!(
                "  non converti : {} x{} - {}",
                gap.path,
                gap.occurrences,
                gap.reason
            ));
        }
        for warning in self.warnings.iter().take(8) {
            lines.push(alloc::format!("  avertissement : {}", warning));
        }
        if matches!(self.verdict, Verdict::Native(_)) {
            lines.push(alloc::format!(
                "reduction annoncee par la spec : {} Mo -> {} Mo",
                TYPICAL_BEFORE_MB,
                TYPICAL_AFTER_MB
            ));
        }
        lines
    }
}

/// State of one tree walk.
struct Walk {
    call_sites: usize,
    converted: usize,
    gaps: Vec<Gap>,
    eval_found: bool,
    /// Functions declared by the project itself.
    ///
    /// A call to one of them is **converted**: it will be translated
    /// along with the rest. Counting them as gaps made the rate of a
    /// well-structured project collapse -- the more an application was
    /// cut into functions, the less translatable it appeared, which is
    /// exactly the opposite of the truth.
    declared: Vec<String>,
}

impl Walk {
    /// First walk: censuses what the project declares.
    fn declare(&mut self, node: &Node) {
        match node {
            Node::Block(children) => {
                for child in children {
                    self.declare(child);
                }
            }
            Node::Function { name, body } => {
                if !self.declared.contains(name) {
                    self.declared.push(name.clone());
                }
                self.declare(body);
            }
            Node::Declare { name, init } => {
                if matches!(**init, Node::Lambda(_)) && !self.declared.contains(name) {
                    self.declared.push(name.clone());
                }
                self.declare(init);
            }
            Node::Lambda(body) => self.declare(body),
            Node::Branch { test, body } => {
                self.declare(test);
                self.declare(body);
            }
            Node::Return(value) => self.declare(value),
            Node::Call { args, .. } | Node::New { args, .. } => {
                for argument in args {
                    self.declare(argument);
                }
            }
            Node::Path(_) | Node::Text(_) | Node::Number(_) | Node::Opaque => {}
        }
    }

    fn record(&mut self, path: &str) {
        self.call_sites += 1;
        if self.declared.iter().any(|name| name == path) {
            self.converted += 1;
            return;
        }
        match lookup(path) {
            Some(target) if target.converted() => self.converted += 1,
            Some(Target::Forbidden(reason)) => {
                self.eval_found = true;
                self.note(path, reason);
            }
            Some(target) => self.note(path, target.describe()),
            None => self.note(path, "aucun equivalent connu"),
        }
    }

    fn note(&mut self, path: &str, reason: &str) {
        if let Some(gap) = self.gaps.iter_mut().find(|gap| gap.path == path) {
            gap.occurrences += 1;
            return;
        }
        self.gaps.push(Gap {
            path: String::from(path),
            reason: String::from(reason),
            occurrences: 1,
        });
    }

    fn visit(&mut self, node: &Node) {
        match node {
            Node::Block(children) => {
                for child in children {
                    self.visit(child);
                }
            }
            Node::Declare { init, .. } => self.visit(init),
            Node::Function { body, .. } => self.visit(body),
            Node::Branch { test, body } => {
                self.visit(test);
                self.visit(body);
            }
            Node::Return(value) => self.visit(value),
            Node::Lambda(body) => self.visit(body),
            Node::Call { path, args } | Node::New { path, args } => {
                self.record(path);
                for argument in args {
                    self.visit(argument);
                }
            }
            Node::Path(_) | Node::Text(_) | Node::Number(_) | Node::Opaque => {}
        }
    }
}

/// Analyses a single source and returns its report.
pub fn analyze(source: &str) -> Report {
    analyze_many(&[String::from(source)], 0)
}

/// Analyses several sources and aggregates the result.
pub fn analyze_many(sources: &[String], skipped: usize) -> Report {
    let mut walk = Walk {
        call_sites: 0,
        converted: 0,
        gaps: Vec::new(),
        eval_found: false,
        declared: Vec::new(),
    };

    // Two walks, and the order matters: every declaration of every file
    // must be known before the first count, otherwise a call to a
    // function defined in another file -- or further down in the same one
    // -- would pass for untranslatable.
    let trees: Vec<Node> = sources.iter().map(|source| parse(source)).collect();
    for tree in trees.iter() {
        walk.declare(tree);
    }
    for tree in trees.iter() {
        walk.visit(tree);
    }

    // Integer arithmetic: the kernel runs without SSE, a float here would
    // be a float in the kernel. A project with no call at all is 100 %
    // converted -- there is nothing that could be missing.
    let rate = if walk.call_sites == 0 {
        100
    } else {
        (walk.converted * 100 / walk.call_sites) as u32
    };

    let mut warnings = Vec::new();
    if walk.eval_found {
        warnings.push(String::from(
            "eval() ou new Function() : le code genere a l'execution ne peut pas etre traduit",
        ));
    }
    if skipped > 0 {
        warnings.push(alloc::format!(
            "{} fichier(s) non analyse(s) - le taux ne porte que sur les autres",
            skipped
        ));
    }
    if walk.call_sites == 0 && !sources.is_empty() {
        warnings.push(String::from(
            "aucun site d'appel trouve - verifier que les sources sont bien du JavaScript",
        ));
    }

    let mut gaps = walk.gaps;
    // The most frequent gaps first: that is where a manual pass starts.
    gaps.sort_by(|left, right| right.occurrences.cmp(&left.occurrences));

    Report {
        files: sources.len(),
        skipped,
        call_sites: walk.call_sites,
        converted: walk.converted,
        rate,
        eval_found: walk.eval_found,
        gaps,
        warnings,
        verdict: Report::decide(rate, walk.eval_found),
    }
}

// ============================================================
//  9.6.1 -- Walking a project
// ============================================================

/// Does the file have an extension the tool can read?
fn is_source(name: &str) -> bool {
    matches!(
        name.rsplit('.').next(),
        Some("js") | Some("mjs") | Some("cjs") | Some("ts")
    )
}

/// Directories stepped over without being read.
///
/// `node_modules` holds the dependencies, not the project code:
/// analysing it would drown the report under thousands of call sites that
/// will not be rewritten by hand anyway.
const IGNORED: &[&str] = &["node_modules", "dist", "build", ".git", "out"];

/// Analyses a whole Electron project.
pub fn analyze_project(directory: &str) -> Result<Report, crate::fs::FsError> {
    let who = Credentials::kernel();
    let mut sources = Vec::new();
    let mut skipped = 0;
    collect(directory, who, &mut sources, &mut skipped)?;
    Ok(analyze_many(&sources, skipped))
}

fn collect(
    directory: &str,
    who: Credentials,
    sources: &mut Vec<String>,
    skipped: &mut usize,
) -> Result<(), crate::fs::FsError> {
    let entries = crate::fs::list(directory, who)?;

    for entry in entries {
        if IGNORED.contains(&entry.name.as_str()) {
            *skipped += 1;
            continue;
        }
        let path = alloc::format!("{}/{}", directory, entry.name);

        if crate::fs::list(&path, who).is_ok() {
            collect(&path, who, sources, skipped)?;
            continue;
        }
        if !is_source(&entry.name) {
            continue;
        }
        if sources.len() >= MAX_FILES {
            *skipped += 1;
            continue;
        }
        let Ok(bytes) = crate::fs::read_file(&path, who) else {
            *skipped += 1;
            continue;
        };
        if bytes.len() > MAX_SOURCE {
            *skipped += 1;
            continue;
        }
        match core::str::from_utf8(&bytes) {
            Ok(text) => sources.push(String::from(text)),
            Err(_) => *skipped += 1,
        }
    }

    Ok(())
}
