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
#[derive(Clone, PartialEq, Eq)]
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
    Reply(Expr),
    Set(String, Expr),
    UpdateGlobal(String, Expr, bool),
    DeleteGlobal(String),
    Wait(Expr),
    Stop,
    Return(Option<Expr>),
    Call(Expr),
    Repeat(Expr, Vec<Statement>),
    While(Expr, Vec<Statement>),
    If(Expr, Vec<Statement>, Vec<Statement>),
    Addon(crate::addon::SyntaxId, Vec<Expr>),
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
