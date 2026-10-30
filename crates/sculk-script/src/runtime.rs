use crate::{
    ast::Value,
    compiler::{Expression, Operation},
    CompiledScripts, Limits, Span,
};
use sculk_bot::{Context, OutboundError};
use std::{borrow::Cow, fmt::Write, sync::Arc};
use tokio::time::Instant;

/// Runtime failures contain no text, private identifiers or adapter internals.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RuntimeError {
    #[error("script cancelled; partial effects may have occurred")]
    Cancelled,
    #[error("script deadline exceeded; partial effects may have occurred")]
    Deadline,
    #[error("script execution step budget exceeded")]
    Steps,
    #[error("script string/output limit exceeded")]
    StringLimit,
    #[error("script expression requires unavailable event context")]
    Context,
    #[error("required command argument is missing")]
    Argument,
    #[error("script permission denied")]
    Permission,
    #[error("invalid compiled handler or execution state")]
    Invalid,
    #[error(transparent)]
    Outbound(#[from] OutboundError),
}
/// Location of a failure; default formatting excludes source literals.
#[derive(Clone, Copy, Debug, thiserror::Error)]
#[error("{error}")]
pub struct ExecutionError {
    pub error: RuntimeError,
    pub span: Option<Span>,
}
/// Optional host sink metadata, emitted through the normal bot error path.
#[derive(Clone, Debug)]
pub struct RuntimeDiagnostic {
    pub filename: Arc<str>,
    pub span: Span,
    pub error: RuntimeError,
}
impl std::fmt::Display for RuntimeDiagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}:{}:{}: {}",
            self.filename.escape_debug(),
            self.span.line,
            self.span.column,
            self.error
        )
    }
}
/// Successful execution counters; replies count adapter acceptance, not delivery.
#[derive(Clone, Copy, Debug, Default)]
pub struct Execution {
    pub steps: usize,
    pub replies: usize,
    pub stopped: bool,
}
struct Budget<'a> {
    ctx: &'a Context,
    limits: &'a Limits,
    steps: usize,
    deadline: Instant,
}
impl Budget<'_> {
    fn tick(&mut self) -> Result<(), RuntimeError> {
        if self.ctx.is_cancelled() {
            return Err(RuntimeError::Cancelled);
        }
        if Instant::now() >= self.deadline {
            return Err(RuntimeError::Deadline);
        }
        if self.steps >= self.limits.steps {
            return Err(RuntimeError::Steps);
        }
        self.steps += 1;
        Ok(())
    }
}
#[derive(PartialEq)]
enum ResultValue<'a> {
    String(Cow<'a, str>),
    Integer(i64),
    Boolean(bool),
}
impl ResultValue<'_> {
    fn boolean(self) -> Result<bool, RuntimeError> {
        if let Self::Boolean(b) = self {
            Ok(b)
        } else {
            Err(RuntimeError::Invalid)
        }
    }
    fn owned(self, limits: &Limits) -> Result<Value, RuntimeError> {
        Ok(match self {
            Self::String(s) => {
                if s.len() > limits.string_bytes {
                    return Err(RuntimeError::StringLimit);
                }
                Value::String(s.as_ref().into())
            }
            Self::Integer(n) => Value::Integer(n),
            Self::Boolean(b) => Value::Boolean(b),
        })
    }
    fn append(&self, out: &mut String, limit: usize) -> Result<(), RuntimeError> {
        let len = match self {
            Self::String(s) => s.len(),
            Self::Integer(n) => {
                if *n == 0 {
                    1
                } else {
                    n.unsigned_abs().ilog10() as usize + 1 + usize::from(*n < 0)
                }
            }
            Self::Boolean(b) => {
                if *b {
                    4
                } else {
                    5
                }
            }
        };
        if out.len().saturating_add(len) > limit {
            return Err(RuntimeError::StringLimit);
        }
        match self {
            Self::String(s) => out.push_str(s),
            Self::Integer(n) => {
                write!(out, "{n}").map_err(|_| RuntimeError::Invalid)?;
            }
            Self::Boolean(b) => out.push_str(if *b { "true" } else { "false" }),
        }
        Ok(())
    }
}
fn from_value(v: &Value) -> ResultValue<'_> {
    match v {
        Value::String(s) => ResultValue::String(Cow::Borrowed(s)),
        Value::Integer(n) => ResultValue::Integer(*n),
        Value::Boolean(b) => ResultValue::Boolean(*b),
    }
}
fn eval<'a>(
    e: &'a Expression,
    ctx: &'a Context,
    locals: &'a [Option<Value>],
    budget: &mut Budget<'_>,
) -> Result<ResultValue<'a>, RuntimeError> {
    budget.tick()?;
    let value = match e {
        Expression::Literal(v) => from_value(v),
        Expression::Message => {
            ResultValue::String(Cow::Borrowed(ctx.content().ok_or(RuntimeError::Context)?))
        }
        Expression::Sender => {
            ResultValue::Integer(ctx.sender().ok_or(RuntimeError::Context)?.get())
        }
        Expression::Room => ResultValue::Integer(ctx.room().ok_or(RuntimeError::Context)?.get()),
        Expression::Argument(index) => ResultValue::String(Cow::Borrowed(
            ctx.arguments().nth(*index).ok_or(RuntimeError::Argument)?,
        )),
        Expression::Local(slot) => from_value(
            locals
                .get(*slot)
                .and_then(Option::as_ref)
                .ok_or(RuntimeError::Invalid)?,
        ),
        Expression::Template(parts) => {
            let mut out = String::new();
            for part in parts {
                eval(part, ctx, locals, budget)?.append(
                    &mut out,
                    budget
                        .limits
                        .interpolation_bytes
                        .min(budget.limits.string_bytes),
                )?;
            }
            ResultValue::String(Cow::Owned(out))
        }
        Expression::Equal(a, b, neg) => {
            let a = eval(a, ctx, locals, budget)?;
            let b = eval(b, ctx, locals, budget)?;
            ResultValue::Boolean((a == b) != *neg)
        }
        Expression::And(a, b) => {
            let a = eval(a, ctx, locals, budget)?.boolean()?;
            ResultValue::Boolean(a && eval(b, ctx, locals, budget)?.boolean()?)
        }
        Expression::Or(a, b) => {
            let a = eval(a, ctx, locals, budget)?.boolean()?;
            ResultValue::Boolean(a || eval(b, ctx, locals, budget)?.boolean()?)
        }
    };
    if let ResultValue::String(s) = &value {
        if s.len() > budget.limits.string_bytes {
            return Err(RuntimeError::StringLimit);
        }
    }
    Ok(value)
}
impl CompiledScripts {
    /// Execute one compiled handler through an existing context. Trusted hosts can
    /// invoke this directly; permissions and argument bounds are still enforced.
    /// Normal scripts use install() and the existing bot's dispatch/error system.
    pub async fn execute(&self, index: usize, ctx: &Context) -> Result<Execution, ExecutionError> {
        let Some(handler) = self.0.handlers.get(index) else {
            return Err(ExecutionError {
                error: RuntimeError::Invalid,
                span: None,
            });
        };
        let limits = &self.0.limits;
        let deadline = Instant::now() + limits.handler_time;
        let work = async {
            let fail = |error| ExecutionError {
                error,
                span: Some(handler.span),
            };
            if let Some(command) = &handler.command {
                if ctx.arguments().count() < command.required {
                    return Err(fail(RuntimeError::Argument));
                }
                if command.permissions.iter().any(|p| !ctx.has_permission(p)) {
                    return Err(fail(RuntimeError::Permission));
                }
            }
            let mut budget = Budget {
                ctx,
                limits,
                steps: 0,
                deadline,
            };
            let mut locals = vec![None; handler.locals];
            let mut pc = 0;
            let mut yielded_at = 0;
            let mut execution = Execution::default();
            while let Some(instruction) = handler.code.get(pc) {
                let failure = |error| ExecutionError {
                    error,
                    span: Some(instruction.span),
                };
                budget.tick().map_err(failure)?;
                // Check at instruction boundaries so branch/jump paths cannot
                // skip cooperative yields merely by avoiding exact multiples.
                if budget.steps - yielded_at >= 64 {
                    tokio::task::yield_now().await;
                    yielded_at = budget.steps;
                }
                match &instruction.operation {
                    Operation::Reply(e) => {
                        let value = eval(e, ctx, &locals, &mut budget).map_err(failure)?;
                        let ResultValue::String(text) = value else {
                            return Err(failure(RuntimeError::Invalid));
                        };
                        ctx.reply(&text)
                            .await
                            .map_err(|e| failure(RuntimeError::Outbound(e)))?;
                        execution.replies += 1;
                    }
                    Operation::Set(slot, e) => {
                        let value = eval(e, ctx, &locals, &mut budget)
                            .and_then(|v| v.owned(limits))
                            .map_err(failure)?;
                        *locals
                            .get_mut(*slot)
                            .ok_or_else(|| failure(RuntimeError::Invalid))? = Some(value);
                    }
                    Operation::FalseJump(e, target) => {
                        if !eval(e, ctx, &locals, &mut budget)
                            .and_then(ResultValue::boolean)
                            .map_err(failure)?
                        {
                            pc = *target;
                            continue;
                        }
                    }
                    Operation::Jump(target) => {
                        pc = *target;
                        continue;
                    }
                    Operation::Stop => {
                        execution.stopped = true;
                        break;
                    }
                }
                pc += 1;
            }
            execution.steps = budget.steps;
            Ok(execution)
        };
        tokio::select! {biased;
            _=ctx.cancelled()=>Err(ExecutionError{error:RuntimeError::Cancelled,span:Some(handler.span)}),
            result=tokio::time::timeout_at(deadline,work)=>result.unwrap_or(Err(ExecutionError{error:RuntimeError::Deadline,span:Some(handler.span)})),
        }
    }
}
