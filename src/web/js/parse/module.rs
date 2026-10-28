//! `import` and `export` declarations at the top level of a module.
//!
//! Nothing here runs: the declarations are recorded in `ModuleInfo` and
//! taken out of the body, which keeps only what has a value -- the
//! declaration after `export`, the expression after `export default` (as
//! a constant named `default`, a name no source can write). The
//! interpreter links the recorded imports before the body runs and
//! makes each export a getter on the module's binding, so an importer
//! sees the binding live, as the language wants.

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::vec::Vec;

use super::super::ast::*;
use super::super::lex::Tok;
use super::super::value::JsStr;
use super::{pattern_names, PResult, Parser};

impl<'a> Parser<'a> {
    /// The string after `from`.
    fn module_specifier(&mut self) -> PResult<JsStr> {
        match self.advance() {
            Tok::Str(source) => Ok(source),
            _ => self.error("nom de module attendu (une chaine)"),
        }
    }

    /// `with { type: "json" }` (or the older `assert`) after a specifier:
    /// read and ignored, the loader decides by the file.
    fn import_attributes(&mut self) -> PResult<()> {
        if (self.is_word("with") || (self.is_word("assert") && !self.newline_before())) && self.punct_at(1, "{") {
            self.advance();
            self.skip_balanced()?;
        }
        Ok(())
    }

    /// A name in an import or export list: an identifier, a keyword
    /// (`default`), or a string.
    fn module_export_name(&mut self) -> PResult<JsStr> {
        match self.advance() {
            Tok::Ident(name) => Ok(self.intern(&name)),
            Tok::Str(name) => Ok(name),
            _ => self.error("nom exporte attendu"),
        }
    }

    /// TypeScript: `type X` in an import or export list names a type.
    fn type_only_entry(&self) -> bool {
        self.options.typescript
            && self.is_word("type")
            && matches!(self.peek_at(1), Tok::Ident(w) if w.as_str() != "as")
    }

    pub(super) fn import_declaration(&mut self) -> PResult<Vec<Stmt>> {
        self.advance(); // import
        // TypeScript: `import type ...` brings types only.
        if self.options.typescript
            && self.is_word("type")
            && !self.word_at(1, "from")
            && !self.punct_at(1, ",")
            && !self.punct_at(1, "=")
        {
            while !matches!(self.peek(), Tok::Str(_) | Tok::Eof) {
                self.advance();
            }
            self.module_specifier()?;
            self.import_attributes()?;
            self.semicolon()?;
            return Ok(Vec::new());
        }
        let mut bindings = Vec::new();
        // `import "./side-effect.js";`
        if let Tok::Str(source) = self.peek().clone() {
            self.advance();
            self.import_attributes()?;
            self.semicolon()?;
            self.module.imports.push(ImportDecl { source, bindings });
            return Ok(Vec::new());
        }
        // TypeScript: `import fs = require("fs");`
        if self.options.typescript && matches!(self.peek(), Tok::Ident(_)) && self.punct_at(1, "=") {
            let name = self.identifier()?;
            self.advance();
            let value = self.assignment()?;
            self.semicolon()?;
            return Ok(alloc::vec![Stmt::Var(VarKind::Const, alloc::vec![(Pattern::Ident(name), Some(value))])]);
        }
        if matches!(self.peek(), Tok::Ident(_)) {
            let local = self.identifier()?;
            bindings.push((ImportName::Default, local));
            if !self.eat(",") {
                return self.import_from(bindings);
            }
        }
        if self.eat("*") {
            if !self.eat_word("as") {
                return self.error("\"as\" attendu");
            }
            let local = self.identifier()?;
            bindings.push((ImportName::Namespace, local));
        } else if self.eat("{") {
            while !self.eat("}") {
                let type_only = self.type_only_entry();
                if type_only {
                    self.advance();
                }
                let imported = self.module_export_name()?;
                let local = if self.eat_word("as") { self.identifier()? } else { imported.clone() };
                if !type_only {
                    bindings.push((ImportName::Named(imported), local));
                }
                if !self.is("}") {
                    self.expect(",")?;
                }
            }
        } else {
            return self.error("liste d'import attendue");
        }
        self.import_from(bindings)
    }

    fn import_from(&mut self, bindings: Vec<(ImportName, JsStr)>) -> PResult<Vec<Stmt>> {
        if !self.eat_word("from") {
            return self.error("\"from\" attendu");
        }
        let source = self.module_specifier()?;
        self.import_attributes()?;
        self.semicolon()?;
        self.module.imports.push(ImportDecl { source, bindings });
        Ok(Vec::new())
    }

    pub(super) fn export_declaration(&mut self) -> PResult<Vec<Stmt>> {
        self.advance(); // export
        let default = JsStr::from("default");
        if self.eat_word("default") {
            let is_async = self.is_word("async") && self.word_at(1, "function") && !self.tokens[self.pos + 1].newline;
            if self.is_word("function") || is_async {
                if is_async {
                    self.advance();
                }
                let func = self.function(is_async, false)?;
                if self.take_signature() {
                    return Ok(Vec::new());
                }
                // An anonymous default function is named "default", and
                // so is its binding.
                let func = if func.name.is_none() {
                    let mut named = (*func).clone();
                    named.name = Some(default.clone());
                    Rc::new(named)
                } else {
                    func
                };
                let local = func.name.clone().unwrap_or_else(|| default.clone());
                self.declare_var(&local);
                self.module.exports.push(ExportEntry::Local(default, local));
                return Ok(alloc::vec![Stmt::Function(func)]);
            }
            if self.options.typescript && self.is_word("abstract") && self.word_at(1, "class") {
                self.advance();
            }
            if self.is_word("class") {
                let class = self.class(false)?;
                let local = class.name.clone().unwrap_or_else(|| default.clone());
                self.module.exports.push(ExportEntry::Local(default, local.clone()));
                return Ok(alloc::vec![Stmt::Class(local, class)]);
            }
            if self.options.typescript && self.is_word("interface") {
                return Ok(self.ts_declaration()?.into_iter().collect());
            }
            let value = self.assignment()?;
            self.semicolon()?;
            self.module.exports.push(ExportEntry::Local(default.clone(), default.clone()));
            return Ok(alloc::vec![Stmt::Var(VarKind::Const, alloc::vec![(Pattern::Ident(default), Some(value))])]);
        }
        if self.eat("*") {
            let alias = if self.eat_word("as") { Some(self.module_export_name()?) } else { None };
            if !self.eat_word("from") {
                return self.error("\"from\" attendu");
            }
            let source = self.module_specifier()?;
            self.import_attributes()?;
            self.semicolon()?;
            self.module.exports.push(match alias {
                Some(name) => ExportEntry::From(name, source, None),
                None => ExportEntry::All(source),
            });
            return Ok(Vec::new());
        }
        // TypeScript: `export type { A } [from "x"]`.
        if self.options.typescript && self.is_word("type") && self.punct_at(1, "{") {
            self.advance();
            self.skip_balanced()?;
            if self.eat_word("from") {
                self.module_specifier()?;
                self.import_attributes()?;
            }
            self.semicolon()?;
            return Ok(Vec::new());
        }
        // TypeScript: `export = value` is CommonJS: `module.exports = value`.
        if self.options.typescript && self.is("=") {
            self.advance();
            let value = self.assignment()?;
            self.semicolon()?;
            let target = Expr::Member(Box::new(Expr::Ident(self.intern("module"))), self.intern("exports"), false);
            return Ok(alloc::vec![Stmt::Expr(Expr::Assign(AssignOp::Assign, Box::new(Pattern::Target(Box::new(target))), Box::new(value)))]);
        }
        if self.eat("{") {
            let mut list = Vec::new();
            while !self.eat("}") {
                let type_only = self.type_only_entry();
                if type_only {
                    self.advance();
                }
                let local = self.module_export_name()?;
                let exported = if self.eat_word("as") { self.module_export_name()? } else { local.clone() };
                if !type_only {
                    list.push((local, exported));
                }
                if !self.is("}") {
                    self.expect(",")?;
                }
            }
            if self.eat_word("from") {
                let source = self.module_specifier()?;
                self.import_attributes()?;
                self.semicolon()?;
                for (imported, exported) in list {
                    self.module.exports.push(ExportEntry::From(exported, source.clone(), Some(imported)));
                }
            } else {
                self.semicolon()?;
                for (local, exported) in list {
                    self.module.exports.push(ExportEntry::Local(exported, local));
                }
            }
            return Ok(Vec::new());
        }
        // `export` before a declaration: the names it binds.
        let statement = self.statement()?;
        let mut names = Vec::new();
        declared_names(&statement, &mut names);
        let declares = matches!(statement, Stmt::Var(..) | Stmt::Function(_) | Stmt::Class(..) | Stmt::Block(_) | Stmt::Empty);
        if !declares {
            return self.error("declaration attendue apres export");
        }
        for name in names {
            self.module.exports.push(ExportEntry::Local(name.clone(), name));
        }
        Ok(alloc::vec![statement])
    }
}

/// The names a declaration binds (an `enum` of TypeScript is a block of
/// a `var` and assignments).
fn declared_names(statement: &Stmt, out: &mut Vec<JsStr>) {
    match statement {
        Stmt::Var(_, declarations) => {
            for (pattern, _) in declarations {
                pattern_names(pattern, out);
            }
        }
        Stmt::Function(func) => out.extend(func.name.iter().cloned()),
        Stmt::Class(name, _) => out.push(name.clone()),
        Stmt::Block(body) => {
            for inner in body {
                if let Stmt::Var(..) = inner {
                    declared_names(inner, out);
                }
            }
        }
        _ => {}
    }
}
