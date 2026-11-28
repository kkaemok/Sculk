use crate::addon::{Capability, Entry, Registry, SyntaxId};
use crate::collections::Builtin;
use crate::{
    ast::{
        self, ArgumentKind, ArgumentSpec, Binary, Declaration, ExprKind, StatementKind, Type, Value,
    },
    lexer, parser, Category, Diagnostic, Diagnostics, GlobalCounters, Limits, Source, Span,
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
    Global(Arc<str>),
    CounterExists(Arc<str>),
    State(Arc<str>, Type),
    StateExists(Arc<str>, Type),
    StateOr(Arc<str>, Type, Box<Expression>),
    MessageId,
    MessageType,
    SentAt,
    Template(Vec<Expression>),
    Equal(Box<Expression>, Box<Expression>, bool),
    Binary(Binary, Box<Expression>, Box<Expression>),
    Not(Box<Expression>),
    Call(usize, Vec<Expression>),
    Builtin(Builtin, Vec<Expression>),
    And(Box<Expression>, Box<Expression>),
    Or(Box<Expression>, Box<Expression>),
    Addon(Arc<Entry>, Vec<Expression>),
}
pub(crate) enum Operation {
    Reply(Expression, ast::ReplyKind),
    Set(usize, Expression),
    UpdateGlobal(Arc<str>, Expression, bool),
    DeleteGlobal(Arc<str>),
    SetState(Arc<str>, Expression),
    DeleteState(Arc<str>, Type),
    Wait(Expression),
    FalseJump(Expression, usize),
    Jump(usize),
    Stop,
    Return(Option<Expression>),
    Call(usize, Vec<Expression>),
    Repeat(Expression, usize, usize),
    RepeatNext(usize, usize),
    Addon(Arc<Entry>, Vec<Expression>),
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
    pub arguments: Vec<ArgumentSpec>,
    pub cooldown_ms: Option<u64>,
}
pub(crate) struct Handler {
    pub filename: Arc<str>,
    pub span: Span,
    pub kind: EventKind,
    pub command: Option<CommandSpec>,
    pub code: Vec<Instruction>,
    pub event_addon: Option<Arc<Entry>>,
    pub locals: usize,
    pub repeat_slots: usize,
}
pub(crate) struct Function {
    pub code: Vec<Instruction>,
    pub locals: usize,
    pub repeat_slots: usize,
}
struct Signature {
    index: usize,
    parameters: Vec<Type>,
    result: Option<Type>,
}
pub(crate) struct Program {
    pub handlers: Vec<Handler>,
    pub functions: Vec<Function>,
    pub globals: Arc<GlobalCounters>,
    pub limits: Limits,
    pub case_sensitive: bool,
    pub units: usize,
    pub capabilities: Vec<Capability>,
}
/// Lightweight metadata without script literals or executable internals.
pub struct HandlerInfo<'a> {
    pub filename: &'a str,
    pub span: Span,
    pub event: EventKind,
    pub command: Option<&'a str>,
}
/// Compiled command metadata for help and completion.
pub struct CommandInfo<'a> {
    pub name: &'a str,
    pub prefixes: &'a [String],
    pub aliases: &'a [String],
    pub arguments: &'a [ArgumentSpec],
    pub cooldown_ms: Option<u64>,
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
    pub fn commands(&self) -> impl Iterator<Item = CommandInfo<'_>> {
        self.0.handlers.iter().filter_map(|handler| {
            let command = handler.command.as_ref()?;
            Some(CommandInfo {
                name: &command.name,
                prefixes: &command.prefixes,
                aliases: &command.aliases,
                arguments: &command.arguments,
                cooldown_ms: command.cooldown_ms,
            })
        })
    }

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
                if let Some(ms) = spec.cooldown_ms {
                    command = command.cooldown(std::time::Duration::from_millis(ms));
                }
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
    registry: Arc<Registry>,
    globals: Option<Arc<GlobalCounters>>,
    capabilities: Vec<Capability>,
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
            registry: Arc::new(Registry::new()),
            globals: None,
            capabilities: Vec::new(),
        }
    }
    /// Freeze validated native registrations. Compiled calls retain resolved
    /// implementations directly; no registry lookup occurs during execution.
    pub fn with_registry(mut self, registry: Registry) -> Self {
        self.registry = Arc::new(registry);
        self
    }
    /// Share counters across independently compiled bundles, including during
    /// validated reload. A fresh compilation otherwise has empty counters.
    pub fn with_globals(mut self, counters: Arc<GlobalCounters>) -> Self {
        self.globals = Some(counters);
        self
    }
    /// Explicit host grants, separate from command permissions. Facility access
    /// is also restricted to each registration's own declared requirements.
    pub fn capabilities(mut self, capabilities: impl IntoIterator<Item = Capability>) -> Self {
        self.capabilities = capabilities.into_iter().collect();
        self
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
        if self.capabilities.len() > 64
            || self.capabilities.iter().collect::<HashSet<_>>().len() != self.capabilities.len()
        {
            return Err(vec![diagnostic(
                "<compiler>",
                Category::Capability,
                "capability grants must be unique and bounded to 64",
            )]);
        }
        let mut handlers = Vec::new();
        let mut functions = Vec::new();
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
            let script = parser::parse(&lines, &mut d, &self.registry);
            let defaults = script.prefixes.as_ref().unwrap_or(&self.defaults);
            validate_prefixes(defaults, span, &mut d);
            let mut signatures = HashMap::new();
            let mut index = functions.len();
            for declaration in &script.declarations {
                if let Declaration::Function {
                    name,
                    parameters,
                    result,
                    span,
                    ..
                } = declaration
                {
                    if index >= 128 {
                        d.push(
                            *span,
                            Category::Limit,
                            "function declaration limit exceeded",
                        );
                        break;
                    }
                    if Builtin::named(name).is_some()
                        || !sculk_bot::valid_command_name(name)
                        || signatures
                            .insert(
                                name.clone(),
                                Signature {
                                    index,
                                    parameters: parameters.iter().map(|(_, ty)| *ty).collect(),
                                    result: *result,
                                },
                            )
                            .is_some()
                    {
                        d.push(
                            *span,
                            Category::Scope,
                            "invalid or duplicate file-local function name",
                        );
                    }
                    index += 1;
                }
            }
            for declaration in script.declarations {
                let (kind, body, span, spec, event_addon) = match declaration {
                    Declaration::Function {
                        parameters,
                        result,
                        body,
                        span,
                        ..
                    } => {
                        if functions.len() >= 128 {
                            break;
                        }
                        let mut semantic = Semantic {
                            d: &mut d,
                            bindings: HashMap::new(),
                            locals: parameters.len(),
                            repeat_slots: 0,
                            loops: Vec::new(),
                            code: Vec::new(),
                            kind: EventKind::All,
                            arguments: None,
                            functions: &signatures,
                            return_type: Some(result),
                            units: &mut units,
                            registry: &self.registry,
                            capabilities: &self.capabilities,
                        };
                        for (slot, (name, ty)) in parameters.into_iter().enumerate() {
                            if !sculk_bot::valid_command_name(&name)
                                || semantic.bindings.insert(name, (slot, ty)).is_some()
                            {
                                semantic.d.push(
                                    span,
                                    Category::Scope,
                                    "invalid or duplicate function parameter",
                                );
                            }
                        }
                        if result.is_some() && !guarantees_return(&body) {
                            semantic.d.push(
                                span,
                                Category::Type,
                                "function must return a value on every path",
                            );
                        }
                        semantic.charge(span);
                        semantic.block(&body, 1);
                        functions.push(Function {
                            code: semantic.code,
                            locals: semantic.locals,
                            repeat_slots: semantic.repeat_slots,
                        });
                        continue;
                    }
                    Declaration::Event {
                        kind,
                        body,
                        span,
                        addon,
                    } => (kind, body, span, None, addon),
                    Declaration::Command {
                        name,
                        explicit,
                        arguments,
                        prefixes,
                        aliases,
                        permissions,
                        cooldown_ms,
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
                        let mut optional_seen = false;
                        for (index, arg) in arguments.iter().enumerate() {
                            if !sculk_bot::valid_command_name(&arg.name) || !args.insert(&arg.name)
                            {
                                d.push(
                                    span,
                                    Category::Command,
                                    "invalid or duplicate argument name",
                                );
                            }
                            if optional_seen && !arg.optional() {
                                d.push(
                                    span,
                                    Category::Command,
                                    "required arguments must precede defaults",
                                );
                            }
                            if arg.kind == ArgumentKind::RemainingText
                                && index + 1 != arguments.len()
                            {
                                d.push(
                                    span,
                                    Category::Command,
                                    "remaining-text must be the last argument",
                                );
                            }
                            optional_seen |= arg.optional();
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
                                arguments,
                                cooldown_ms,
                            }),
                            None,
                        )
                    }
                };
                let mut semantic = Semantic {
                    d: &mut d,
                    bindings: HashMap::new(),
                    locals: 0,
                    repeat_slots: 0,
                    loops: Vec::new(),
                    code: Vec::new(),
                    kind,
                    functions: &signatures,
                    return_type: None,
                    arguments: spec.as_ref().map(|s| {
                        s.arguments
                            .iter()
                            .map(|arg| arg.kind.value_type())
                            .collect()
                    }),
                    units: &mut units,
                    registry: &self.registry,
                    capabilities: &self.capabilities,
                };
                let event_addon = event_addon.and_then(|id| semantic.entry(id, span));
                semantic.charge(span);
                semantic.block(&body, 1);
                handlers.push(Handler {
                    filename: source.filename.into(),
                    span,
                    kind,
                    command: spec,
                    code: semantic.code,
                    locals: semantic.locals,
                    repeat_slots: semantic.repeat_slots,
                    event_addon,
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
                functions,
                globals: self.globals.clone().unwrap_or_default(),
                limits: self.limits.clone(),
                case_sensitive: self.case_sensitive,
                units,
                capabilities: self.capabilities.clone(),
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
fn guarantees_return(body: &[ast::Statement]) -> bool {
    body.iter().any(|statement| match &statement.kind {
        StatementKind::Return(_) => true,
        StatementKind::If(_, yes, no) => guarantees_return(yes) && guarantees_return(no),
        _ => false,
    })
}
struct Semantic<'a, 'b> {
    d: &'a mut Diagnostics<'b>,
    bindings: HashMap<String, (usize, Type)>,
    locals: usize,
    repeat_slots: usize,
    loops: Vec<LoopJumps>,
    code: Vec<Instruction>,
    kind: EventKind,
    arguments: Option<Vec<Type>>,
    functions: &'a HashMap<String, Signature>,
    return_type: Option<Option<Type>>,
    units: &'a mut usize,
    registry: &'a Registry,
    capabilities: &'a [Capability],
}

#[derive(Default)]
struct LoopJumps {
    breaks: Vec<usize>,
    continues: Vec<usize>,
}
impl Semantic<'_, '_> {
    fn finish_loop(&mut self, continue_at: usize) {
        let jumps = self
            .loops
            .pop()
            .expect("compiler is closing its current loop");
        let exit = self.code.len();
        for index in jumps.breaks {
            self.patch_jump(index, exit);
        }
        for index in jumps.continues {
            self.patch_jump(index, continue_at);
        }
    }
    fn entry(&mut self, id: SyntaxId, span: Span) -> Option<Arc<Entry>> {
        let entry = self.registry.entry(id).clone();
        if entry
            .syntax
            .capabilities
            .iter()
            .any(|c| !self.capabilities.contains(c))
        {
            self.d.push(
                span,
                Category::Capability,
                "addon requires a capability not granted by the host",
            );
            return None;
        }
        Some(entry)
    }
    fn arguments(
        &mut self,
        entry: &Entry,
        args: &[ast::Expr],
        depth: usize,
    ) -> Option<Vec<Expression>> {
        if args.len() != entry.syntax.parameters.len() {
            return None;
        }
        let mut compiled = Vec::new();
        for (arg, parameter) in args.iter().zip(&entry.syntax.parameters) {
            let (e, ty) = self.expr(arg, depth + 1)?;
            if ty != parameter.ty {
                self.d.push(
                    arg.span,
                    Category::Type,
                    "addon parameter type does not match its registered signature",
                );
                return None;
            }
            compiled.push(e);
        }
        Some(compiled)
    }

    fn function_call(
        &mut self,
        name: &str,
        args: &[ast::Expr],
        span: Span,
        depth: usize,
    ) -> Option<(usize, Vec<Expression>, Option<Type>)> {
        let Some(signature) = self.functions.get(name) else {
            self.d
                .push(span, Category::Scope, "unknown function in this file");
            return None;
        };
        if args.len() != signature.parameters.len() {
            self.d.push(
                span,
                Category::Type,
                "wrong number of arguments for function",
            );
            return None;
        }
        let mut compiled = Vec::with_capacity(args.len());
        for (arg, ty) in args.iter().zip(&signature.parameters) {
            let (expression, actual) = self.expr(arg, depth + 1)?;
            if actual != *ty {
                self.d
                    .push(arg.span, Category::Type, "function argument type mismatch");
                return None;
            }
            compiled.push(expression);
        }
        Some((signature.index, compiled, signature.result))
    }

    fn patch_jump(&mut self, index: usize, destination: usize) {
        if let Some(instruction) = self.code.get_mut(index) {
            match &mut instruction.operation {
                Operation::FalseJump(_, target)
                | Operation::Jump(target)
                | Operation::Repeat(_, _, target) => *target = destination,
                _ => {}
            }
        }
    }

    fn scoped_block(&mut self, body: &[ast::Statement], depth: usize) {
        let original = self.bindings.clone();
        self.block(body, depth + 1);
        self.bindings = original;
    }

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
                StatementKind::Reply(e, kind) => {
                    let available = match kind {
                        ast::ReplyKind::Room => {
                            matches!(
                                self.kind,
                                EventKind::Message
                                    | EventKind::Synchronization
                                    | EventKind::Scheduled
                            )
                        }
                        ast::ReplyKind::Quote => self.kind == EventKind::Message,
                    };
                    if !available {
                        self.d.push(
                            span,
                            Category::Type,
                            "messaging effect requires its current room or message context",
                        );
                    }
                    if let Some((e, Type::String)) = self.expr(e, 1) {
                        self.emit(span, Operation::Reply(e, *kind));
                    } else {
                        self.d.push(
                            span,
                            Category::Type,
                            "reply/send requires a string expression",
                        );
                    }
                }
                StatementKind::UpdateGlobal(name, value, add) => {
                    let Some((value, Type::Integer)) = self.expr(value, 1) else {
                        self.d.push(
                            span,
                            Category::Type,
                            "global counters require integer values",
                        );
                        continue;
                    };
                    self.emit(
                        span,
                        Operation::UpdateGlobal(name.as_str().into(), value, *add),
                    );
                }
                StatementKind::DeleteGlobal(name) => {
                    self.emit(span, Operation::DeleteGlobal(name.as_str().into()))
                }
                StatementKind::SetState(name, ty, value) => {
                    let Some((value, actual)) = self.expr(value, 1) else {
                        continue;
                    };
                    if actual == *ty {
                        self.emit(span, Operation::SetState(name.as_str().into(), value));
                    } else {
                        self.d.push(
                            span,
                            Category::Type,
                            "state assignment must match declared type",
                        );
                    }
                }
                StatementKind::DeleteState(name, ty) => {
                    self.emit(span, Operation::DeleteState(name.as_str().into(), *ty))
                }
                StatementKind::Wait(value) => {
                    if self.return_type.is_some() {
                        self.d
                            .push(span, Category::Type, "wait is only available in a handler");
                        continue;
                    }
                    let Some((value, Type::Integer)) = self.expr(value, 1) else {
                        self.d
                            .push(span, Category::Type, "wait duration must be an integer");
                        continue;
                    };
                    self.emit(span, Operation::Wait(value));
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
                StatementKind::Addon(id, args) => {
                    if self.return_type.is_some() {
                        self.d.push(
                            span,
                            Category::Type,
                            "addon effects are only available in handlers",
                        );
                        continue;
                    }
                    if let Some(entry) = self.entry(*id, span) {
                        if let Some(args) = self.arguments(&entry, args, 1) {
                            self.emit(span, Operation::Addon(entry, args));
                        }
                    }
                }
                StatementKind::Stop if self.return_type.is_some() => {
                    self.d
                        .push(span, Category::Type, "use return inside a function");
                }
                StatementKind::Stop => self.emit(span, Operation::Stop),
                StatementKind::Break | StatementKind::Continue => {
                    let Some(jumps) = self.loops.last_mut() else {
                        self.d.push(
                            span,
                            Category::Scope,
                            "loop control requires an enclosing loop",
                        );
                        continue;
                    };
                    let patch = if matches!(statement.kind, StatementKind::Break) {
                        &mut jumps.breaks
                    } else {
                        &mut jumps.continues
                    };
                    patch.push(self.code.len());
                    self.emit(span, Operation::Jump(0));
                }
                StatementKind::Return(value) => {
                    let Some(expected) = self.return_type else {
                        self.d.push(
                            span,
                            Category::Type,
                            "return is only available in a function",
                        );
                        continue;
                    };
                    let compiled = match value {
                        Some(expr) => match self.expr(expr, 1) {
                            Some((expr, ty)) if Some(ty) == expected => Some(expr),
                            _ => {
                                self.d
                                    .push(span, Category::Type, "function return type mismatch");
                                continue;
                            }
                        },
                        None if expected.is_none() => None,
                        None => {
                            self.d.push(
                                span,
                                Category::Type,
                                "function must return its declared type",
                            );
                            continue;
                        }
                    };
                    self.emit(span, Operation::Return(compiled));
                }
                StatementKind::Call(expr) => {
                    if let ExprKind::Call(name, args) = &expr.kind {
                        match self.function_call(name, args, span, 1) {
                            Some((index, args, None)) => {
                                self.emit(span, Operation::Call(index, args))
                            }
                            Some(_) => {
                                self.d
                                    .push(span, Category::Type, "call requires a void function")
                            }
                            None => {}
                        }
                    } else {
                        self.d
                            .push(span, Category::Type, "call requires a named void function");
                    }
                }
                StatementKind::Repeat(count, body) => {
                    let Some((count, Type::Integer)) = self.expr(count, 1) else {
                        self.d
                            .push(span, Category::Type, "repeat count must be an integer");
                        continue;
                    };
                    let start = self.code.len();
                    let slot = self.repeat_slots;
                    self.repeat_slots += 1;
                    self.emit(span, Operation::Repeat(count, slot, 0));
                    self.loops.push(LoopJumps::default());
                    self.scoped_block(body, depth);
                    let continue_at = self.code.len();
                    self.emit(span, Operation::RepeatNext(slot, start + 1));
                    self.patch_jump(start, self.code.len());
                    self.finish_loop(continue_at);
                }
                StatementKind::While(condition, body) => {
                    let Some((condition, Type::Boolean)) = self.expr(condition, 1) else {
                        self.d
                            .push(span, Category::Type, "while condition must be boolean");
                        continue;
                    };
                    let start = self.code.len();
                    self.emit(span, Operation::FalseJump(condition, 0));
                    self.loops.push(LoopJumps::default());
                    self.scoped_block(body, depth);
                    self.emit(span, Operation::Jump(start));
                    self.patch_jump(start, self.code.len());
                    self.finish_loop(start);
                }
                StatementKind::If(condition, yes, no) => {
                    let Some((condition, Type::Boolean)) = self.expr(condition, 1) else {
                        self.d
                            .push(span, Category::Type, "if condition must be boolean");
                        continue;
                    };
                    let branch = self.code.len();
                    self.emit(span, Operation::FalseJump(condition, 0));
                    self.scoped_block(yes, depth);
                    let jump = self.code.len();
                    self.emit(span, Operation::Jump(0));
                    self.patch_jump(branch, self.code.len());
                    self.scoped_block(no, depth);
                    self.patch_jump(jump, self.code.len());
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
            ExprKind::Addon(id, args) => {
                let entry = self.entry(*id, span)?;
                let args = self.arguments(&entry, args, depth)?;
                let Some(ty) = entry.syntax.returns else {
                    self.d.push(
                        span,
                        Category::Addon,
                        "effect cannot be used as an expression",
                    );
                    return None;
                };
                (Expression::Addon(entry, args), ty)
            }
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
            ExprKind::Global(name) => (Expression::Global(name.as_str().into()), Type::Integer),
            ExprKind::State(name, ty) => (Expression::State(name.as_str().into(), *ty), *ty),
            ExprKind::Local(name) => {
                let Some((slot, ty)) = self.bindings.get(name) else {
                    self.d.push(
                        span,
                        Category::Scope,
                        "local variable is not defined in this scope",
                    );
                    return None;
                };
                (Expression::Local(*slot), *ty)
            }
            ExprKind::Name(name)
                if self.return_type.is_some() && self.bindings.contains_key(name) =>
            {
                let (slot, ty) = self.bindings[name];
                (Expression::Local(slot), ty)
            }
            ExprKind::Name(name) => match name.as_str() {
                "message" if self.kind == EventKind::Message => (Expression::Message, Type::String),
                "message-id" if self.kind == EventKind::Message => {
                    (Expression::MessageId, Type::Integer)
                }
                "message-type" if self.kind == EventKind::Message => {
                    (Expression::MessageType, Type::Integer)
                }
                "sent-at" if self.kind == EventKind::Message => (Expression::SentAt, Type::Integer),
                "sender" | "sender-id" if self.kind == EventKind::Message => {
                    (Expression::Sender, Type::Integer)
                }
                "room" | "room-id" | "event-room"
                    if matches!(
                        self.kind,
                        EventKind::Message | EventKind::Synchronization | EventKind::Scheduled
                    ) =>
                {
                    (Expression::Room, Type::Integer)
                }
                _ => {
                    if let Some(arg) = name
                        .strip_prefix("arg-")
                        .and_then(|n| n.parse::<usize>().ok())
                    {
                        let ty = arg
                            .checked_sub(1)
                            .and_then(|index| self.arguments.as_ref()?.get(index).copied());
                        let Some(ty) = ty else {
                            self.d.push(
                                span,
                                Category::Scope,
                                "argument must be declared in this command header",
                            );
                            return None;
                        };
                        (Expression::Argument(arg - 1), ty)
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
                    let (value, ty) = self.expr(p, depth + 1)?;
                    if matches!(ty, Type::List(_) | Type::Map(_)) {
                        self.d
                            .push(p.span, Category::Type, "interpolation requires a scalar");
                        return None;
                    }
                    compiled.push(value);
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
            ExprKind::Call(name, arguments) => {
                if matches!(name.as_str(), "state-exists" | "state-or") {
                    let [ast::Expr {
                        kind: ExprKind::State(key, ty),
                        ..
                    }, rest @ ..] = arguments.as_slice()
                    else {
                        self.d
                            .push(span, Category::Type, "expected a typed state variable");
                        return None;
                    };
                    match (name.as_str(), rest) {
                        ("state-exists", []) => {
                            return Some((
                                Expression::StateExists(key.as_str().into(), *ty),
                                Type::Boolean,
                            ))
                        }
                        ("state-or", [fallback]) => {
                            let (fallback, actual) = self.expr(fallback, depth + 1)?;
                            if actual == *ty {
                                return Some((
                                    Expression::StateOr(
                                        key.as_str().into(),
                                        *ty,
                                        Box::new(fallback),
                                    ),
                                    *ty,
                                ));
                            }
                        }
                        _ => {}
                    }
                    self.d.push(
                        span,
                        Category::Type,
                        "state default must match the declared type",
                    );
                    return None;
                }
                if name == "counter-exists" {
                    let [ast::Expr {
                        kind: ExprKind::Global(counter),
                        ..
                    }] = arguments.as_slice()
                    else {
                        self.d.push(
                            span,
                            Category::Type,
                            "counter-exists requires a global counter",
                        );
                        return None;
                    };
                    return Some((
                        Expression::CounterExists(counter.as_str().into()),
                        Type::Boolean,
                    ));
                }
                if let Some(builtin) = Builtin::named(name) {
                    let mut compiled = Vec::with_capacity(arguments.len());
                    let mut types = Vec::with_capacity(arguments.len());
                    for arg in arguments {
                        let (expr, ty) = self.expr(arg, depth + 1)?;
                        compiled.push(expr);
                        types.push(ty);
                    }
                    let Some(ty) = builtin.result(&types) else {
                        self.d.push(
                            span,
                            Category::Type,
                            "invalid collection function arguments",
                        );
                        return None;
                    };
                    return Some((Expression::Builtin(builtin, compiled), ty));
                }
                let (index, arguments, result) =
                    self.function_call(name, arguments, span, depth)?;
                let Some(ty) = result else {
                    self.d
                        .push(span, Category::Type, "void function cannot produce a value");
                    return None;
                };
                (Expression::Call(index, arguments), ty)
            }
            ExprKind::Not(value) => {
                let (value, ty) = self.expr(value, depth + 1)?;
                if ty != Type::Boolean {
                    self.d
                        .push(span, Category::Type, "not operand must be boolean");
                    return None;
                }
                (Expression::Not(Box::new(value)), Type::Boolean)
            }
            ExprKind::Binary(operator, left, right) => {
                let (left, left_type) = self.expr(left, depth + 1)?;
                let (right, right_type) = self.expr(right, depth + 1)?;
                let (input, output) = match operator {
                    Binary::Contains | Binary::StartsWith | Binary::EndsWith => {
                        (Type::String, Type::Boolean)
                    }
                    Binary::Less | Binary::LessEqual | Binary::Greater | Binary::GreaterEqual => {
                        (Type::Integer, Type::Boolean)
                    }
                    _ => (Type::Integer, Type::Integer),
                };
                if left_type != input || right_type != input {
                    self.d.push(
                        span,
                        Category::Type,
                        "binary operator operand type mismatch",
                    );
                    return None;
                }
                (
                    Expression::Binary(*operator, Box::new(left), Box::new(right)),
                    output,
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
