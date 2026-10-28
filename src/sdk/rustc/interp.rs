//! 9.5.1 -- Tree-walking interpreter (spec 13.2, "interpreter" column).
//!
//! ## Why an interpreter as well as a compiler
//!
//! 13.2 gives Rust two columns, and they answer two different questions.
//! The compiler answers "what will this program do on the machine"; the
//! interpreter answers "what does this program do", now, with no linking
//! and no binary written to a volume. `dev interpret main.rs` is the
//! second, and so is every check in the self-test that compares a
//! computed value against an expected one -- comparing them through the
//! compiler would mean loading a binary per assertion.
//!
//! ## It shares the tree, and that is the point
//!
//! Both back-ends read `ast::Program` unchanged. A program that gave two
//! answers would have to be two programs, so the interpreter is also the
//! oracle the compiler is checked against: the self-test runs the same
//! source both ways and compares what comes out.
//!
//! ## Two bounds, again
//!
//! A step budget and a stack floor, for the reason `sdk::script` states
//! at length: this runs on a 32 KiB kernel stack, inside a scheduled
//! task, and a `loop {}` in a source must not cost a core until reboot.
//!
//! ## Integers wrap
//!
//! `wrapping_add` and its family, not `checked_add`. That is the
//! **release** behaviour of Rust, and it is the one the compiled code
//! has -- x86 does not trap on overflow and no check is emitted. The two
//! back-ends must agree, and agreeing on the cheaper of the two is what
//! keeps the compiler honest rather than the interpreter strict.

use alloc::string::String;
use alloc::vec::Vec;

use super::ast::*;
use super::check::{Checked, ConstValue};
use super::{BUILTINS, Builtin};

/// Default step budget.
pub const DEFAULT_BUDGET: u64 = 400_000;

/// Maximum call depth.
///
/// Ten, and the number comes from the stack rather than from taste.
/// One level of interpreted call costs the frames of `call`,
/// `call_inner`, `block`, `statement`, `expr` and `apply`, and in a debug
/// build that is a few kilobytes -- on a 128 KiB task stack already part
/// spent by whoever called the interpreter. `sdk::script` was bitten by
/// exactly this: twenty-four levels wanted more than 200 KiB, and the
/// symptom was neighbouring stacks quietly overwritten.
///
/// The counted limit is set **below** what the stack floor would catch,
/// so a program that recurses too far gets "recursion trop profonde"
/// with a number behind it rather than a guard that fired on a byte
/// count. Twelve levels were measured on this build -- the self-test
/// walks up to the wall and prints where it found it -- and ten is the
/// figure kept, with the two levels of slack that a differently
/// optimised build could eat.
///
/// The **compiled** form has no such limit: it recurses on its own ring 3
/// stack, which the manifest sizes, and the self-test proves it by
/// running a recursion in a sandbox that the interpreter would refuse.
pub const MAX_DEPTH: usize = 10;

/// Stack margin below which one more level is refused, in bytes.
///
/// The physical control, and the only one the compiler's frame sizes
/// cannot contradict. `MAX_DEPTH` counts levels; this counts the bytes
/// that are actually left.
const STACK_MARGIN: u64 = 16 * 1024;

/// Longest line the buffer holds, matching `codegen::BUFFER_SIZE`.
const BUFFER_SIZE: usize = 512;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunError {
    /// Step budget exhausted: the program does not terminate.
    Budget,
    /// Recursion deeper than the kernel stack allows.
    TooDeep,
    DivisionByZero(usize),
    /// The program called `exit`.
    Exited(i64),
    /// A name the checker should have caught. Reaching it means the two
    /// passes disagree, which is worth its own message.
    Internal(String),
}

impl RunError {
    pub fn message(&self) -> String {
        match self {
            RunError::Budget => {
                String::from("budget de pas epuise - le programme ne termine pas")
            }
            RunError::TooDeep => String::from("recursion trop profonde"),
            RunError::DivisionByZero(line) => {
                alloc::format!("ligne {} : panique - division par zero", line)
            }
            RunError::Exited(code) => alloc::format!("exit({})", code),
            RunError::Internal(detail) => {
                alloc::format!("incoherence interne de l'interprete : {}", detail)
            }
        }
    }
}

/// A value at run time. One word, like the compiled representation --
/// except that a `&str` carries its text instead of an address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Int(u64),
    Bool(bool),
    Str(String),
    Unit,
}

impl Value {
    pub fn as_int(&self) -> u64 {
        match self {
            Value::Int(value) => *value,
            Value::Bool(value) => u64::from(*value),
            _ => 0,
        }
    }

    fn as_bool(&self) -> bool {
        match self {
            Value::Bool(value) => *value,
            Value::Int(value) => *value != 0,
            _ => false,
        }
    }

    /// How `println!` renders it, signedness taken from the static type.
    pub fn render(&self, ty: Type) -> String {
        match self {
            Value::Int(value) if ty == Type::I64 => alloc::format!("{}", *value as i64),
            Value::Int(value) => alloc::format!("{}", value),
            Value::Bool(true) => String::from("true"),
            Value::Bool(false) => String::from("false"),
            Value::Str(text) => text.clone(),
            Value::Unit => String::from("()"),
        }
    }
}

/// What a statement did to the flow.
enum Flow {
    Next,
    Break,
    Continue,
    Return(Value),
}

/// One run of a program.
pub struct Interpreter<'a> {
    program: &'a Program,
    checked: &'a Checked,
    /// Scopes of the current call, innermost last.
    scopes: Vec<Vec<(String, Value, Type)>>,
    /// Lines the program produced. Captured rather than printed, so the
    /// sandbox and the self-test can look at them.
    pub output: Vec<String>,
    /// The line being built by `print!`, flushed by `println!`.
    pending: String,
    steps: u64,
    budget: u64,
    depth: usize,
    stack_floor: u64,
    /// A `break`, `continue` or `return` that left an expression.
    ///
    /// `loop { n += 1; if n == 7 { break; } }` is the shape that makes
    /// this necessary: the trailing `if` is the block's tail, so it is
    /// evaluated as an **expression**, and an expression returns a value,
    /// not a flow. Without somewhere to put the `break`, it was
    /// swallowed and the loop ran until the step budget stopped it.
    ///
    /// The compiled form has no such problem -- a `break` there is a
    /// `jmp` to a label that lexically encloses it, whatever the
    /// expression around it. This field is what makes the interpreter
    /// agree with it.
    escaped: Option<Flow>,
    /// Exit code, when the program called `exit`.
    pub exit_code: Option<i64>,
}

/// Runs a checked program from `main`.
pub fn run<'a>(
    program: &'a Program,
    checked: &'a Checked,
    budget: u64,
) -> (Interpreter<'a>, Result<(), RunError>) {
    let bottom = crate::scheduler::current_stack_bottom();
    let mut interpreter = Interpreter {
        program,
        checked,
        scopes: Vec::new(),
        output: Vec::new(),
        pending: String::new(),
        steps: 0,
        budget,
        depth: 0,
        stack_floor: if bottom == 0 { 0 } else { bottom + STACK_MARGIN },
        escaped: None,
        exit_code: None,
    };

    let outcome = interpreter.call("main", &[]).map(|_| ());
    // A line left open by a `print!` with no `println!` is still a line
    // the program produced. Dropping it would make the last thing a
    // program said the thing hardest to see.
    interpreter.flush_pending();

    let outcome = match outcome {
        Err(RunError::Exited(code)) => {
            interpreter.exit_code = Some(code);
            Ok(())
        }
        other => other,
    };
    (interpreter, outcome)
}

impl Interpreter<'_> {
    fn tick(&mut self) -> Result<(), RunError> {
        self.steps += 1;
        if self.steps > self.budget {
            return Err(RunError::Budget);
        }
        if self.stack_floor != 0 && crate::scheduler::stack_pointer() < self.stack_floor {
            return Err(RunError::TooDeep);
        }
        Ok(())
    }

    fn flush_pending(&mut self) {
        if !self.pending.is_empty() {
            let line = core::mem::take(&mut self.pending);
            self.output.push(line);
        }
    }

    fn lookup(&self, name: &str) -> Option<(Value, Type)> {
        for scope in self.scopes.iter().rev() {
            if let Some((_, value, ty)) = scope.iter().find(|(other, _, _)| other == name) {
                return Some((value.clone(), *ty));
            }
        }
        self.checked
            .constants
            .iter()
            .find(|(other, _, _)| other == name)
            .map(|(_, ty, value)| {
                (
                    match value {
                        ConstValue::Int(raw) => Value::Int(*raw),
                        ConstValue::Bool(raw) => Value::Bool(*raw),
                        ConstValue::Str(raw) => Value::Str(raw.clone()),
                    },
                    *ty,
                )
            })
    }

    fn declare(&mut self, name: &str, value: Value, ty: Type) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.push((String::from(name), value, ty));
        }
    }

    fn assign(&mut self, name: &str, value: Value) {
        for scope in self.scopes.iter_mut().rev() {
            if let Some(slot) = scope.iter_mut().find(|(other, _, _)| other == name) {
                slot.1 = value;
                return;
            }
        }
    }

    /// Calls a function by name.
    pub fn call(&mut self, name: &str, args: &[Value]) -> Result<Value, RunError> {
        self.tick()?;
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            self.depth -= 1;
            return Err(RunError::TooDeep);
        }

        let result = self.call_inner(name, args);
        self.depth -= 1;
        result
    }

    fn call_inner(&mut self, name: &str, args: &[Value]) -> Result<Value, RunError> {
        if let Some(entry) = BUILTINS.iter().find(|entry| entry.name == name) {
            return self.builtin(entry.kind, args);
        }

        let Some(function) = self.program.function(name) else {
            return Err(RunError::Internal(alloc::format!(
                "fonction \"{}\" absente a l'execution",
                name
            )));
        };

        // A fresh scope stack per call: a function must not see the
        // locals of its caller, and sharing the vector would give it
        // exactly that.
        let saved = core::mem::take(&mut self.scopes);
        self.scopes.push(Vec::new());
        for (param, value) in function.params.iter().zip(args.iter()) {
            self.declare(&param.name, value.clone(), param.ty);
        }

        let outcome = self.block(&function.body);
        let value = match outcome {
            Ok((Flow::Return(value), _)) => Ok(value),
            Ok((_, tail)) => Ok(tail),
            Err(error) => Err(error),
        };

        self.scopes = saved;
        value
    }

    fn builtin(&mut self, kind: Builtin, args: &[Value]) -> Result<Value, RunError> {
        match kind {
            Builtin::Exit => {
                let code = args.first().map(|value| value.as_int() as i64).unwrap_or(0);
                Err(RunError::Exited(code))
            }
            // Sleeping and yielding are real calls in the compiled
            // program and no-ops here: an interpreter that slept would
            // make `dev interpret` take as long as the program says,
            // inside a kernel task, for no gain in fidelity.
            Builtin::SleepMs | Builtin::Yield => Ok(Value::Unit),
            Builtin::Pid => Ok(Value::Int(crate::scheduler::current_pid())),
        }
    }

    /// Runs a block. Returns how it ended and, if it ended normally, the
    /// value of its tail expression.
    fn block(&mut self, block: &Block) -> Result<(Flow, Value), RunError> {
        self.scopes.push(Vec::new());
        let mut result = (Flow::Next, Value::Unit);

        for statement in block.statements.iter() {
            match self.statement(statement)? {
                Flow::Next => {}
                other => {
                    result = (other, Value::Unit);
                    self.scopes.pop();
                    return Ok(result);
                }
            }
        }
        if let Some(tail) = block.tail.as_ref() {
            let value = self.expr(tail)?;
            result = match self.escaped.take() {
                Some(flow) => (flow, Value::Unit),
                None => (Flow::Next, value),
            };
        }

        self.scopes.pop();
        Ok(result)
    }

    fn statement(&mut self, statement: &Stmt) -> Result<Flow, RunError> {
        self.tick()?;

        match statement {
            Stmt::Let {
                name,
                declared,
                value,
                ..
            } => {
                let evaluated = self.expr(value)?;
                let ty = declared.unwrap_or_else(|| self.type_of(value));
                self.declare(name, evaluated, ty);
                Ok(Flow::Next)
            }

            Stmt::Assign {
                name,
                op,
                value,
                line,
            } => {
                let right = self.expr(value)?;
                let next = match op {
                    None => right,
                    Some(operator) => {
                        let (current, ty) = self
                            .lookup(name)
                            .ok_or_else(|| RunError::Internal(name.clone()))?;
                        self.apply(*operator, current, right, ty, *line)?
                    }
                };
                self.assign(name, next);
                Ok(Flow::Next)
            }

            Stmt::If(condition, then_block, else_block, _) => {
                if self.expr(condition)?.as_bool() {
                    Ok(self.block(then_block)?.0)
                } else if let Some(other) = else_block {
                    Ok(self.block(other)?.0)
                } else {
                    Ok(Flow::Next)
                }
            }

            Stmt::While(condition, body, _) => {
                loop {
                    self.tick()?;
                    if !self.expr(condition)?.as_bool() {
                        break;
                    }
                    match self.block(body)?.0 {
                        Flow::Break => break,
                        Flow::Return(value) => return Ok(Flow::Return(value)),
                        _ => {}
                    }
                }
                Ok(Flow::Next)
            }

            Stmt::Loop(body, _) => {
                loop {
                    self.tick()?;
                    match self.block(body)?.0 {
                        Flow::Break => break,
                        Flow::Return(value) => return Ok(Flow::Return(value)),
                        _ => {}
                    }
                }
                Ok(Flow::Next)
            }

            Stmt::Print { .. } => self.print(statement),

            Stmt::Break(_) => Ok(Flow::Break),
            Stmt::Continue(_) => Ok(Flow::Continue),

            Stmt::Return(value, _) => {
                let evaluated = match value {
                    Some(expr) => self.expr(expr)?,
                    None => Value::Unit,
                };
                Ok(Flow::Return(evaluated))
            }

            Stmt::Expr(expr, _) => {
                self.expr(expr)?;
                Ok(self.escaped.take().unwrap_or(Flow::Next))
            }

        }
    }

    /// `print!` / `println!`, kept out of `statement`.
    ///
    /// Not tidiness: `statement` sits on the recursive path, and in a
    /// debug build its frame is the sum of every arm's locals. The
    /// formatting arm is the widest of them and the one recursion never
    /// needs, so moving it out is stack given back to recursion.
    #[inline(never)]
    fn print(&mut self, statement: &Stmt) -> Result<Flow, RunError> {
        let Stmt::Print {
            parts,
            args,
            newline,
            ..
        } = statement
        else {
            return Ok(Flow::Next);
        };

        for part in parts.iter() {
            match part {
                FormatPart::Literal(text) => self.pending.push_str(text),
                FormatPart::Hole(index) => {
                    let Some(argument) = args.get(*index) else {
                        continue;
                    };
                    let ty = self.type_of(argument);
                    let value = self.expr(argument)?;
                    self.pending.push_str(&value.render(ty));
                }
            }
            if self.pending.len() > BUFFER_SIZE {
                self.pending.truncate(BUFFER_SIZE);
            }
        }
        if *newline {
            let line = core::mem::take(&mut self.pending);
            self.output.push(line);
        }
        Ok(Flow::Next)
    }

    fn expr(&mut self, expr: &Expr) -> Result<Value, RunError> {
        self.tick()?;

        match expr {
            Expr::Int(value, _) => Ok(Value::Int(*value)),
            Expr::Bool(value) => Ok(Value::Bool(*value)),
            Expr::Str(text) => Ok(Value::Str(text.clone())),
            Expr::Unit => Ok(Value::Unit),

            Expr::Path(name, _) => self.path(name),

            Expr::Unary(operator, inner, _) => self.unary(*operator, inner),

            // Short circuit, exactly as the compiled form does it: the
            // right side must not run, and it may print.
            Expr::Binary(BinOp::And, left, right, _) => {
                if !self.expr(left)?.as_bool() {
                    return Ok(Value::Bool(false));
                }
                Ok(Value::Bool(self.expr(right)?.as_bool()))
            }
            Expr::Binary(BinOp::Or, left, right, _) => {
                if self.expr(left)?.as_bool() {
                    return Ok(Value::Bool(true));
                }
                Ok(Value::Bool(self.expr(right)?.as_bool()))
            }

            Expr::Binary(operator, left, right, line) => {
                let ty = self.type_of(left);
                let left_value = self.expr(left)?;
                let right_value = self.expr(right)?;
                self.apply(*operator, left_value, right_value, ty, *line)
            }

            Expr::Cast(inner, _, _) => self.expr(inner).map(|value| match value {
                Value::Bool(flag) => Value::Int(u64::from(flag)),
                other => other,
            }),

            Expr::Call(name, args, _) => self.call_expr(name, args),

            Expr::If(condition, then_block, else_block, _) => {
                if self.expr(condition)?.as_bool() {
                    self.valued(then_block)
                } else if let Some(other) = else_block {
                    self.valued(other)
                } else {
                    Ok(Value::Unit)
                }
            }

            Expr::Block(block) => self.valued(block),
        }
    }

    /// A name in value position.
    ///
    /// Out of `expr` because `lookup` clones a `Value`, and a `Value`
    /// carries a `String`: the temporaries of that clone would otherwise
    /// sit in `expr`'s frame, which is on the recursive path. Every
    /// kilobyte taken out of that frame is a level of recursion given
    /// back.
    #[inline(never)]
    fn path(&mut self, name: &str) -> Result<Value, RunError> {
        self.lookup(name)
            .map(|(value, _)| value)
            .ok_or_else(|| RunError::Internal(String::from(name)))
    }

    #[inline(never)]
    fn unary(&mut self, operator: UnOp, inner: &Expr) -> Result<Value, RunError> {
        let ty = self.type_of(inner);
        let value = self.expr(inner)?;
        Ok(match operator {
            UnOp::Neg => Value::Int((value.as_int() as i64).wrapping_neg() as u64),
            UnOp::Not => match ty {
                Type::Bool => Value::Bool(!value.as_bool()),
                _ => Value::Int(!value.as_int()),
            },
        })
    }

    #[inline(never)]
    fn call_expr(&mut self, name: &str, args: &[Expr]) -> Result<Value, RunError> {
        let mut values = Vec::with_capacity(args.len());
        for argument in args.iter() {
            values.push(self.expr(argument)?);
        }
        self.call(name, &values)
    }

    /// Runs a block in value position, keeping any flow that escapes.
    ///
    /// The value and the flow are two different answers to two different
    /// questions, and a block can produce either. Returning only the
    /// value is what lost the `break`.
    fn valued(&mut self, block: &Block) -> Result<Value, RunError> {
        let (flow, value) = self.block(block)?;
        match flow {
            Flow::Next => Ok(value),
            other => {
                self.escaped = Some(other);
                Ok(Value::Unit)
            }
        }
    }

    /// Applies a binary operator to two values already evaluated.
    fn apply(
        &mut self,
        operator: BinOp,
        left: Value,
        right: Value,
        ty: Type,
        line: usize,
    ) -> Result<Value, RunError> {
        // Strings compare, and nothing else applies to them.
        if let (Value::Str(a), Value::Str(b)) = (&left, &right) {
            return Ok(match operator {
                BinOp::Eq => Value::Bool(a == b),
                BinOp::Ne => Value::Bool(a != b),
                _ => Value::Unit,
            });
        }
        if let (Value::Bool(a), Value::Bool(b)) = (&left, &right)
            && matches!(operator, BinOp::Eq | BinOp::Ne)
        {
            return Ok(Value::Bool(if operator == BinOp::Eq { a == b } else { a != b }));
        }

        let a = left.as_int();
        let b = right.as_int();
        let signed = ty.is_signed();

        Ok(match operator {
            BinOp::Add => Value::Int(a.wrapping_add(b)),
            BinOp::Sub => Value::Int(a.wrapping_sub(b)),
            BinOp::Mul => Value::Int(a.wrapping_mul(b)),
            BinOp::Div => {
                if b == 0 {
                    return Err(RunError::DivisionByZero(line));
                }
                if signed {
                    Value::Int(((a as i64).wrapping_div(b as i64)) as u64)
                } else {
                    Value::Int(a / b)
                }
            }
            BinOp::Rem => {
                if b == 0 {
                    return Err(RunError::DivisionByZero(line));
                }
                if signed {
                    Value::Int(((a as i64).wrapping_rem(b as i64)) as u64)
                } else {
                    Value::Int(a % b)
                }
            }
            BinOp::BitAnd => Value::Int(a & b),
            BinOp::BitOr => Value::Int(a | b),
            BinOp::BitXor => Value::Int(a ^ b),
            // The count is masked to six bits, because that is what the
            // processor does and the two back-ends must agree.
            BinOp::Shl => Value::Int(a.wrapping_shl((b & 63) as u32)),
            BinOp::Shr => {
                if signed {
                    Value::Int(((a as i64).wrapping_shr((b & 63) as u32)) as u64)
                } else {
                    Value::Int(a.wrapping_shr((b & 63) as u32))
                }
            }
            BinOp::Eq => Value::Bool(a == b),
            BinOp::Ne => Value::Bool(a != b),
            BinOp::Lt if signed => Value::Bool((a as i64) < (b as i64)),
            BinOp::Le if signed => Value::Bool((a as i64) <= (b as i64)),
            BinOp::Gt if signed => Value::Bool((a as i64) > (b as i64)),
            BinOp::Ge if signed => Value::Bool((a as i64) >= (b as i64)),
            BinOp::Lt => Value::Bool(a < b),
            BinOp::Le => Value::Bool(a <= b),
            BinOp::Gt => Value::Bool(a > b),
            BinOp::Ge => Value::Bool(a >= b),
            BinOp::And | BinOp::Or => Value::Bool(left.as_bool() && right.as_bool()),
        })
    }

    /// Static type of an expression, for rendering and for signedness.
    fn type_of(&self, expr: &Expr) -> Type {
        match expr {
            Expr::Int(_, Some(ty)) => *ty,
            Expr::Int(_, None) => Type::I64,
            Expr::Bool(_) => Type::Bool,
            Expr::Str(_) => Type::Str,
            Expr::Unit => Type::Unit,
            Expr::Path(name, _) => self.lookup(name).map(|(_, ty)| ty).unwrap_or(Type::I64),
            Expr::Unary(UnOp::Neg, _, _) => Type::I64,
            Expr::Unary(UnOp::Not, inner, _) => self.type_of(inner),
            Expr::Binary(operator, left, _, _) => {
                if operator.is_comparison() || matches!(operator, BinOp::And | BinOp::Or) {
                    Type::Bool
                } else {
                    self.type_of(left)
                }
            }
            Expr::Cast(_, ty, _) => *ty,
            Expr::Call(name, _, _) => super::check::signature_of(self.program, name)
                .map(|signature| signature.result)
                .unwrap_or(Type::Unit),
            Expr::If(_, then_block, _, _) => then_block
                .tail
                .as_ref()
                .map(|tail| self.type_of(tail))
                .unwrap_or(Type::Unit),
            Expr::Block(block) => block
                .tail
                .as_ref()
                .map(|tail| self.type_of(tail))
                .unwrap_or(Type::Unit),
        }
    }

    pub fn steps_used(&self) -> u64 {
        self.steps
    }
}
