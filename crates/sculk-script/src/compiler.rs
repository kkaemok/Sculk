use crate::{
    ast::{self, Declaration, ExprKind, StatementKind, Type, Value},
    lexer, parser, Category, Diagnostic, Diagnostics, Limits, Source, Span,
};
use sculk_bot::{Bot, Command, EventKind, PermissionId, RegistrationError};
use std::{
    collections::{HashMap, HashSet},
    io::Read,
    path::Path,
    sync::Arc,
};

#[derive(Clone)]
pub(crate) enum Expression {
    Literal(Value),
    Message,
    Sender,
    Room,
    Argument(usize),
    Local(usize),
    Template(Vec<Expression>),
    Equal(Box<Expression>, Box<Expression>, bool),
    And(Box<Expression>, Box<Expression>),
    Or(Box<Expression>, Box<Expression>),
}
pub(crate) enum Operation {
    Reply(Expression),
    Set(usize, Expression),
    FalseJump(Expression, usize),
    Jump(usize),
    Stop,
}
pub(crate) struct Instruction {
    pub operation: Operation,
    pub span: Span,
}
pub(crate) struct CommandSpec {
    pub name: String,
    pub aliases: Vec<String>,
    pub prefixes: Vec<String>,
    pub permissions: Vec<PermissionId>,
    pub required: usize,
}
pub(crate) struct Handler {
    pub filename: Arc<str>,
    pub span: Span,
    pub kind: EventKind,
    pub command: Option<CommandSpec>,
    pub code: Vec<Instruction>,
    pub locals: usize,
}
pub(crate) struct Program {
    pub handlers: Vec<Handler>,
    pub limits: Limits,
    pub case_sensitive: bool,
    pub units: usize,
}
/// Lightweight metadata without script literals or executable internals.
pub struct HandlerInfo<'a> {
    pub filename: &'a str,
    pub span: Span,
    pub event: EventKind,
    pub command: Option<&'a str>,
}
/// Immutable compiled bundle. Cloning shares instructions, never reparses source.
#[derive(Clone)]
pub struct CompiledScripts(pub(crate) Arc<Program>);
impl std::fmt::Debug for CompiledScripts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompiledScripts")
            .field("handlers", &self.0.handlers.len())
            .field("instructions_and_expressions", &self.0.units)
            .finish()
    }
}
impl CompiledScripts {
    pub fn instruction_count(&self) -> usize {
        self.0.units
    }
    pub fn handlers(&self) -> impl ExactSizeIterator<Item = HandlerInfo<'_>> {
        self.0.handlers.iter().map(|h| HandlerInfo {
            filename: &h.filename,
            span: h.span,
            event: h.kind,
            command: h.command.as_ref().map(|c| c.name.as_str()),
        })
    }
    /// Consumes the startup bot: if registration fails, no partially installed bot
    /// can be run. Host permissions must already be registered. No I/O occurs.
    pub fn install(&self, bot: Bot) -> Result<Bot, RegistrationError> {
        self.install_with_errors(bot, Arc::new(|_| {}))
    }
    /// Optional synchronous, nonblocking diagnostic sink. Default logs no source.
    pub fn install_with_errors(
        &self,
        mut bot: Bot,
        sink: Arc<dyn Fn(crate::RuntimeDiagnostic) + Send + Sync>,
    ) -> Result<Bot, RegistrationError> {
        if bot.case_sensitive() != self.0.case_sensitive {
            return Err(RegistrationError::Invalid);
        }
        if self.0.limits.allow_prefixless {
            bot.allow_prefixless_commands(true)?;
        }
        for (index, handler) in self.0.handlers.iter().enumerate() {
            let program = self.clone();
            let sink = sink.clone();
            let callback = move |ctx: sculk_bot::Context| {
                let program = program.clone();
                let sink = sink.clone();
                async move {
                    match program.execute(index, &ctx).await {
                        Ok(_) => Ok(()),
                        Err(error) => {
                            let handler = &program.0.handlers[index];
                            sink(crate::RuntimeDiagnostic {
                                filename: handler.filename.clone(),
                                span: error.span.unwrap_or(handler.span),
                                error: error.error,
                            });
                            Err(match error.error {
                                crate::RuntimeError::Outbound(e) => {
                                    sculk_bot::HandlerError::Outbound(e)
                                }
                                _ => sculk_bot::HandlerError::Failed,
                            })
                        }
                    }
                }
            };
            if let Some(spec) = &handler.command {
                let mut command = Command::new(&spec.name).prefixes(spec.prefixes.clone());
                for alias in &spec.aliases {
                    command = command.alias(alias);
                }
                for permission in &spec.permissions {
                    command = command.require(permission.clone());
                }
                bot.register(command, callback)?;
            } else {
                bot.on(handler.kind, callback)?;
            }
        }
        Ok(bot)
    }
}
/// Startup-only compiler/loader. Both extensions use identical code paths.
pub struct Compiler {
    limits: Limits,
    case_sensitive: bool,
    defaults: Vec<String>,
}
impl Default for Compiler {
    fn default() -> Self {
        Self::new(Limits::default())
    }
}
impl Compiler {
    pub fn new(limits: Limits) -> Self {
        Self {
            limits,
            case_sensitive: true,
            defaults: vec!["!".into()],
        }
    }
    /// Must match the consuming bot; Unicode lowercase, without normalization.
    pub fn case_sensitive(mut self, value: bool) -> Self {
        self.case_sensitive = value;
        self
    }
    /// Framework fallback when a file has no options/prefix declaration.
    pub fn default_prefixes(mut self, prefixes: Vec<String>) -> Self {
        self.defaults = prefixes;
        self
    }
    /// Explicit host filesystem loading, bounded before UTF-8 decoding. Scripts
    /// themselves have no filesystem operations. Run at startup/off runtime threads.
    pub fn load_files(
        &self,
        paths: &[impl AsRef<Path>],
    ) -> Result<CompiledScripts, Vec<Diagnostic>> {
        if !self.limits.valid() || paths.len() > self.limits.scripts {
            return Err(vec![diagnostic(
                "<loader>",
                Category::Limit,
                "invalid limits or too many script files",
            )]);
        }
        let mut names = Vec::new();
        let mut texts = Vec::new();
        let mut errors = Vec::new();
        for path in paths {
            let name = path.as_ref().to_string_lossy().into_owned();
            if name.len() > 4096 || !crate::supported(&name) {
                errors.push(diagnostic(
                    "<loader>",
                    Category::Loading,
                    "script extension must be .sculk or .sc",
                ));
                continue;
            }
            let read = std::fs::File::open(path).and_then(|file| {
                let mut bytes = Vec::new();
                file.take(self.limits.file_bytes as u64 + 1)
                    .read_to_end(&mut bytes)?;
                Ok(bytes)
            });
            match read {
                Ok(bytes) if bytes.len() > self.limits.file_bytes => errors.push(diagnostic(
                    &name,
                    Category::Limit,
                    "script file size limit exceeded",
                )),
                Ok(bytes) => match String::from_utf8(bytes) {
                    Ok(text) => {
                        names.push(name);
                        texts.push(text);
                    }
                    Err(_) => errors.push(diagnostic(
                        &name,
                        Category::Loading,
                        "script file is not UTF-8",
                    )),
                },
                Err(_) => errors.push(diagnostic(
                    &name,
                    Category::Loading,
                    "script file could not be read",
                )),
            }
            if errors.len() >= self.limits.diagnostics {
                break;
            }
        }
        if !errors.is_empty() {
            return Err(errors);
        }
        self.compile(
            &names
                .iter()
                .zip(&texts)
                .map(|(filename, text)| Source { filename, text })
                .collect::<Vec<_>>(),
        )
    }
    pub fn compile(&self, sources: &[Source<'_>]) -> Result<CompiledScripts, Vec<Diagnostic>> {
        if !self.limits.valid() || sources.is_empty() || sources.len() > self.limits.scripts {
            return Err(vec![diagnostic(
                "<compiler>",
                Category::Limit,
                "invalid limits or script count",
            )]);
        }
        let mut handlers = Vec::new();
        let mut errors = Vec::new();
        let mut units = 0;
        let mut conflicts = HashSet::new();
        let mut files = HashSet::new();
        let mut prefix_set = HashSet::from(["!".to_owned()]);
        for source in sources {
            if errors.len() >= self.limits.diagnostics {
                break;
            }
            let mut d = Diagnostics {
                file: source.filename,
                limits: &self.limits,
                errors: Vec::new(),
            };
            let span = Span {
                line: 1,
                column: 1,
                ..Span::default()
            };
            if source.filename.len() > 4096 {
                errors.push(diagnostic(
                    "<compiler>",
                    Category::Loading,
                    "filename is too long",
                ));
                continue;
            }
            if !crate::supported(source.filename) {
                d.push(
                    span,
                    Category::Loading,
                    "script extension must be .sculk or .sc",
                );
            }
            if !files.insert(source.filename) {
                d.push(span, Category::Loading, "duplicate source filename");
            }
            if source.text.len() > self.limits.file_bytes {
                d.push(span, Category::Limit, "script file size limit exceeded");
            }
            if !d.errors.is_empty() {
                errors.extend(d.errors);
                continue;
            }
            let lines = lexer::lex(source.text, &mut d);
            let script = parser::parse(&lines, &mut d);
            let defaults = script.prefixes.as_ref().unwrap_or(&self.defaults);
            validate_prefixes(defaults, span, &mut d);
            for declaration in script.declarations {
                let (kind, body, span, spec) = match declaration {
                    Declaration::Event { kind, body, span } => (kind, body, span, None),
                    Declaration::Command {
                        name,
                        explicit,
                        arguments,
                        prefixes,
                        aliases,
                        permissions,
                        body,
                        span,
                    } => {
                        let actual = if let Some(explicit) = explicit {
                            if prefixes
                                .as_ref()
                                .is_some_and(|p| p != &vec![explicit.clone()])
                            {
                                d.push(
                                    span,
                                    Category::Command,
                                    "explicit command prefix conflicts with prefixes property",
                                );
                            }
                            vec![explicit]
                        } else {
                            prefixes.unwrap_or_else(|| defaults.clone())
                        };
                        validate_prefixes(&actual, span, &mut d);
                        if !sculk_bot::valid_command_name(&name)
                            || aliases.iter().any(|a| !sculk_bot::valid_command_name(a))
                        {
                            d.push(span, Category::Command, "invalid command name or alias");
                        }
                        if aliases.len() > 16 || arguments.len() > 64 || permissions.len() > 128 {
                            d.push(span, Category::Limit, "command metadata limit exceeded");
                        }
                        let mut args = HashSet::new();
                        for arg in &arguments {
                            if !sculk_bot::valid_command_name(arg) || !args.insert(arg) {
                                d.push(
                                    span,
                                    Category::Command,
                                    "invalid or duplicate argument name",
                                );
                            }
                        }
                        for prefix in &actual {
                            prefix_set.insert(prefix.clone());
                            for name in std::iter::once(&name).chain(&aliases) {
                                let normalized = if self.case_sensitive {
                                    name.clone()
                                } else {
                                    name.to_lowercase()
                                };
                                if !conflicts.insert((prefix.clone(), normalized)) {
                                    d.push(
                                        span,
                                        Category::Command,
                                        "duplicate command or ambiguous alias for this prefix",
                                    );
                                }
                            }
                        }
                        let mut ids = Vec::new();
                        for p in permissions {
                            match PermissionId::new(&p) {
                                Ok(id) => ids.push(id),
                                Err(_) => {
                                    d.push(span, Category::Command, "invalid permission identifier")
                                }
                            }
                        }
                        (
                            EventKind::Message,
                            body,
                            span,
                            Some(CommandSpec {
                                name,
                                aliases,
                                prefixes: actual,
                                permissions: ids,
                                required: arguments.len(),
                            }),
                        )
                    }
                };
                let mut semantic = Semantic {
                    d: &mut d,
                    bindings: HashMap::new(),
                    locals: 0,
                    code: Vec::new(),
                    kind,
                    arguments: spec.as_ref().map(|s| s.required),
                    units: &mut units,
                };
                semantic.charge(span);
                semantic.block(&body, 1);
                handlers.push(Handler {
                    filename: source.filename.into(),
                    span,
                    kind,
                    command: spec,
                    code: semantic.code,
                    locals: semantic.locals,
                });
            }
            errors.extend(d.errors);
        }
        if handlers.iter().filter(|h| h.command.is_some()).count() > 128
            || handlers.iter().filter(|h| h.command.is_none()).count() > 128
            || prefix_set.len() > 128
        {
            errors.push(diagnostic(
                "<compiler>",
                Category::Limit,
                "framework command, event or prefix registry limit exceeded",
            ));
        }
        errors.truncate(self.limits.diagnostics);
        if errors.is_empty() {
            Ok(CompiledScripts(Arc::new(Program {
                handlers,
                limits: self.limits.clone(),
                case_sensitive: self.case_sensitive,
                units,
            })))
        } else {
            Err(errors)
        }
    }
}
fn diagnostic(file: &str, category: Category, explanation: &'static str) -> Diagnostic {
    Diagnostic {
        filename: file.into(),
        span: Span {
            line: 1,
            column: 1,
            ..Span::default()
        },
        category,
        explanation,
    }
}
fn validate_prefixes(prefixes: &[String], span: Span, d: &mut Diagnostics<'_>) {
    if prefixes.is_empty() || prefixes.len() > 16 {
        d.push(
            span,
            Category::Limit,
            "prefix list must contain 1..16 strings",
        );
    }
    let mut unique = HashSet::new();
    for p in prefixes {
        if !sculk_bot::valid_prefix(p, d.limits.allow_prefixless) {
            d.push(
                span,
                Category::Command,
                "invalid prefix; empty prefixes require explicit host opt-in",
            );
        }
        if !unique.insert(p) {
            d.push(span, Category::Command, "duplicate prefix in list");
        }
    }
}
struct Semantic<'a, 'b> {
    d: &'a mut Diagnostics<'b>,
    bindings: HashMap<String, (usize, Type)>,
    locals: usize,
    code: Vec<Instruction>,
    kind: EventKind,
    arguments: Option<usize>,
    units: &'a mut usize,
}
impl Semantic<'_, '_> {
    fn charge(&mut self, span: Span) -> bool {
        *self.units += 1;
        if *self.units > self.d.limits.instructions {
            self.d.push(
                span,
                Category::Limit,
                "compiled instruction/expression budget exceeded",
            );
            false
        } else {
            true
        }
    }
    fn emit(&mut self, span: Span, operation: Operation) {
        if self.charge(span) {
            self.code.push(Instruction { span, operation });
        }
    }
    fn block(&mut self, body: &[ast::Statement], depth: usize) {
        if depth > self.d.limits.depth {
            return;
        }
        for statement in body {
            let span = statement.span;
            if *self.units > self.d.limits.instructions {
                break;
            }
            match &statement.kind {
                StatementKind::Reply(e) => {
                    if !matches!(self.kind, EventKind::Message | EventKind::Synchronization) {
                        self.d.push(
                            span,
                            Category::Type,
                            "messaging effect requires a current room",
                        );
                    }
                    if let Some((e, Type::String)) = self.expr(e, 1) {
                        self.emit(span, Operation::Reply(e));
                    } else {
                        self.d.push(
                            span,
                            Category::Type,
                            "reply/send requires a string expression",
                        );
                    }
                }
                StatementKind::Set(name, e) => {
                    if let Some((e, ty)) = self.expr(e, 1) {
                        let slot = if let Some((slot, old)) = self.bindings.get(name) {
                            if old != &ty {
                                self.d.push(
                                    span,
                                    Category::Type,
                                    "local variable type cannot change",
                                );
                                continue;
                            }
                            *slot
                        } else {
                            if self.locals >= self.d.limits.locals {
                                self.d
                                    .push(span, Category::Limit, "local variable limit exceeded");
                                continue;
                            }
                            let slot = self.locals;
                            self.locals += 1;
                            self.bindings.insert(name.clone(), (slot, ty));
                            slot
                        };
                        self.emit(span, Operation::Set(slot, e));
                    }
                }
                StatementKind::Stop => self.emit(span, Operation::Stop),
                StatementKind::If(condition, yes, no) => {
                    let Some((condition, Type::Boolean)) = self.expr(condition, 1) else {
                        self.d
                            .push(span, Category::Type, "if condition must be boolean");
                        continue;
                    };
                    let original = self.bindings.clone();
                    let branch = self.code.len();
                    self.emit(span, Operation::FalseJump(condition, 0));
                    self.block(yes, depth + 1);
                    let jump = self.code.len();
                    self.emit(span, Operation::Jump(0));
                    let alternate = self.code.len();
                    self.bindings = original.clone();
                    self.block(no, depth + 1);
                    self.bindings = original;
                    let end = self.code.len();
                    if let Some(Instruction {
                        operation: Operation::FalseJump(_, target),
                        ..
                    }) = self.code.get_mut(branch)
                    {
                        *target = alternate;
                    }
                    if let Some(Instruction {
                        operation: Operation::Jump(target),
                        ..
                    }) = self.code.get_mut(jump)
                    {
                        *target = end;
                    }
                }
            }
        }
    }
    fn expr(&mut self, expr: &ast::Expr, depth: usize) -> Option<(Expression, Type)> {
        let span = expr.span;
        if depth > self.d.limits.depth {
            self.d
                .push(span, Category::Limit, "compiled expression depth exceeded");
            return None;
        }
        if !self.charge(span) {
            return None;
        }
        Some(match &expr.kind {
            ExprKind::Literal(value) => {
                if let Value::String(s) = value {
                    if s.len() > self.d.limits.string_bytes {
                        self.d
                            .push(span, Category::Limit, "string literal limit exceeded");
                        return None;
                    }
                }
                (Expression::Literal(value.clone()), value.ty())
            }
            ExprKind::Local(name) => {
                let Some((slot, ty)) = self.bindings.get(name) else {
                    self.d.push(
                        span,
                        Category::Scope,
                        "local variable is not defined in this scope",
                    );
                    return None;
                };
                (Expression::Local(*slot), ty.clone())
            }
            ExprKind::Name(name) => match name.as_str() {
                "message" if self.kind == EventKind::Message => (Expression::Message, Type::String),
                "sender" | "sender-id" if self.kind == EventKind::Message => {
                    (Expression::Sender, Type::Integer)
                }
                "room" | "room-id" | "event-room"
                    if matches!(self.kind, EventKind::Message | EventKind::Synchronization) =>
                {
                    (Expression::Room, Type::Integer)
                }
                _ => {
                    if let Some(arg) = name
                        .strip_prefix("arg-")
                        .and_then(|n| n.parse::<usize>().ok())
                    {
                        if arg == 0 || self.arguments.is_none_or(|count| arg > count) {
                            self.d.push(
                                span,
                                Category::Scope,
                                "argument must be declared in this command header",
                            );
                            return None;
                        }
                        (Expression::Argument(arg - 1), Type::String)
                    } else {
                        self.d.push(
                            span,
                            Category::Type,
                            "unknown expression or expression unavailable for this event",
                        );
                        return None;
                    }
                }
            },
            ExprKind::Template(parts) => {
                let mut compiled = Vec::new();
                for p in parts {
                    compiled.push(self.expr(p, depth + 1)?.0);
                }
                (Expression::Template(compiled), Type::String)
            }
            ExprKind::Equal(a, b, neg) => {
                let (a, ta) = self.expr(a, depth + 1)?;
                let (b, tb) = self.expr(b, depth + 1)?;
                if ta != tb {
                    self.d.push(
                        span,
                        Category::Type,
                        "comparison operands must have the same type",
                    );
                    return None;
                }
                (
                    Expression::Equal(Box::new(a), Box::new(b), *neg),
                    Type::Boolean,
                )
            }
            ExprKind::And(a, b) | ExprKind::Or(a, b) => {
                let (a, ta) = self.expr(a, depth + 1)?;
                let (b, tb) = self.expr(b, depth + 1)?;
                if ta != Type::Boolean || tb != Type::Boolean {
                    self.d
                        .push(span, Category::Type, "and/or operands must be boolean");
                    return None;
                }
                let e = if matches!(expr.kind, ExprKind::And(..)) {
                    Expression::And(Box::new(a), Box::new(b))
                } else {
                    Expression::Or(Box::new(a), Box::new(b))
                };
                (e, Type::Boolean)
            }
        })
    }
}
