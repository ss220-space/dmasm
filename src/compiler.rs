use dreammaker::ast::Expression;
use dreammaker::ast::Follow;
use dreammaker::ast::PropertyAccessKind;
use dreammaker::ast::{AssignOp, BinaryOp, UnaryOp};

use crate::operands::{self, DMString, Label, Value, Variable};
use crate::Instruction;
use crate::Node;

mod assignment;

mod binary_ops;
mod builtin_procs;
mod chain_builder;
mod follow;
mod term;
mod ternary;
mod unary;

use chain_builder::ChainBuilder;

// TODO: Think
fn is_writable(var: &Variable) -> bool {
    match var {
        // Does Field count? We probably don't hit that code path but it might count
        Variable::Usr
        | Variable::Src
        | Variable::Args
        | Variable::Dot
        | Variable::CacheIndex
        // TODO: These can be constant too.
        | Variable::Arg { .. }
        | Variable::Local { .. }
        // TODO: Stuff like global.vars are constant and should return false here.
        | Variable::Global { .. } => true,
        _ => false,
    }
}

#[derive(Debug)]
pub enum CompileError {
    ParseError(dreammaker::DMError),
    UnsupportedExpressionTerm(dreammaker::ast::Term),
    UnsupportedPrefabWithVars,
    ExpectedLValue,
    ExpectedFieldReference,
    NamedArgumentsNotImplemented,
    IncorrectArgCount(String),
    MissingArgument {
        proc: String,
        index: u32,
    },
    TooManyArguments {
        proc: String,
        expected: u32,
    },
    UnsupportedBuiltin {
        proc: String,
    },
    UnexpectedRange,
    UnexpectedGlobal,
    UnexpectedNamedArguments,
    UnsupportedImplicitNew,
    UnsupportedRelativeCall,
    UnsupportedImplicitLocate,
    UnsupportedSafeListAccess,
    InvalidLocateArgs,
}

impl From<dreammaker::DMError> for CompileError {
    fn from(err: dreammaker::DMError) -> Self {
        Self::ParseError(err)
    }
}

pub fn compile_expr(code: &str, params: &[&str]) -> Result<Vec<Node>, CompileError> {
    let mut compiler = Compiler {
        params,
        nodes: vec![Node::Instruction(
            Instruction::DbgFile(DMString(b"<dmasm expression>".to_vec())),
            (),
        )],
        label_count: 0,
        short_circuit_labels: vec![],
    };

    // Expression begin
    let ctx = dreammaker::Context::default();
    let lexer = dreammaker::lexer::Lexer::new(&ctx, Default::default(), code.as_bytes());
    let expr = dreammaker::parser::parse_expression(&ctx, Default::default(), lexer)?;

    // TODO: Error check expr

    let kind = compiler.emit_expr(expr)?;
    compiler.emit_move_to_stack(kind)?;

    let mut arg_id = 0;
    for _ in params {
        compiler.emit_ins(Instruction::GetVar(Variable::Arg(arg_id)));
        arg_id += 1;
    }

    compiler.emit_ins(Instruction::NewList(params.len() as u32 + 1));
    compiler.emit_ins(Instruction::Ret);
    Ok(compiler.nodes)
}

#[derive(Debug, PartialEq)]
enum EvalKind {
    // The result of the expression will be on the top of the stack
    Stack,

    // The result of the expression is a list entry L[K] where the top of the stack is the index and the 2nd top of the stack is the list
    ListRef,

    // The result of the expression is 2 values on the Stack due to the `To` operator
    Range,

    // The result of the expression is the `global` pseudo-object
    Global,

    // The result of the expression can be accessed using a Variable operand
    Var(Variable),

    // Similar to Var, but more state
    Field(ChainBuilder, String),
    // TODO: Eval?
}

#[derive(Clone)]
struct Compiler<'a> {
    params: &'a [&'a str],
    nodes: Vec<Node>,
    label_count: u32,
    short_circuit_labels: Vec<(String, bool)>,
}

impl<'a> Compiler<'a> {
    fn emit_ins(&mut self, ins: Instruction) {
        self.nodes.push(Node::Instruction(ins, ()));
    }

    fn emit_label(&mut self, label: String) {
        self.nodes.push(Node::Label(label));
    }

    fn emit_find_var(&mut self, ident: dreammaker::ast::Ident) -> EvalKind {
        if let Some(index) = self.params.iter().rposition(|x| *x == ident) {
            return EvalKind::Var(Variable::Arg(index as u32));
        }

        match ident.as_str() {
            "." => EvalKind::Var(Variable::Dot),
            "usr" => EvalKind::Var(Variable::Usr),
            "src" => EvalKind::Var(Variable::Src),
            "args" => EvalKind::Var(Variable::Args),
            "world" => EvalKind::Var(Variable::World),
            "global" => EvalKind::Global,

            // Anything else is treated as a global var
            _ => EvalKind::Var(Variable::Global(DMString(ident.into()))),
        }
    }

    fn emit_move_to_stack(&mut self, kind: EvalKind) -> Result<EvalKind, CompileError> {
        match kind {
            EvalKind::Stack => {}

            EvalKind::ListRef => {
                self.emit_ins(Instruction::ListGet);
            }

            EvalKind::Range => return Err(CompileError::UnexpectedRange),
            EvalKind::Global => return Err(CompileError::UnexpectedGlobal),

            EvalKind::Var(var) => {
                self.emit_ins(Instruction::GetVar(var));
            }

            EvalKind::Field(builder, field) => {
                let var = builder.get_field(DMString(field.into()));
                self.emit_ins(Instruction::GetVar(var));
            }
        }

        Ok(EvalKind::Stack)
    }

    // TODO: lots of copied code from emit_move_to_stack
    fn emit_move_to_chain_builder(&mut self, kind: EvalKind) -> Result<ChainBuilder, CompileError> {
        match kind {
            EvalKind::Stack => {
                self.emit_ins(Instruction::SetVar(Variable::Cache));
                Ok(ChainBuilder::begin(Variable::Cache))
            }

            EvalKind::ListRef => {
                self.emit_ins(Instruction::ListGet);
                self.emit_ins(Instruction::SetVar(Variable::Cache));
                Ok(ChainBuilder::begin(Variable::Cache))
            }

            EvalKind::Range => Err(CompileError::UnexpectedRange),
            EvalKind::Global => Err(CompileError::UnexpectedGlobal),

            EvalKind::Field(mut builder, field) => {
                builder.append(DMString(field.into()));
                Ok(builder)
            }

            EvalKind::Var(var) => Ok(ChainBuilder::begin(var)),
        }
    }

    fn short_circuit(&mut self) -> String {
        let label = self.short_circuit_labels.last_mut().unwrap();
        label.1 = true;
        label.0.to_owned()
    }

    fn emit_inner_expr(&mut self, expr: Expression) -> Result<EvalKind, CompileError> {
        match expr {
            Expression::TernaryOp { cond, if_, else_ } => ternary::emit(self, *cond, *if_, *else_),
            Expression::BinaryOp { op, lhs, rhs } => binary_ops::emit(self, op, *lhs, *rhs),
            Expression::AssignOp { op, lhs, rhs } => assignment::emit(self, op, *lhs, *rhs),

            Expression::Base {
                unary,
                term,
                follow,
            } => {
                let unspanned_follows: Vec<Follow> = follow.into_iter().map(|f| f.elem).collect();
                let kind = term::emit(self, term.elem)?;
                let kind = follow::emit(self, unspanned_follows, kind)?;
                let kind = unary::emit(self, unary, kind)?;
                Ok(kind)
            }
        }
    }

    fn emit_expr(&mut self, expr: Expression) -> Result<EvalKind, CompileError> {
        let label = format!("LAB_{:0>4X}", self.label_count);
        self.label_count += 1;
        self.short_circuit_labels.push((label, false));

        let kind = self.emit_inner_expr(expr)?;

        let (label, used) = self.short_circuit_labels.pop().unwrap();

        // We only care if the label was actually used
        if used {
            self.emit_move_to_stack(kind)?;
            self.emit_label(label);
            return Ok(EvalKind::Stack);
        }

        Ok(kind)
    }
}

#[test]
fn compile_test() {
    let context: dreammaker::Context = Default::default();
    let lexer =
        dreammaker::lexer::Lexer::new(&context, Default::default(), "pick(1;;2,3;4)".as_bytes());
    let expr = dreammaker::parser::parse_expression(&context, Default::default(), lexer);
    context.assert_success();
    println!("{:#?}\n\n\n", expr);

    let expr = compile_expr("a?.b++", &["a"]);
    println!("{:#?}", expr);

    if let Ok(expr) = expr {
        println!("{}", crate::format(&expr));
        let code = crate::assembler::assemble(&expr, &mut crate::TestAssembleEnv);
        println!("{:#x?}", code);
    }
}
