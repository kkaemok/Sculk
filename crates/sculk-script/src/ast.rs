//! Typed syntax nodes, discarded after semantic compilation.
use crate::Span;
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Type {
    String,
    Integer,
    Boolean,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    String(std::sync::Arc<str>),
    Integer(i64),
    Boolean(bool),
}
impl Value {
    pub fn ty(&self) -> Type {
        match self {
            Self::String(_) => Type::String,
            Self::Integer(_) => Type::Integer,
            Self::Boolean(_) => Type::Boolean,
        }
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
}
#[derive(Debug)]
pub enum Declaration {
    Event {
        kind: sculk_bot::EventKind,
        body: Vec<Statement>,
        span: Span,
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
