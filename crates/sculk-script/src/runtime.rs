use crate::addon::{AddonContext, AddonError, CallGuard, Capability, Entry, Implementation, Meter};
use crate::{
    ast::Value,
    compiler::{Expression, Operation},
    CompiledScripts, Limits, Span,
};
use sculk_bot::{Context, OutboundError};
use std::{fmt::Write, sync::Arc};
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
    #[error(transparent)]
    Addon(AddonError),
}
impl From<AddonError> for RuntimeError {
    fn from(error: AddonError) -> Self {
        match error {
            AddonError::Steps => Self::Steps,
            AddonError::Deadline => Self::Deadline,
            AddonError::Cancelled => Self::Cancelled,
            AddonError::StringLimit => Self::StringLimit,
            AddonError::Context => Self::Context,
            AddonError::Outbound(e) => Self::Outbound(e),
            other => Self::Addon(other),
        }
    }
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
    shared: Option<Arc<Meter>>,
    capabilities: &'a [Capability],
}
impl Budget<'_> {
    fn used(&self) -> usize {
        self.shared.as_ref().map_or(self.steps, |m| m.used())
    }
    fn tick(&mut self) -> Result<(), RuntimeError> {
        self.charge(1)
    }
    fn charge(&mut self, cost: usize) -> Result<(), RuntimeError> {
        if self.ctx.is_cancelled() {
            return Err(RuntimeError::Cancelled);
        }
        if Instant::now() >= self.deadline {
            return Err(RuntimeError::Deadline);
        }
        if let Some(meter) = &self.shared {
            return meter.charge(cost).map_err(RuntimeError::from);
        }
        self.steps = self
            .steps
            .checked_add(cost)
            .filter(|used| *used <= self.limits.steps)
            .ok_or(RuntimeError::Steps)?;
        Ok(())
    }
    fn context(
        &mut self,
        entry: &Entry,
        args: &[Value],
    ) -> Result<(AddonContext, CallGuard), RuntimeError> {
        if entry
            .syntax
            .capabilities
            .iter()
            .any(|c| !self.capabilities.contains(c))
        {
            return Err(AddonError::Capability.into());
        }
        if args.len() != entry.syntax.parameters.len()
            || args
                .iter()
                .zip(&entry.syntax.parameters)
                .any(|(a, p)| a.ty() != p.ty)
        {
            return Err(AddonError::Type.into());
        }
        self.charge(entry.syntax.cost)?;
        let meter = self
            .shared
            .get_or_insert_with(|| {
                Arc::new(Meter::new(self.steps, self.limits.steps, self.deadline))
            })
            .clone();
        Ok(AddonContext::new(
            self.ctx.clone(),
            meter,
            entry.capabilities.clone(),
            self.limits.string_bytes,
        ))
    }
    fn call(&mut self, entry: &Entry, args: &[Value]) -> Result<Value, RuntimeError> {
        let (ctx, guard) = self.context(entry, args)?;
        let result = match &entry.implementation {
            Implementation::Sync(f) => f(ctx, args),
            _ => return Err(RuntimeError::Invalid),
        };
        drop(guard);
        self.charge(0)?;
        let value = result.map_err(RuntimeError::from)?;
        if Some(value.ty()) != entry.syntax.returns {
            return Err(AddonError::Type.into());
        }
        if value
            .as_string()
            .is_some_and(|s| s.len() > self.limits.string_bytes)
        {
            return Err(RuntimeError::StringLimit);
        }
        Ok(value)
    }
}
#[derive(Debug)]
enum Text<'a> {
    Borrowed(&'a str),
    Owned(String),
    Shared(Arc<str>),
}
impl std::ops::Deref for Text<'_> {
    type Target = str;
    fn deref(&self) -> &str {
        match self {
            Self::Borrowed(s) => s,
            Self::Owned(s) => s,
            Self::Shared(s) => s,
        }
    }
}
impl PartialEq for Text<'_> {
    fn eq(&self, other: &Self) -> bool {
        **self == **other
    }
}
#[derive(PartialEq)]
enum ResultValue<'a> {
    String(Text<'a>),
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
                Value::String(match s {
                    Text::Shared(s) => s,
                    other => (&*other).into(),
                })
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
        Value::String(s) => ResultValue::String(Text::Borrowed(s)),
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
        Expression::Addon(entry, arguments) => {
            let values = evaluate_arguments(arguments, ctx, locals, budget)?;
            match budget.call(entry, &values)? {
                Value::String(s) => ResultValue::String(Text::Shared(s)),
                Value::Integer(n) => ResultValue::Integer(n),
                Value::Boolean(b) => ResultValue::Boolean(b),
            }
        }
        Expression::Literal(v) => from_value(v),
        Expression::Message => {
            ResultValue::String(Text::Borrowed(ctx.content().ok_or(RuntimeError::Context)?))
        }
        Expression::Sender => {
            ResultValue::Integer(ctx.sender().ok_or(RuntimeError::Context)?.get())
        }
        Expression::Room => ResultValue::Integer(ctx.room().ok_or(RuntimeError::Context)?.get()),
        Expression::Argument(index) => ResultValue::String(Text::Borrowed(
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
            ResultValue::String(Text::Owned(out))
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
fn evaluate_arguments(
    args: &[Expression],
    ctx: &Context,
    locals: &[Option<Value>],
    budget: &mut Budget<'_>,
) -> Result<Vec<Value>, RuntimeError> {
    args.iter()
        .map(|arg| eval(arg, ctx, locals, budget)?.owned(budget.limits))
        .collect()
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
                shared: None,
                capabilities: &self.0.capabilities,
            };
            if let Some(event) = &handler.event_addon {
                let accepted = budget
                    .call(event, &[])
                    .map_err(fail)?
                    .as_boolean()
                    .ok_or_else(|| fail(AddonError::Type.into()))?;
                if !accepted {
                    return Ok(Execution {
                        steps: budget.used(),
                        replies: 0,
                        stopped: true,
                    });
                }
            }
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
                if budget.used() - yielded_at >= 64 {
                    tokio::task::yield_now().await;
                    yielded_at = budget.used();
                }
                match &instruction.operation {
                    Operation::Addon(entry, args) => {
                        let args =
                            evaluate_arguments(args, ctx, &locals, &mut budget).map_err(failure)?;
                        let (addon_ctx, guard) = budget.context(entry, &args).map_err(failure)?;
                        let result = match &entry.implementation {
                            Implementation::Async(f) => f(addon_ctx, args).await,
                            _ => return Err(failure(RuntimeError::Invalid)),
                        };
                        drop(guard);
                        budget.charge(0).map_err(failure)?;
                        result.map_err(|e| failure(e.into()))?;
                    }
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
            execution.steps = budget.used();
            execution.replies += budget.shared.as_ref().map_or(0, |m| m.replies());
            Ok(execution)
        };
        tokio::select! {biased;
            _=ctx.cancelled()=>Err(ExecutionError{error:RuntimeError::Cancelled,span:Some(handler.span)}),
            result=tokio::time::timeout_at(deadline,work)=>result.unwrap_or(Err(ExecutionError{error:RuntimeError::Deadline,span:Some(handler.span)})),
        }
    }
}
