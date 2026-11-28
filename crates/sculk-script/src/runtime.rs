use crate::addon::{AddonContext, AddonError, CallGuard, Capability, Entry, Implementation, Meter};
use crate::{
    ast::{ArgumentKind, ArgumentSpec, Binary, ReplyKind, Value},
    compiler::{Expression, Function, Operation},
    Collection, CompiledScripts, GlobalCounters, Limits, Span,
};
use sculk_bot::{Context, OutboundError};
use std::{borrow::Cow, fmt::Write, sync::Arc, time::Duration};
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
    #[error("script integer arithmetic overflow or division by zero")]
    Arithmetic,
    #[error("script function call depth exceeded")]
    CallDepth,
    #[error("script repeat count outside 0..=10000")]
    RepeatCount,
    #[error("script counter storage unavailable or full")]
    State,
    #[error("typed state variable has no stored value")]
    StateUnset,
    #[error("script wait duration outside 0..=30000 milliseconds")]
    WaitDuration,
    #[error("collection index or key absent")]
    CollectionIndex,
    #[error("collection size or key length exceeded")]
    CollectionLimit,
    #[error("script string/output limit exceeded")]
    StringLimit,
    #[error("script expression requires unavailable event context")]
    Context,
    #[error("required command argument is missing")]
    Argument,
    #[error("invalid value for command argument {0}")]
    ArgumentValue(usize),
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

fn bind_arguments(
    specs: &[ArgumentSpec],
    ctx: &Context,
    limits: &Limits,
) -> Result<Vec<Value>, RuntimeError> {
    let mut available = ctx.arguments();
    let mut values = Vec::with_capacity(specs.len());
    for (index, spec) in specs.iter().enumerate() {
        let Some(first) = available.next() else {
            values.push(spec.default.clone().ok_or(RuntimeError::Argument)?);
            continue;
        };
        let parsed = match spec.kind {
            ArgumentKind::Text => Some(Value::String(first.into())),
            ArgumentKind::RemainingText => {
                let mut text = first.to_owned();
                for part in available.by_ref() {
                    text.push(' ');
                    text.push_str(part);
                }
                Some(Value::String(text.into()))
            }
            ArgumentKind::Integer => first.parse().ok().map(Value::Integer),
            ArgumentKind::Boolean => first.parse().ok().map(Value::Boolean),
            ArgumentKind::Duration => duration_ms(first).map(Value::Integer),
            ArgumentKind::Room => scoped_argument(first, ctx.room().map(|room| room.get())),
            ArgumentKind::Member => scoped_argument(first, ctx.sender().map(|sender| sender.get())),
            ArgumentKind::Message => {
                scoped_argument(first, ctx.message().map(|message| message.id.get()))
            }
        }
        .ok_or(RuntimeError::ArgumentValue(index + 1))?;
        if parsed
            .as_string()
            .is_some_and(|value| value.len() > limits.string_bytes)
        {
            return Err(RuntimeError::StringLimit);
        }
        values.push(parsed);
    }
    Ok(values)
}

fn scoped_argument(text: &str, allowed: Option<i64>) -> Option<Value> {
    let exact = text.parse::<i64>().ok()?;
    (Some(exact) == allowed).then_some(Value::Integer(exact))
}

fn duration_ms(text: &str) -> Option<i64> {
    for (unit, multiplier) in [("ms", 1), ("s", 1000), ("m", 60_000), ("h", 3_600_000)] {
        if let Some(value) = text.strip_suffix(unit) {
            let number = value.parse::<i64>().ok()?.checked_mul(multiplier)?;
            return (number >= 0).then_some(number);
        }
    }
    None
}

fn counter_key<'a>(name: &'a str, ctx: &Context) -> Result<Cow<'a, str>, RuntimeError> {
    match name.strip_prefix("room::") {
        Some(name) => {
            let room = ctx.room().ok_or(RuntimeError::Context)?;
            Ok(Cow::Owned(format!("room::{}::{name}", room.get())))
        }
        None => Ok(Cow::Borrowed(name)),
    }
}

struct Budget<'a> {
    ctx: &'a Context,
    limits: &'a Limits,
    steps: usize,
    deadline: Instant,
    shared: Option<Arc<Meter>>,
    capabilities: &'a [Capability],
    functions: &'a [Function],
    globals: &'a GlobalCounters,
    call_depth: usize,
    arguments: Vec<Value>,
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
    Collection(Collection),
}
impl ResultValue<'_> {
    fn integer(self) -> Result<i64, RuntimeError> {
        if let Self::Integer(value) = self {
            Ok(value)
        } else {
            Err(RuntimeError::Invalid)
        }
    }
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
            Self::Collection(value) => Value::Collection(value),
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
            Self::Collection(_) => return Err(RuntimeError::Invalid),
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
            Self::Collection(_) => return Err(RuntimeError::Invalid),
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
        Value::Collection(value) => ResultValue::Collection(value.clone()),
    }
}
fn from_owned(value: Value) -> ResultValue<'static> {
    match value {
        Value::String(s) => ResultValue::String(Text::Shared(s)),
        Value::Integer(n) => ResultValue::Integer(n),
        Value::Boolean(b) => ResultValue::Boolean(b),
        Value::Collection(value) => ResultValue::Collection(value),
    }
}

fn binary<'a>(
    operator: Binary,
    left: ResultValue<'a>,
    right: ResultValue<'a>,
) -> Result<ResultValue<'a>, RuntimeError> {
    if let (ResultValue::String(left), ResultValue::String(right)) = (&left, &right) {
        return Ok(ResultValue::Boolean(match operator {
            Binary::Contains => left.contains(&**right),
            Binary::StartsWith => left.starts_with(&**right),
            Binary::EndsWith => left.ends_with(&**right),
            _ => return Err(RuntimeError::Invalid),
        }));
    }
    let a = left.integer()?;
    let b = right.integer()?;
    let result = match operator {
        Binary::Add => a.checked_add(b),
        Binary::Subtract => a.checked_sub(b),
        Binary::Multiply => a.checked_mul(b),
        Binary::Divide => a.checked_div(b),
        Binary::Remainder => a.checked_rem(b),
        Binary::Less => return Ok(ResultValue::Boolean(a < b)),
        Binary::LessEqual => return Ok(ResultValue::Boolean(a <= b)),
        Binary::Greater => return Ok(ResultValue::Boolean(a > b)),
        Binary::GreaterEqual => return Ok(ResultValue::Boolean(a >= b)),
        _ => return Err(RuntimeError::Invalid),
    };
    result
        .map(ResultValue::Integer)
        .ok_or(RuntimeError::Arithmetic)
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
            from_owned(budget.call(entry, &values)?)
        }
        Expression::Literal(v) => from_value(v),
        Expression::Message => {
            ResultValue::String(Text::Borrowed(ctx.content().ok_or(RuntimeError::Context)?))
        }
        Expression::Sender => {
            ResultValue::Integer(ctx.sender().ok_or(RuntimeError::Context)?.get())
        }
        Expression::Global(name) => ResultValue::Integer(
            budget
                .globals
                .get(&counter_key(name, ctx)?)
                .map_err(|_| RuntimeError::State)?,
        ),
        Expression::CounterExists(name) => ResultValue::Boolean(
            budget
                .globals
                .contains(&counter_key(name, ctx)?)
                .map_err(|_| RuntimeError::State)?,
        ),
        Expression::State(name, ty) => from_owned(
            budget
                .globals
                .get_state(&counter_key(name, ctx)?, *ty)
                .map_err(|_| RuntimeError::State)?
                .ok_or(RuntimeError::StateUnset)?,
        ),
        Expression::StateExists(name, ty) => ResultValue::Boolean(
            budget
                .globals
                .get_state(&counter_key(name, ctx)?, *ty)
                .map_err(|_| RuntimeError::State)?
                .is_some(),
        ),
        Expression::StateOr(name, ty, fallback) => {
            if let Some(value) = budget
                .globals
                .get_state(&counter_key(name, ctx)?, *ty)
                .map_err(|_| RuntimeError::State)?
            {
                from_owned(value)
            } else {
                eval(fallback, ctx, locals, budget)?
            }
        }
        Expression::MessageId => {
            ResultValue::Integer(ctx.message().ok_or(RuntimeError::Context)?.id.get())
        }
        Expression::MessageType => ResultValue::Integer(
            ctx.message()
                .ok_or(RuntimeError::Context)?
                .kind
                .wire_type()
                .into(),
        ),
        Expression::SentAt => {
            ResultValue::Integer(ctx.message().ok_or(RuntimeError::Context)?.sent_at_seconds)
        }
        Expression::Room => ResultValue::Integer(ctx.room().ok_or(RuntimeError::Context)?.get()),
        Expression::Argument(index) => from_owned(
            budget
                .arguments
                .get(*index)
                .cloned()
                .ok_or(RuntimeError::Argument)?,
        ),
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
            if let ResultValue::Collection(value) = &a {
                budget.charge(value.len().div_ceil(8))?;
            }
            ResultValue::Boolean((a == b) != *neg)
        }
        Expression::Binary(operator, left, right) => {
            let left = eval(left, ctx, locals, budget)?;
            let right = eval(right, ctx, locals, budget)?;
            binary(*operator, left, right)?
        }
        Expression::Not(value) => {
            ResultValue::Boolean(!eval(value, ctx, locals, budget)?.boolean()?)
        }
        Expression::Builtin(builtin, args) => {
            let values = evaluate_arguments(args, ctx, locals, budget)?;
            if let Some(Value::Collection(value)) = values.first() {
                budget.charge(value.len().div_ceil(8))?;
            }
            from_owned(builtin.run(values)?)
        }
        Expression::Call(index, args) => {
            let values = evaluate_arguments(args, ctx, locals, budget)?;
            from_owned(call_function(*index, values, budget)?.ok_or(RuntimeError::Invalid)?)
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
struct Frame {
    locals: Vec<Option<Value>>,
    repeats: Vec<u16>,
    pc: usize,
}
enum Action<'a> {
    Advance,
    Stop,
    Return(Option<Value>),
    Reply(&'a Expression, ReplyKind),
    Wait(Duration),
    Addon(&'a Entry, &'a [Expression]),
}
impl Frame {
    fn new(locals: usize, repeats: usize) -> Self {
        Self {
            locals: vec![None; locals],
            repeats: vec![0; repeats],
            pc: 0,
        }
    }

    fn step<'a>(
        &mut self,
        operation: &'a Operation,
        budget: &mut Budget<'_>,
    ) -> Result<Action<'a>, RuntimeError> {
        self.pc += 1;
        let ctx = budget.ctx;
        let limits = budget.limits;
        match operation {
            Operation::Reply(expr, kind) => return Ok(Action::Reply(expr, *kind)),
            Operation::Addon(entry, args) => return Ok(Action::Addon(entry, args)),
            Operation::Stop => return Ok(Action::Stop),
            Operation::Return(expr) => {
                let result = expr
                    .as_ref()
                    .map(|expr| eval(expr, ctx, &self.locals, budget)?.owned(limits))
                    .transpose()?;
                return Ok(Action::Return(result));
            }
            Operation::Set(slot, expr) => {
                let value = eval(expr, ctx, &self.locals, budget)?.owned(limits)?;
                *self.locals.get_mut(*slot).ok_or(RuntimeError::Invalid)? = Some(value);
            }
            Operation::UpdateGlobal(name, expr, add) => {
                let value = eval(expr, ctx, &self.locals, budget)?.integer()?;
                budget.globals.update(&counter_key(name, ctx)?, |old| {
                    if *add {
                        old.checked_add(value).ok_or(RuntimeError::Arithmetic)
                    } else {
                        Ok(value)
                    }
                })?;
            }
            Operation::DeleteGlobal(name) => budget
                .globals
                .delete(&counter_key(name, ctx)?)
                .map_err(|_| RuntimeError::State)?,
            Operation::SetState(name, expr) => {
                let value = eval(expr, ctx, &self.locals, budget)?.owned(limits)?;
                budget
                    .globals
                    .set_state(&counter_key(name, ctx)?, value)
                    .map_err(|_| RuntimeError::State)?;
            }
            Operation::DeleteState(name, ty) => {
                budget
                    .globals
                    .delete_state(&counter_key(name, ctx)?, *ty)
                    .map_err(|_| RuntimeError::State)?;
            }
            Operation::Wait(expr) => {
                let milliseconds = eval(expr, ctx, &self.locals, budget)?.integer()?;
                if !(0..=30000).contains(&milliseconds) {
                    return Err(RuntimeError::WaitDuration);
                }
                return Ok(Action::Wait(Duration::from_millis(milliseconds as u64)));
            }
            Operation::FalseJump(expr, target) => {
                if !eval(expr, ctx, &self.locals, budget)?.boolean()? {
                    self.pc = *target;
                }
            }
            Operation::Jump(target) => self.pc = *target,
            Operation::Repeat(expr, slot, target) => {
                let count = eval(expr, ctx, &self.locals, budget)?.integer()?;
                if !(0..=10000).contains(&count) {
                    return Err(RuntimeError::RepeatCount);
                }
                self.repeats[*slot] = count as u16;
                if count == 0 {
                    self.pc = *target;
                }
            }
            Operation::RepeatNext(slot, target) => {
                let remaining = &mut self.repeats[*slot];
                *remaining = remaining.checked_sub(1).ok_or(RuntimeError::Invalid)?;
                if *remaining > 0 {
                    self.pc = *target;
                }
            }
            Operation::Call(index, args) => {
                let values = evaluate_arguments(args, ctx, &self.locals, budget)?;
                call_function(*index, values, budget)?;
            }
        }
        Ok(Action::Advance)
    }
}

fn call_function(
    index: usize,
    args: Vec<Value>,
    budget: &mut Budget<'_>,
) -> Result<Option<Value>, RuntimeError> {
    budget.charge(1)?;
    if budget.call_depth >= 16 {
        return Err(RuntimeError::CallDepth);
    }
    let function = budget.functions.get(index).ok_or(RuntimeError::Invalid)?;
    let mut frame = Frame::new(function.locals, function.repeat_slots);
    for (argument, slot) in args.into_iter().zip(&mut frame.locals) {
        *slot = Some(argument);
    }
    budget.call_depth += 1;
    let result = (|| {
        while let Some(instruction) = function.code.get(frame.pc) {
            budget.tick()?;
            match frame.step(&instruction.operation, budget)? {
                Action::Advance => {}
                Action::Return(value) => return Ok(value),
                _ => return Err(RuntimeError::Invalid),
            }
        }
        Ok(None)
    })();
    budget.call_depth -= 1;
    result
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
            let arguments = if let Some(command) = &handler.command {
                if command.permissions.iter().any(|p| !ctx.has_permission(p)) {
                    return Err(fail(RuntimeError::Permission));
                }
                bind_arguments(&command.arguments, ctx, limits).map_err(fail)?
            } else {
                Vec::new()
            };
            let mut budget = Budget {
                ctx,
                limits,
                steps: 0,
                deadline,
                shared: None,
                capabilities: &self.0.capabilities,
                functions: &self.0.functions,
                globals: &self.0.globals,
                call_depth: 0,
                arguments,
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
            let mut frame = Frame::new(handler.locals, handler.repeat_slots);
            let mut yielded_at = 0;
            let mut execution = Execution::default();
            while let Some(instruction) = handler.code.get(frame.pc) {
                let failure = |error| ExecutionError {
                    error,
                    span: Some(instruction.span),
                };
                budget.tick().map_err(failure)?;
                // Check at instruction boundaries so branch/jump paths cannot
                // skip cooperative yields merely by avoiding exact multiples.
                if budget.used() - yielded_at >= 64 {
                    tokio::task::yield_now().await;
                    budget.charge(0).map_err(failure)?;
                    yielded_at = budget.used();
                }
                match frame
                    .step(&instruction.operation, &mut budget)
                    .map_err(failure)?
                {
                    Action::Advance => {}
                    Action::Wait(duration) => tokio::time::sleep(duration).await,
                    Action::Addon(entry, args) => {
                        let args = evaluate_arguments(args, ctx, &frame.locals, &mut budget)
                            .map_err(failure)?;
                        let (addon_ctx, guard) = budget.context(entry, &args).map_err(failure)?;
                        let result = match &entry.implementation {
                            Implementation::Async(f) => f(addon_ctx, args).await,
                            _ => return Err(failure(RuntimeError::Invalid)),
                        };
                        drop(guard);
                        budget.charge(0).map_err(failure)?;
                        result.map_err(|e| failure(e.into()))?;
                    }
                    Action::Reply(e, kind) => {
                        let value = eval(e, ctx, &frame.locals, &mut budget).map_err(failure)?;
                        let ResultValue::String(text) = value else {
                            return Err(failure(RuntimeError::Invalid));
                        };
                        let reply = match kind {
                            ReplyKind::Room => ctx.reply(&text).await,
                            ReplyKind::Quote => ctx.quote_reply(&text).await,
                        };
                        reply.map_err(|e| failure(RuntimeError::Outbound(e)))?;
                        execution.replies += 1;
                    }
                    Action::Stop => {
                        execution.stopped = true;
                        break;
                    }
                    Action::Return(_) => return Err(failure(RuntimeError::Invalid)),
                }
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
