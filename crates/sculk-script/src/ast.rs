//! Typed syntax nodes, discarded after semantic compilation.
use crate::{Collection, Scalar, Span};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Type {
    String,
    Integer,
    Boolean,
    List(Scalar),
    Map(Scalar),
}
impl Type {
    pub(crate) fn named(name: &str) -> Option<Self> {
        Some(match name {
            "text" | "string" => Self::String,
            "integer" => Self::Integer,
            "boolean" => Self::Boolean,
            "text-list" => Self::List(Scalar::Text),
            "integer-list" => Self::List(Scalar::Integer),
            "boolean-list" => Self::List(Scalar::Boolean),
            "text-map" => Self::Map(Scalar::Text),
            "integer-map" => Self::Map(Scalar::Integer),
            "boolean-map" => Self::Map(Scalar::Boolean),
            _ => return None,
        })
    }
}
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Value {
    String(std::sync::Arc<str>),
    Integer(i64),
    Boolean(bool),
    Collection(Collection),
}
impl Value {
    pub fn as_integer(&self) -> Option<i64> {
        if let Self::Integer(n) = self {
            Some(*n)
        } else {
            None
        }
    }
    pub fn as_string(&self) -> Option<&str> {
        if let Self::String(s) = self {
            Some(s)
        } else {
            None
        }
    }
    pub fn as_boolean(&self) -> Option<bool> {
        if let Self::Boolean(b) = self {
            Some(*b)
        } else {
            None
        }
    }
    pub fn ty(&self) -> Type {
        match self {
            Self::String(_) => Type::String,
            Self::Integer(_) => Type::Integer,
            Self::Boolean(_) => Type::Boolean,
            Self::Collection(collection) => collection.ty(),
        }
    }
}
impl std::fmt::Debug for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Value").field(&self.ty()).finish()
    }
}

/// Validated source command argument interpretation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArgumentKind {
    Text,
    RemainingText,
    Integer,
    Boolean,
    Duration,
    Room,
    Member,
    Message,
}

impl ArgumentKind {
    pub(crate) fn value_type(self) -> Type {
        match self {
            Self::Text | Self::RemainingText => Type::String,
            Self::Boolean => Type::Boolean,
            _ => Type::Integer,
        }
    }
}

#[derive(Debug)]
pub struct ArgumentSpec {
    pub name: String,
    pub kind: ArgumentKind,
    pub(crate) default: Option<Value>,
}

impl ArgumentSpec {
    pub fn optional(&self) -> bool {
        self.default.is_some()
    }
}
#[derive(Debug)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}
#[derive(Clone, Copy, Debug)]
pub enum Binary {
    Add,
    Subtract,
    Multiply,
    Divide,
    Remainder,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
    Contains,
    StartsWith,
    EndsWith,
}

#[derive(Debug)]
pub enum ExprKind {
    Literal(Value),
    Name(String),
    Local(String),
    Global(String),
    State(String, Type),
    Template(Vec<Expr>),
    Equal(Box<Expr>, Box<Expr>, bool),
    Binary(Binary, Box<Expr>, Box<Expr>),
    Not(Box<Expr>),
    Call(String, Vec<Expr>),
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    Addon(crate::addon::SyntaxId, Vec<Expr>),
}
#[derive(Debug)]
pub struct Statement {
    pub kind: StatementKind,
    pub span: Span,
}
#[derive(Debug)]
pub enum StatementKind {
    Reply(Expr, ReplyKind),
    Set(String, Expr),
    UpdateGlobal(String, Expr, bool),
    DeleteGlobal(String),
    SetState(String, Type, Expr),
    DeleteState(String, Type),
    Wait(Expr),
    Stop,
    Break,
    Continue,
    Return(Option<Expr>),
    Call(Expr),
    Repeat(Expr, Vec<Statement>),
    While(Expr, Vec<Statement>),
    If(Expr, Vec<Statement>, Vec<Statement>),
    Addon(crate::addon::SyntaxId, Vec<Expr>),
}

#[derive(Clone, Copy, Debug)]
pub enum ReplyKind {
    Room,
    Quote,
}
#[derive(Debug)]
pub enum Declaration {
    Function {
        name: String,
        parameters: Vec<(String, Type)>,
        result: Option<Type>,
        body: Vec<Statement>,
        span: Span,
    },
    Event {
        kind: sculk_bot::EventKind,
        body: Vec<Statement>,
        span: Span,
        addon: Option<crate::addon::SyntaxId>,
    },
    Command {
        name: String,
        explicit: Option<String>,
        arguments: Vec<ArgumentSpec>,
        prefixes: Option<Vec<String>>,
        aliases: Vec<String>,
        permissions: Vec<String>,
        cooldown_ms: Option<u64>,
        body: Vec<Statement>,
        span: Span,
    },
}
#[derive(Default, Debug)]
pub struct Script {
    pub prefixes: Option<Vec<String>>,
    pub declarations: Vec<Declaration>,
}
