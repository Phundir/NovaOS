//! 9.5.1 -- Type and flow checking.
//!
//! ## Why a checker at all, when the code generator could just emit
//!
//! Because every value in this subset is one 64-bit word, and the word
//! does not say what it is. `a / b` on two `i64` is `idiv`; on two `u64`
//! it is `div`; the two differ only in sign handling, and the wrong one
//! gives a plausible number rather than a fault. `a < b` is `jl` or `jb`
//! for the same reason. So the type has to be decided **before** the
//! instruction is chosen, and there is exactly one place that can do it.
//!
//! The checker also fills a second role: it is what makes an error
//! arrive with a line number and a name, rather than as a program that
//! runs and answers wrong.
//!
//! ## What it refuses that a full Rust compiler would accept
//!
//! Nothing, as far as it goes -- the subset is a subset, not a dialect.
//! A program this checker accepts, a real `rustc` accepts too and gives
//! the same answers, save for the two behaviours named in the module
//! header of `super`: arithmetic wraps instead of panicking on overflow,
//! and there is no borrow checker because there is nothing to borrow.
//!
//! ## Definite return
//!
//! A function that promises an `i64` must not be able to fall off its
//! end: what would be returned is whatever `rax` last held, which is a
//! value, plausible and wrong. `always_returns` is deliberately
//! conservative -- it may refuse a function that in fact always returns,
//! and it never accepts one that does not.

use alloc::string::String;
use alloc::vec::Vec;

use super::ast::*;
use super::{BUILTINS, Builtin};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckError {
    Undefined(usize, String),
    UnknownFunction(usize, String),
    /// Two types that had to be equal and were not.
    Mismatch(usize, &'static str, &'static str, String),
    NotMutable(usize, String),
    BadArity(usize, String, usize, usize),
    /// An operator applied to a type it has no meaning for.
    BadOperand(usize, &'static str, &'static str),
    /// A cast the subset does not perform.
    BadCast(usize, &'static str, &'static str),
    /// An integer literal too large for the type it lands in.
    LiteralOutOfRange(usize, u64, &'static str),
    /// `break` or `continue` outside a loop.
    OutsideLoop(usize, &'static str),
    /// A function that can reach its end without returning a value.
    MissingReturn(usize, String, &'static str),
    /// A value `print!` has no way to render.
    NotPrintable(usize, &'static str),
    /// `main` with a signature the runtime cannot call.
    BadMain(usize),
    /// A local shadowing a parameter or another local of the same block.
    ///
    /// Real Rust allows shadowing; this subset gives one stack slot per
    /// name per function, so a second `let x` would silently reuse the
    /// first one's slot with a possibly different type.
    Shadowed(usize, String),
}

impl CheckError {
    pub fn message(&self) -> String {
        match self {
            CheckError::Undefined(line, name) => {
                alloc::format!("ligne {} : \"{}\" n'est pas declare", line, name)
            }
            CheckError::UnknownFunction(line, name) => {
                alloc::format!("ligne {} : fonction \"{}\" inconnue", line, name)
            }
            CheckError::Mismatch(line, expected, found, context) => alloc::format!(
                "ligne {} : {} attendu, {} trouve ({})",
                line,
                expected,
                found,
                context
            ),
            CheckError::NotMutable(line, name) => alloc::format!(
                "ligne {} : \"{}\" n'est pas mutable - le declarer avec let mut",
                line,
                name
            ),
            CheckError::BadArity(line, name, expected, found) => alloc::format!(
                "ligne {} : \"{}\" attend {} argument(s), {} fourni(s)",
                line,
                name,
                expected,
                found
            ),
            CheckError::BadOperand(line, operator, ty) => alloc::format!(
                "ligne {} : l'operateur {} ne s'applique pas a {}",
                line,
                operator,
                ty
            ),
            CheckError::BadCast(line, from, to) => {
                alloc::format!("ligne {} : conversion impossible de {} vers {}", line, from, to)
            }
            CheckError::LiteralOutOfRange(line, value, ty) => {
                alloc::format!("ligne {} : le litteral {} ne tient pas dans un {}", line, value, ty)
            }
            CheckError::OutsideLoop(line, keyword) => {
                alloc::format!("ligne {} : {} hors d'une boucle", line, keyword)
            }
            CheckError::MissingReturn(line, name, ty) => alloc::format!(
                "ligne {} : \"{}\" promet un {} mais peut atteindre sa fin sans en rendre un",
                line,
                name,
                ty
            ),
            CheckError::NotPrintable(line, ty) => alloc::format!(
                "ligne {} : {} ne sait pas s'afficher - i64, u64, bool et &str le savent",
                line,
                ty
            ),
            CheckError::BadMain(line) => alloc::format!(
                "ligne {} : main ne prend pas de parametre et ne rend rien",
                line
            ),
            CheckError::Shadowed(line, name) => alloc::format!(
                "ligne {} : \"{}\" est deja declare dans cette fonction - ce sous-ensemble ne masque pas un nom",
                line,
                name
            ),
        }
    }
}

/// What is known about a name in scope.
#[derive(Debug, Clone)]
pub struct Local {
    pub name: String,
    pub ty: Type,
    pub mutable: bool,
    /// Stack slot, assigned here and read by `codegen`. Doing it in the
    /// checker rather than in the code generator means the layout is
    /// decided once, by the pass that already walks every declaration.
    pub slot: usize,
}

/// Signature of something callable.
#[derive(Debug, Clone)]
pub struct Signature {
    pub params: Vec<Type>,
    pub result: Type,
    pub builtin: Option<Builtin>,
}

/// What the checker hands to the two back-ends.
#[derive(Debug, Clone)]
pub struct Checked {
    /// Slots needed by each function, in the order of `Program::functions`.
    pub frames: Vec<usize>,
    /// Every name of every function, with its slot **and its type**,
    /// keyed by function index.
    ///
    /// The type travels with the slot because the code generator needs
    /// it: `a / b` is `idiv` or `div` depending on the sign, and the
    /// tree does not say which. Recovering it there by re-deriving would
    /// mean a second checker.
    pub slots: Vec<Vec<(String, usize, Type)>>,
    /// Constants, folded to a value.
    pub constants: Vec<(String, Type, ConstValue)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConstValue {
    Int(u64),
    Bool(bool),
    Str(String),
}

struct Checker<'a> {
    program: &'a Program,
    /// Scopes of the function being checked, innermost last.
    scopes: Vec<Vec<Local>>,
    /// Every local of the current function, so that a slot is never
    /// reused between two blocks. A function's frame is small; sharing
    /// slots between disjoint blocks would save a few bytes and make a
    /// dangling read possible.
    all_locals: Vec<(String, usize, Type)>,
    next_slot: usize,
    result: Type,
    loop_depth: usize,
    constants: Vec<(String, Type, ConstValue)>,
}

/// Checks a whole program.
pub fn check(program: &Program) -> Result<Checked, CheckError> {
    // `main` first: a program whose entry point has the wrong shape is
    // wrong whatever else it contains.
    let main = program
        .function("main")
        .expect("l'absence de main est refusee par le parseur");
    if !main.params.is_empty() || main.result != Type::Unit {
        return Err(CheckError::BadMain(main.line));
    }

    let mut constants = Vec::new();
    for item in program.constants.iter() {
        let value = fold_const(program, &constants, item)?;
        constants.push((item.name.clone(), item.ty, value));
    }

    let mut frames = Vec::new();
    let mut slots = Vec::new();

    for function in program.functions.iter() {
        let mut checker = Checker {
            program,
            scopes: alloc::vec![Vec::new()],
            all_locals: Vec::new(),
            next_slot: 0,
            result: function.result,
            loop_depth: 0,
            constants: constants.clone(),
        };

        for param in function.params.iter() {
            checker.declare(param.line_safe(), &param.name, param.ty, false)?;
        }

        let body_type = checker.block(&function.body, Some(function.result))?;

        // A tail expression is the return value; it must have the
        // declared type. Without a tail, the function must return
        // explicitly on every path.
        if function.body.tail.is_some() {
            if body_type != function.result {
                return Err(CheckError::Mismatch(
                    function.line,
                    function.result.as_str(),
                    body_type.as_str(),
                    alloc::format!("valeur de retour de \"{}\"", function.name),
                ));
            }
        } else if function.result != Type::Unit && !always_returns(&function.body) {
            return Err(CheckError::MissingReturn(
                function.line,
                function.name.clone(),
                function.result.as_str(),
            ));
        }

        frames.push(checker.next_slot);
        slots.push(checker.all_locals);
    }

    Ok(Checked {
        frames,
        slots,
        constants,
    })
}

impl Param {
    /// Parameters carry no line of their own; the function's will do.
    fn line_safe(&self) -> usize {
        0
    }
}

impl Checker<'_> {
    fn declare(
        &mut self,
        line: usize,
        name: &str,
        ty: Type,
        mutable: bool,
    ) -> Result<usize, CheckError> {
        if self.all_locals.iter().any(|(other, _, _)| other == name) {
            return Err(CheckError::Shadowed(line, String::from(name)));
        }
        let slot = self.next_slot;
        self.next_slot += 1;
        self.all_locals.push((String::from(name), slot, ty));
        self.scopes
            .last_mut()
            .expect("une portee existe toujours")
            .push(Local {
                name: String::from(name),
                ty,
                mutable,
                slot,
            });
        Ok(slot)
    }

    fn lookup(&self, name: &str) -> Option<&Local> {
        for scope in self.scopes.iter().rev() {
            if let Some(local) = scope.iter().find(|local| local.name == name) {
                return Some(local);
            }
        }
        None
    }

    fn constant(&self, name: &str) -> Option<Type> {
        self.constants
            .iter()
            .find(|(other, _, _)| other == name)
            .map(|(_, ty, _)| *ty)
    }

    fn signature(&self, name: &str) -> Option<Signature> {
        signature_of(self.program, name)
    }

    /// Checks a block and returns the type of its value.
    fn block(&mut self, block: &Block, expected: Option<Type>) -> Result<Type, CheckError> {
        self.scopes.push(Vec::new());
        let mut result = Type::Unit;

        for statement in block.statements.iter() {
            self.statement(statement)?;
        }
        if let Some(tail) = block.tail.as_ref() {
            result = self.expr(tail, expected)?;
        }

        self.scopes.pop();
        Ok(result)
    }

    fn statement(&mut self, statement: &Stmt) -> Result<(), CheckError> {
        match statement {
            Stmt::Let {
                name,
                mutable,
                declared,
                value,
                line,
            } => {
                let ty = self.expr(value, *declared)?;
                if let Some(declared) = declared
                    && *declared != ty
                {
                    return Err(CheckError::Mismatch(
                        *line,
                        declared.as_str(),
                        ty.as_str(),
                        alloc::format!("initialisation de \"{}\"", name),
                    ));
                }
                if ty == Type::Unit {
                    return Err(CheckError::Mismatch(
                        *line,
                        "une valeur",
                        "()",
                        alloc::format!("initialisation de \"{}\"", name),
                    ));
                }
                self.declare(*line, name, ty, *mutable)?;
                Ok(())
            }

            Stmt::Assign {
                name,
                op,
                value,
                line,
            } => {
                let Some(local) = self.lookup(name) else {
                    // A constant assigned to gets the message about
                    // constants, not "undeclared".
                    if self.constant(name).is_some() {
                        return Err(CheckError::NotMutable(*line, name.clone()));
                    }
                    return Err(CheckError::Undefined(*line, name.clone()));
                };
                let (target, mutable) = (local.ty, local.mutable);
                if !mutable {
                    return Err(CheckError::NotMutable(*line, name.clone()));
                }
                let found = self.expr(value, Some(target))?;

                match op {
                    None => {
                        if found != target {
                            return Err(CheckError::Mismatch(
                                *line,
                                target.as_str(),
                                found.as_str(),
                                alloc::format!("affectation a \"{}\"", name),
                            ));
                        }
                    }
                    Some(operator) => {
                        // The compound form is checked as the operator it
                        // carries, so `s += 1` complains about `+` on a
                        // &str rather than about an assignment.
                        binary_result(*line, *operator, target, found)?;
                    }
                }
                Ok(())
            }

            Stmt::If(condition, then_block, else_block, line) => {
                self.condition(condition, *line)?;
                self.block(then_block, None)?;
                if let Some(block) = else_block {
                    self.block(block, None)?;
                }
                Ok(())
            }

            Stmt::While(condition, body, line) => {
                self.condition(condition, *line)?;
                self.loop_depth += 1;
                self.block(body, None)?;
                self.loop_depth -= 1;
                Ok(())
            }

            Stmt::Loop(body, _) => {
                self.loop_depth += 1;
                self.block(body, None)?;
                self.loop_depth -= 1;
                Ok(())
            }

            Stmt::Break(line) => {
                if self.loop_depth == 0 {
                    return Err(CheckError::OutsideLoop(*line, "break"));
                }
                Ok(())
            }

            Stmt::Continue(line) => {
                if self.loop_depth == 0 {
                    return Err(CheckError::OutsideLoop(*line, "continue"));
                }
                Ok(())
            }

            Stmt::Return(value, line) => {
                let found = match value {
                    Some(expr) => self.expr(expr, Some(self.result))?,
                    None => Type::Unit,
                };
                if found != self.result {
                    return Err(CheckError::Mismatch(
                        *line,
                        self.result.as_str(),
                        found.as_str(),
                        String::from("return"),
                    ));
                }
                Ok(())
            }

            Stmt::Expr(expr, _) => {
                self.expr(expr, None)?;
                Ok(())
            }

            Stmt::Print { args, line, .. } => {
                for argument in args.iter() {
                    let ty = self.expr(argument, None)?;
                    if ty == Type::Unit {
                        return Err(CheckError::NotPrintable(*line, ty.as_str()));
                    }
                }
                Ok(())
            }
        }
    }

    fn condition(&mut self, expr: &Expr, line: usize) -> Result<(), CheckError> {
        let ty = self.expr(expr, Some(Type::Bool))?;
        if ty != Type::Bool {
            return Err(CheckError::Mismatch(
                line,
                "bool",
                ty.as_str(),
                String::from("condition"),
            ));
        }
        Ok(())
    }

    /// Type of an expression, given what the context would like.
    ///
    /// `expected` only ever decides the type of an **untyped integer
    /// literal**. It is not an inference engine: propagating it further
    /// would make `let x: u64 = a + b;` retype `a` and `b`, which is
    /// exactly the kind of quiet retyping a subset must not do.
    fn expr(&mut self, expr: &Expr, expected: Option<Type>) -> Result<Type, CheckError> {
        match expr {
            Expr::Int(value, suffix) => {
                let ty = match suffix {
                    Some(ty) => *ty,
                    None => match expected {
                        Some(hint) if hint.is_integer() => hint,
                        _ => Type::I64,
                    },
                };
                if ty == Type::I64 && *value > i64::MAX as u64 + 1 {
                    return Err(CheckError::LiteralOutOfRange(0, *value, ty.as_str()));
                }
                Ok(ty)
            }
            Expr::Bool(_) => Ok(Type::Bool),
            Expr::Str(_) => Ok(Type::Str),
            Expr::Unit => Ok(Type::Unit),

            Expr::Path(name, line) => {
                if let Some(local) = self.lookup(name) {
                    return Ok(local.ty);
                }
                if let Some(ty) = self.constant(name) {
                    return Ok(ty);
                }
                Err(CheckError::Undefined(*line, name.clone()))
            }

            Expr::Unary(operator, inner, line) => {
                let ty = self.expr(inner, expected)?;
                match operator {
                    UnOp::Neg => {
                        if ty != Type::I64 {
                            return Err(CheckError::BadOperand(*line, "-", ty.as_str()));
                        }
                        Ok(Type::I64)
                    }
                    UnOp::Not => {
                        if ty == Type::Bool || ty.is_integer() {
                            Ok(ty)
                        } else {
                            Err(CheckError::BadOperand(*line, "!", ty.as_str()))
                        }
                    }
                }
            }

            Expr::Binary(operator, left, right, line) => {
                // The hint crosses to both sides for an arithmetic
                // operator, and to neither for a comparison -- where what
                // the context wants is a bool, which says nothing about
                // the operands. The left side then types the right, so
                // `n == 0` picks up `n`'s type for the literal.
                let hint = if operator.is_comparison() {
                    None
                } else {
                    expected
                };
                let left_type = self.expr(left, hint)?;
                let right_hint = match operator {
                    BinOp::Shl | BinOp::Shr => None,
                    _ => Some(left_type),
                };
                let right_type = self.expr(right, right_hint)?;
                binary_result(*line, *operator, left_type, right_type)
            }

            Expr::Cast(inner, target, line) => {
                let source = self.expr(inner, None)?;
                let allowed = match (source, target) {
                    (Type::I64, Type::U64) | (Type::U64, Type::I64) => true,
                    (Type::I64, Type::I64) | (Type::U64, Type::U64) => true,
                    // `bool as i64` is the one widening Rust allows.
                    (Type::Bool, Type::I64) | (Type::Bool, Type::U64) => true,
                    _ => false,
                };
                if !allowed {
                    return Err(CheckError::BadCast(*line, source.as_str(), target.as_str()));
                }
                Ok(*target)
            }

            Expr::Call(name, args, line) => {
                let Some(signature) = self.signature(name) else {
                    return Err(CheckError::UnknownFunction(*line, name.clone()));
                };
                if signature.params.len() != args.len() {
                    return Err(CheckError::BadArity(
                        *line,
                        name.clone(),
                        signature.params.len(),
                        args.len(),
                    ));
                }
                for (argument, declared) in args.iter().zip(signature.params.iter()) {
                    let found = self.expr(argument, Some(*declared))?;
                    if found != *declared {
                        return Err(CheckError::Mismatch(
                            *line,
                            declared.as_str(),
                            found.as_str(),
                            alloc::format!("argument de \"{}\"", name),
                        ));
                    }
                }
                Ok(signature.result)
            }

            Expr::If(condition, then_block, else_block, line) => {
                self.condition(condition, *line)?;
                let then_type = self.block(then_block, expected)?;
                match else_block {
                    Some(block) => {
                        let else_type = self.block(block, expected)?;
                        if then_type != else_type {
                            return Err(CheckError::Mismatch(
                                *line,
                                then_type.as_str(),
                                else_type.as_str(),
                                String::from("les deux branches d'un if"),
                            ));
                        }
                        Ok(then_type)
                    }
                    // An `if` with no `else` has no value: there is a
                    // path through it that produces nothing.
                    None => {
                        if then_type != Type::Unit {
                            return Err(CheckError::Mismatch(
                                *line,
                                "()",
                                then_type.as_str(),
                                String::from("un if sans else ne rend pas de valeur"),
                            ));
                        }
                        Ok(Type::Unit)
                    }
                }
            }

            Expr::Block(block) => self.block(block, expected),
        }
    }
}

/// The type of `left op right`, or the error that says why there is none.
pub fn binary_result(
    line: usize,
    operator: BinOp,
    left: Type,
    right: Type,
) -> Result<Type, CheckError> {
    match operator {
        BinOp::And | BinOp::Or => {
            if left != Type::Bool || right != Type::Bool {
                let offender = if left != Type::Bool { left } else { right };
                return Err(CheckError::BadOperand(
                    line,
                    operator.as_str(),
                    offender.as_str(),
                ));
            }
            Ok(Type::Bool)
        }

        BinOp::Eq | BinOp::Ne => {
            if left != right {
                return Err(CheckError::Mismatch(
                    line,
                    left.as_str(),
                    right.as_str(),
                    alloc::format!("comparaison {}", operator.as_str()),
                ));
            }
            if left == Type::Unit {
                return Err(CheckError::BadOperand(line, operator.as_str(), "()"));
            }
            Ok(Type::Bool)
        }

        BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
            if left != right {
                return Err(CheckError::Mismatch(
                    line,
                    left.as_str(),
                    right.as_str(),
                    alloc::format!("comparaison {}", operator.as_str()),
                ));
            }
            if !left.is_integer() {
                return Err(CheckError::BadOperand(
                    line,
                    operator.as_str(),
                    left.as_str(),
                ));
            }
            Ok(Type::Bool)
        }

        BinOp::Shl | BinOp::Shr => {
            if !left.is_integer() {
                return Err(CheckError::BadOperand(line, operator.as_str(), left.as_str()));
            }
            if !right.is_integer() {
                return Err(CheckError::BadOperand(
                    line,
                    operator.as_str(),
                    right.as_str(),
                ));
            }
            Ok(left)
        }

        _ => {
            if !left.is_integer() {
                return Err(CheckError::BadOperand(line, operator.as_str(), left.as_str()));
            }
            if left != right {
                return Err(CheckError::Mismatch(
                    line,
                    left.as_str(),
                    right.as_str(),
                    alloc::format!("operation {}", operator.as_str()),
                ));
            }
            Ok(left)
        }
    }
}

/// Signature of a user function or of a builtin.
pub fn signature_of(program: &Program, name: &str) -> Option<Signature> {
    if let Some(function) = program.function(name) {
        return Some(Signature {
            params: function.params.iter().map(|param| param.ty).collect(),
            result: function.result,
            builtin: None,
        });
    }
    BUILTINS
        .iter()
        .find(|entry| entry.name == name)
        .map(|entry| Signature {
            params: entry.params.to_vec(),
            result: entry.result,
            builtin: Some(entry.kind),
        })
}

// ============================================================
//  Constant folding
// ============================================================

/// Evaluates a `const` initialiser.
///
/// Only literals and already-folded constants: an initialiser calling a
/// function would need the function to run at compile time, which means
/// an interpreter inside the compiler. `interp` is that interpreter, and
/// wiring it in here would make a `const` able to print.
fn fold_const(
    program: &Program,
    known: &[(String, Type, ConstValue)],
    item: &Const,
) -> Result<ConstValue, CheckError> {
    let _ = program;
    match &item.value {
        Expr::Int(value, _) => {
            if item.ty == Type::I64 && *value > i64::MAX as u64 + 1 {
                return Err(CheckError::LiteralOutOfRange(
                    item.line,
                    *value,
                    item.ty.as_str(),
                ));
            }
            if !item.ty.is_integer() {
                return Err(CheckError::Mismatch(
                    item.line,
                    item.ty.as_str(),
                    "un entier",
                    item.name.clone(),
                ));
            }
            Ok(ConstValue::Int(*value))
        }
        Expr::Unary(UnOp::Neg, inner, line) => {
            let Expr::Int(value, _) = inner.as_ref() else {
                return Err(CheckError::Undefined(*line, item.name.clone()));
            };
            if item.ty != Type::I64 {
                return Err(CheckError::Mismatch(
                    item.line,
                    item.ty.as_str(),
                    "i64",
                    item.name.clone(),
                ));
            }
            Ok(ConstValue::Int((*value as i64).wrapping_neg() as u64))
        }
        Expr::Bool(value) => {
            if item.ty != Type::Bool {
                return Err(CheckError::Mismatch(
                    item.line,
                    item.ty.as_str(),
                    "bool",
                    item.name.clone(),
                ));
            }
            Ok(ConstValue::Bool(*value))
        }
        Expr::Str(text) => {
            if item.ty != Type::Str {
                return Err(CheckError::Mismatch(
                    item.line,
                    item.ty.as_str(),
                    "&str",
                    item.name.clone(),
                ));
            }
            Ok(ConstValue::Str(text.clone()))
        }
        Expr::Path(name, line) => match known.iter().find(|(other, _, _)| other == name) {
            Some((_, ty, value)) if *ty == item.ty => Ok(value.clone()),
            Some((_, ty, _)) => Err(CheckError::Mismatch(
                *line,
                item.ty.as_str(),
                ty.as_str(),
                item.name.clone(),
            )),
            None => Err(CheckError::Undefined(*line, name.clone())),
        },
        other => Err(CheckError::Undefined(other.line(), item.name.clone())),
    }
}

// ============================================================
//  Definite return
// ============================================================

/// Does every path through this block leave the function?
pub fn always_returns(block: &Block) -> bool {
    if block.tail.is_some() {
        return true;
    }
    block.statements.iter().any(statement_returns)
}

fn statement_returns(statement: &Stmt) -> bool {
    match statement {
        Stmt::Return(_, _) => true,
        Stmt::If(_, then_block, Some(else_block), _) => {
            always_returns(then_block) && always_returns(else_block)
        }
        // A `loop` with no `break` never ends, so nothing after it is
        // reachable and the function cannot fall through it.
        Stmt::Loop(body, _) => !contains_break(body),
        _ => false,
    }
}

/// Is there a `break` that would leave *this* loop?
///
/// A `break` inside a nested loop belongs to that loop, so the walk stops
/// descending at every `while` and `loop`. Missing that would make an
/// infinite outer loop look breakable, and a function relying on it would
/// be refused for no reason.
fn contains_break(block: &Block) -> bool {
    block.statements.iter().any(|statement| match statement {
        Stmt::Break(_) => true,
        Stmt::If(_, then_block, else_block, _) => {
            contains_break(then_block)
                || else_block.as_ref().is_some_and(contains_break)
        }
        _ => false,
    })
}
