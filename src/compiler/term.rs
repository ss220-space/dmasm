use dreammaker::ast::*;

use crate::compiler::*;
use crate::Instruction;

pub(super) fn emit(compiler: &mut Compiler<'_>, term: Term) -> Result<EvalKind, CompileError> {
    match term {
        // Nested expression, probably something in brackets
        Term::Expr(expr) => compiler.emit_expr(*expr),

        // Simple stack pushes
        Term::Null => {
            compiler.emit_ins(Instruction::PushVal(Value::Null));
            Ok(EvalKind::Stack)
        }
        Term::Int(i) => {
            compiler.emit_ins(Instruction::PushInt(i));
            Ok(EvalKind::Stack)
        }
        Term::Float(f) => {
            compiler.emit_ins(Instruction::PushVal(Value::Number(f)));
            Ok(EvalKind::Stack)
        }
        Term::String(str) => {
            compiler.emit_ins(Instruction::PushVal(Value::DMString(DMString(str.into()))));
            Ok(EvalKind::Stack)
        }

        // Identifiers. These could be params or globals.
        Term::Ident(ident) => Ok(compiler.emit_find_var(ident)),

        // Resources
        Term::Resource(resource) => {
            compiler.emit_ins(Instruction::PushVal(Value::Resource(resource)));
            Ok(EvalKind::Stack)
        }

        // as() stuff. We might eventually change this to its own EvalKind so statements can act on them better.
        Term::As(val) => {
            compiler.emit_ins(Instruction::PushInt(val.bits() as i32));
            Ok(EvalKind::Stack)
        }

        // Type paths: We don't support the anonymous kind with variable declarations.
        Term::Prefab(prefab) => {
            if !prefab.vars.is_empty() {
                return Err(CompileError::UnsupportedPrefabWithVars);
            }

            let mut path = String::new();

            // TODO: Relative stuff
            for (op, part) in prefab.path {
                use std::fmt::Write;
                write!(&mut path, "{}{}", op, part).unwrap();
            }

            compiler.emit_ins(Instruction::PushVal(Value::Path(path)));
            Ok(EvalKind::Stack)
        }

        Term::Call(ident, args) => {
            // If any of the arguments are a Expression:AssignOp, byond does _crazy_ not-so-well defined things.
            // We can implement this later...
            if args
                .iter()
                .any(|x| matches!(x, Expression::AssignOp { .. }))
            {
                return Err(CompileError::NamedArgumentsNotImplemented);
            }

            match builtin_procs::emit(compiler, &ident, &args)? {
                // Handled by builtin_procs
                Some(kind) => Ok(kind),

                // We've got to call a proc
                None => {
                    let arg_count = args.len() as u32;

                    // Bring all arguments onto the stack
                    for arg in args {
                        let expr = compiler.emit_expr(arg)?;
                        compiler.emit_move_to_stack(expr)?;
                    }

                    // We're treating all Term::Call expressions as global calls
                    compiler.emit_ins(Instruction::CallGlob(
                        arg_count,
                        operands::Proc(format!("/proc/{}", ident)),
                    ));

                    Ok(EvalKind::Stack)
                }
            }
        }

        Term::DynamicCall(lhs, rhs) => {
            // If any of the arguments are a Expression:AssignOp, byond does _crazy_ not-so-well defined things.
            // We can implement this later...
            if rhs.iter().any(|x| matches!(x, Expression::AssignOp { .. })) {
                return Err(CompileError::NamedArgumentsNotImplemented);
            }

            let lhs_len = lhs.len();
            let rhs_len = rhs.len();

            if lhs.is_empty() {
                return Err(CompileError::MissingArgument {
                    proc: "call".to_owned(),
                    index: 1,
                    name: "ProcRef/Object/LibName".to_owned(),
                });
            }

            if lhs_len > 2 {
                return Err(CompileError::TooManyArguments {
                    proc: "call".to_owned(),
                    expected: 2,
                });
            }

            // Push LHS
            for expr in lhs {
                let kind = compiler.emit_expr(expr)?;
                compiler.emit_move_to_stack(kind)?;
            }

            // Push RHS
            for expr in rhs {
                let kind = compiler.emit_expr(expr)?;
                compiler.emit_move_to_stack(kind)?;
            }

            match lhs_len {
                1 => compiler.emit_ins(Instruction::CallPath(rhs_len as u32)),
                2 => compiler.emit_ins(Instruction::CallName(rhs_len as u32)),

                _ => unreachable!(),
            }

            Ok(EvalKind::Stack)
        }

        Term::SelfCall { .. } | Term::ParentCall { .. } => {
            // Can't implement these until we compile full procs
            // Well, maybe we could
            return Err(CompileError::UnsupportedRelativeCall);
        }

        Term::New { type_, args } => match type_ {
            NewType::Prefab(prefab) => {
                if !prefab.vars.is_empty() {
                    return Err(CompileError::UnsupportedPrefabWithVars);
                }

                let path = format!("{}", FormatTypePath(&prefab.path));
                let typeval = operands::Value::Path(path);
                compiler.emit_ins(Instruction::PushVal(typeval));

                emit_new(compiler, args)
            }

            NewType::MiniExpr { ident, fields } => {
                let var = compiler.emit_find_var(ident);
                let follows: Vec<Follow> = fields.into_iter().map(|f| f.into()).collect();

                let kind = follow::emit(compiler, follows, var)?;
                compiler.emit_move_to_stack(kind)?;

                emit_new(compiler, args)
            }

            NewType::Implicit => Err(CompileError::UnsupportedImplicitNew),
        },

        Term::Locate { args, in_list } => {
            let args_len = args.len();

            // Push everything first to simplify later code
            for expr in args {
                let kind = compiler.emit_expr(expr)?;
                compiler.emit_move_to_stack(kind)?;
            }

            match args_len {
                // locate()
                0 => return Err(CompileError::UnsupportedImplicitLocate),

                // locate(ref|type)
                1 if in_list.is_none() => {
                    compiler.emit_ins(Instruction::LocateRef);
                }

                // locate(type) in container
                1 if in_list.is_some() => {
                    let kind = compiler.emit_expr(*in_list.unwrap())?;
                    compiler.emit_move_to_stack(kind)?;

                    compiler.emit_ins(Instruction::LocateType);
                }

                // locate(X, Y, Z)
                3 => {
                    compiler.emit_ins(Instruction::LocatePos);
                }

                _ => return Err(CompileError::InvalidLocateArgs),
            }

            Ok(EvalKind::Stack)
        }

        other => Err(CompileError::UnsupportedExpressionTerm(other)),
    }
}

// Assuming the type to create will always be on the stack
fn emit_new(
    compiler: &mut Compiler<'_>,
    args: Option<Vec<Expression>>,
) -> Result<EvalKind, CompileError> {
    let mut arg_count = 0;
    if let Some(args) = args {
        arg_count = args.len() as u32;
        for arg in args {
            let expr = compiler.emit_expr(arg)?;
            compiler.emit_move_to_stack(expr)?;
        }
    }

    compiler.emit_ins(Instruction::New(arg_count));
    Ok(EvalKind::Stack)
}