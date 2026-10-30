//! Statically linked, trusted Rust syntax. Capabilities constrain Sculk's API;
//! they cannot sandbox native Rust, its captured state, syscalls or spawned work.
//!
//! ```
//! use sculk_script::{addon::{Registry, Syntax}, Compiler, Source, Type};
//! let mut registry = Registry::new();
//! registry.expression(
//!     Syntax::new("sample value %id%")
//!         .parameter("id", Type::Integer).returns(Type::Integer).cost(2),
//!     |_, args| Ok(args[0].clone()),
//! ).unwrap();
//! let compiler = Compiler::default().with_registry(registry);
//! let program = compiler.compile(&[Source {
//!     filename: "sample.sc",
//!     text: "on message:\n    reply \"%sample value sender%\"\n",
//! }]).expect("typed script");
//! assert_eq!(program.handlers().len(), 1);
//! ```
mod context;
use crate::{
    lexer::{Kind, Token},
    Type, Value,
};
pub use context::{AddonContext, AddonError};
pub(crate) use context::{CallGuard, Meter};
use sculk_bot::{BoxFuture, EventKind, PermissionId};
use std::{collections::HashSet, future::Future, ops::Range, sync::Arc};

/// A facility or explicit host-defined application grant. None are granted by
/// default. Application grants are separate from per-sender bot permissions.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Capability {
    ReadMessage,
    ReadIdentity,
    Reply,
    Application(PermissionId),
}
/// Pattern parameter in declaration order. Values are immutable primitive types.
#[derive(Clone, Debug)]
pub struct Parameter {
    pub name: String,
    pub ty: Type,
}
/// Startup syntax metadata. Pattern grammar: literal words and `%named%` atomic
/// expression slots, with a leading literal; no regex/optional/alternative syntax.
pub struct Syntax {
    pub(crate) pattern: String,
    pub(crate) parameters: Vec<Parameter>,
    pub(crate) returns: Option<Type>,
    pub(crate) cost: usize,
    pub(crate) capabilities: Vec<Capability>,
}
impl Syntax {
    pub fn new(pattern: impl Into<String>) -> Self {
        Self {
            pattern: pattern.into(),
            parameters: Vec::new(),
            returns: None,
            cost: 0,
            capabilities: Vec::new(),
        }
    }
    pub fn parameter(mut self, name: impl Into<String>, ty: Type) -> Self {
        self.parameters.push(Parameter {
            name: name.into(),
            ty,
        });
        self
    }
    pub fn returns(mut self, ty: Type) -> Self {
        self.returns = Some(ty);
        self
    }
    /// Positive steps charged on every actual invocation, in addition to argument
    /// evaluation and facility calls. Real step/time/string limits still apply.
    pub fn cost(mut self, steps: usize) -> Self {
        self.cost = steps;
        self
    }
    pub fn requires(mut self, capability: Capability) -> Self {
        self.capabilities.push(capability);
        self
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyntaxKind {
    Expression,
    Condition,
    Effect,
    Event(EventKind),
}
/// Immutable descriptor, never an invocation API that could bypass capabilities.
pub struct Descriptor<'a> {
    pub pattern: &'a str,
    pub parameters: &'a [Parameter],
    pub returns: Option<Type>,
    pub cost: usize,
    pub capabilities: &'a [Capability],
    pub kind: SyntaxKind,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SyntaxId(pub(crate) usize);
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RegistrationError {
    #[error("invalid addon pattern; use literal words and named atomic slots")]
    Pattern,
    #[error("addon signature does not match the pattern or syntax kind")]
    Signature,
    #[error("duplicate addon syntax")]
    Duplicate,
    #[error("ambiguous or prefix-overlapping addon syntax")]
    Ambiguous,
    #[error("addon syntax conflicts with built-in grammar")]
    Reserved,
    #[error("addon execution cost must be 1..100000 steps")]
    Cost,
    #[error("addon registry or metadata limit exceeded")]
    Limit,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Part {
    Word(String),
    Slot(usize),
}
type SyncImplementation =
    Arc<dyn Fn(AddonContext, &[Value]) -> Result<Value, AddonError> + Send + Sync>;
type AsyncImplementation = Arc<
    dyn Fn(AddonContext, Vec<Value>) -> BoxFuture<'static, Result<(), AddonError>> + Send + Sync,
>;
pub(crate) enum Implementation {
    Sync(SyncImplementation),
    Async(AsyncImplementation),
}
pub(crate) struct Entry {
    pub syntax: Syntax,
    pub kind: SyntaxKind,
    pub parts: Vec<Part>,
    pub implementation: Implementation,
    pub capabilities: Arc<[Capability]>,
}
impl Entry {
    pub fn descriptor(&self) -> Descriptor<'_> {
        Descriptor {
            pattern: &self.syntax.pattern,
            parameters: &self.syntax.parameters,
            returns: self.syntax.returns,
            cost: self.syntax.cost,
            capabilities: &self.syntax.capabilities,
            kind: self.kind,
        }
    }
}
/// Bounded mutable startup registry (64 entries, 16 parts, 8 parameters and
/// capabilities per entry). Moving it into Compiler freezes its snapshot.
#[derive(Default, Clone)]
pub struct Registry {
    pub(crate) entries: Vec<Arc<Entry>>,
}
impl Registry {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub fn descriptor(&self, id: SyntaxId) -> Option<Descriptor<'_>> {
        self.entries.get(id.0).map(|e| e.descriptor())
    }
    pub fn expression<F>(
        &mut self,
        syntax: Syntax,
        implementation: F,
    ) -> Result<SyntaxId, RegistrationError>
    where
        F: Fn(AddonContext, &[Value]) -> Result<Value, AddonError> + Send + Sync + 'static,
    {
        if syntax.returns.is_none() {
            return Err(RegistrationError::Signature);
        }
        self.insert(
            syntax,
            SyntaxKind::Expression,
            Implementation::Sync(Arc::new(implementation)),
        )
    }
    pub fn condition<F>(
        &mut self,
        mut syntax: Syntax,
        implementation: F,
    ) -> Result<SyntaxId, RegistrationError>
    where
        F: Fn(AddonContext, &[Value]) -> Result<bool, AddonError> + Send + Sync + 'static,
    {
        if syntax.returns.is_some_and(|t| t != Type::Boolean) {
            return Err(RegistrationError::Signature);
        }
        syntax.returns = Some(Type::Boolean);
        self.insert(
            syntax,
            SyntaxKind::Condition,
            Implementation::Sync(Arc::new(move |ctx, args| {
                implementation(ctx, args).map(Value::Boolean)
            })),
        )
    }
    pub fn effect<F, Fut>(
        &mut self,
        syntax: Syntax,
        implementation: F,
    ) -> Result<SyntaxId, RegistrationError>
    where
        F: Fn(AddonContext, Vec<Value>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), AddonError>> + Send + 'static,
    {
        if syntax.returns.is_some() {
            return Err(RegistrationError::Signature);
        }
        self.insert(
            syntax,
            SyntaxKind::Effect,
            Implementation::Async(Arc::new(move |ctx, args| {
                Box::pin(implementation(ctx, args))
            })),
        )
    }
    /// A local alias/filter over an existing verified bot event kind. Event
    /// patterns have no slots; callbacks return whether to execute the script body.
    pub fn event<F>(
        &mut self,
        mut syntax: Syntax,
        kind: EventKind,
        implementation: F,
    ) -> Result<SyntaxId, RegistrationError>
    where
        F: Fn(AddonContext) -> Result<bool, AddonError> + Send + Sync + 'static,
    {
        if kind == EventKind::All
            || !syntax.parameters.is_empty()
            || syntax.returns.is_some_and(|t| t != Type::Boolean)
        {
            return Err(RegistrationError::Signature);
        }
        syntax.returns = Some(Type::Boolean);
        self.insert(
            syntax,
            SyntaxKind::Event(kind),
            Implementation::Sync(Arc::new(move |ctx, _| {
                implementation(ctx).map(Value::Boolean)
            })),
        )
    }
    fn insert(
        &mut self,
        syntax: Syntax,
        kind: SyntaxKind,
        implementation: Implementation,
    ) -> Result<SyntaxId, RegistrationError> {
        if self.entries.len() == 64
            || syntax.pattern.len() > 256
            || syntax.parameters.len() > 8
            || syntax.capabilities.len() > 8
        {
            return Err(RegistrationError::Limit);
        }
        if !(1..=100000).contains(&syntax.cost) {
            return Err(RegistrationError::Cost);
        }
        let mut capabilities = HashSet::new();
        if syntax.capabilities.iter().any(|c| !capabilities.insert(c)) {
            return Err(RegistrationError::Signature);
        }
        let mut parts = Vec::new();
        let mut slots = 0;
        let mut names = HashSet::new();
        for word in syntax.pattern.split_whitespace() {
            if word.starts_with('%') && word.ends_with('%') && word.len() > 2 {
                let name = &word[1..word.len() - 1];
                if !valid_word(name)
                    || !names.insert(name)
                    || syntax.parameters.get(slots).is_none_or(|p| p.name != name)
                {
                    return Err(RegistrationError::Signature);
                }
                parts.push(Part::Slot(slots));
                slots += 1;
            } else {
                if !valid_word(word) {
                    return Err(RegistrationError::Pattern);
                }
                if matches!(word, "and" | "or" | "is" | "not") {
                    return Err(RegistrationError::Reserved);
                }
                parts.push(Part::Word(word.into()));
            }
        }
        if parts.is_empty() || parts.len() > 16 || slots != syntax.parameters.len() {
            return Err(RegistrationError::Signature);
        }
        let Some(Part::Word(head)) = parts.first() else {
            return Err(RegistrationError::Pattern);
        };
        if reserved(head) {
            return Err(RegistrationError::Reserved);
        }
        if matches!(kind, SyntaxKind::Event(_)) && slots != 0 {
            return Err(RegistrationError::Signature);
        }
        for existing in &self.entries {
            if existing.parts == parts {
                return Err(RegistrationError::Duplicate);
            }
            if existing
                .parts
                .iter()
                .zip(&parts)
                .all(|(a, b)| match (a, b) {
                    (Part::Word(a), Part::Word(b)) => a == b,
                    _ => true,
                })
            {
                return Err(RegistrationError::Ambiguous);
            }
        }
        let capabilities = syntax.capabilities.clone().into();
        let id = SyntaxId(self.entries.len());
        self.entries.push(Arc::new(Entry {
            syntax,
            kind,
            parts,
            implementation,
            capabilities,
        }));
        Ok(id)
    }
    pub(crate) fn entry(&self, id: SyntaxId) -> &Arc<Entry> {
        &self.entries[id.0]
    }
    /// Compilation only. Each slot is one simple atom or balanced parenthesized
    /// expression. No implementation executes and no token data is kept at runtime.
    pub(crate) fn resolve(&self, tokens: &[Token], group: Group) -> Option<Matched> {
        for (index, entry) in self.entries.iter().enumerate() {
            if !group.accepts(entry.kind) {
                continue;
            }
            let mut at = 0;
            let mut arguments = Vec::new();
            let mut ok = true;
            for part in &entry.parts {
                match part {
                    Part::Word(w) => {
                        if !tokens
                            .get(at)
                            .is_some_and(|t| matches!(&t.kind,Kind::Word(s) if s==w))
                        {
                            ok = false;
                            break;
                        }
                        at += 1;
                    }
                    Part::Slot(_) => {
                        let start = at;
                        let Some(end) = atom_end(tokens, at) else {
                            ok = false;
                            break;
                        };
                        at = end;
                        arguments.push(start..end);
                    }
                }
            }
            if ok {
                return Some(Matched {
                    id: SyntaxId(index),
                    consumed: at,
                    arguments,
                });
            }
        }
        None
    }
    pub(crate) fn has_head(&self, token: &Token, group: Group) -> bool {
        self.entries.iter().any(|e| {
            group.accepts(e.kind)
                && matches!((&e.parts[0],&token.kind),(Part::Word(a),Kind::Word(b)) if a==b)
        })
    }
}
#[derive(Clone, Copy)]
pub(crate) enum Group {
    Expression,
    Effect,
    Event,
}
impl Group {
    fn accepts(self, kind: SyntaxKind) -> bool {
        matches!(
            (self, kind),
            (
                Self::Expression,
                SyntaxKind::Expression | SyntaxKind::Condition
            ) | (Self::Effect, SyntaxKind::Effect)
                | (Self::Event, SyntaxKind::Event(_))
        )
    }
}
pub(crate) struct Matched {
    pub id: SyntaxId,
    pub consumed: usize,
    pub arguments: Vec<Range<usize>>,
}
fn valid_word(word: &str) -> bool {
    !word.is_empty()
        && word.len() <= 64
        && !word.starts_with(|c: char| c.is_ascii_digit())
        && !(word.starts_with('-') && word[1..].starts_with(|c: char| c.is_ascii_digit()))
        && word
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
}
fn reserved(word: &str) -> bool {
    matches!(
        word,
        "reply"
            | "send"
            | "set"
            | "stop"
            | "if"
            | "else"
            | "on"
            | "command"
            | "options"
            | "message"
            | "sender"
            | "sender-id"
            | "room"
            | "room-id"
            | "event-room"
            | "true"
            | "false"
            | "connection"
            | "synchronization"
            | "unknown"
            | "lag"
    ) || word.starts_with("arg-")
}
fn atom_end(tokens: &[Token], start: usize) -> Option<usize> {
    match tokens.get(start)?.kind {
        Kind::Open => {
            let mut depth = 0;
            for (index, token) in tokens.iter().enumerate().skip(start) {
                match token.kind {
                    Kind::Open => {
                        depth += 1;
                        if depth > 64 {
                            return None;
                        }
                    }
                    Kind::Close => {
                        depth -= 1;
                        if depth == 0 {
                            return Some(index + 1);
                        }
                    }
                    _ => {}
                }
            }
            None
        }
        Kind::Word(_) | Kind::Integer(_) | Kind::String(_) | Kind::Local(_) => Some(start + 1),
        _ => None,
    }
}
