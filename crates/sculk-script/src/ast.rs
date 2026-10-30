//! Typed syntax nodes, discarded after semantic compilation.
use crate::Span;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Type {
    String,
    Integer,
    Boolean,
}
#[derive(Clone, PartialEq, Eq)]
pub enum Value {
    String(std::sync::Arc<str>),
    Integer(i64),
    Boolean(bool),
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
        }
    }
}
impl std::fmt::Debug for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Value").field(&self.ty()).finish()
    }
}
#[derive(Debug)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}
#[derive(Debug)]
pub enum ExprKind {
    Literal(Value),
    Name(String),
    Local(String),
    Template(Vec<Expr>),
    Equal(Box<Expr>, Box<Expr>, bool),
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
    Reply(Expr),
    Set(String, Expr),
    Stop,
    If(Expr, Vec<Statement>, Vec<Statement>),
    Addon(crate::addon::SyntaxId, Vec<Expr>),
}
#[derive(Debug)]
pub enum Declaration {
    Event {
        kind: sculk_bot::EventKind,
        body: Vec<Statement>,
        span: Span,
        addon: Option<crate::addon::SyntaxId>,
    },
    Command {
        name: String,
        explicit: Option<String>,
        arguments: Vec<String>,
        prefixes: Option<Vec<String>>,
        aliases: Vec<String>,
        permissions: Vec<String>,
        body: Vec<Statement>,
        span: Span,
    },
}
#[derive(Default, Debug)]
pub struct Script {
    pub prefixes: Option<Vec<String>>,
    pub declarations: Vec<Declaration>,
}
