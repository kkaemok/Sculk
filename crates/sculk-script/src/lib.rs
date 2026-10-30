//! Compile `.sculk` and `.sc` through one bounded language implementation and
//! register its handlers with sculk-bot. Scripts have no file/network/env API.
//!
//! ```
//! use sculk_script::{Compiler, Source};
//! use sculk_bot::{Bot, Config};
//! let program = Compiler::default().compile(&[Source {
//!     filename: "ping.sc",
//!     text: "command ping:\n    trigger:\n        reply \"Pong!\"\n",
//! }]).expect("valid script");
//! let bot = program.install(Bot::new(Config::default()).unwrap()).unwrap();
//! // Supply an explicit Outbound adapter and a bounded source to run it.
//! assert_eq!(program.handlers().len(), 1);
//! ```
pub mod addon;
mod ast;
mod compiler;
mod lexer;
mod parser;
mod runtime;
pub use ast::{Type, Value};
pub use compiler::{CompiledScripts, Compiler, HandlerInfo};
pub use runtime::{Execution, ExecutionError, RuntimeDiagnostic, RuntimeError};
use std::{fmt, path::Path, time::Duration};

/// One UTF-8 source supplied by an explicit host loader, never by script code.
pub struct Source<'a> {
    pub filename: &'a str,
    pub text: &'a str,
}
/// Byte offsets plus one-based Unicode-scalar line/column coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub line: usize,
    pub column: usize,
}
impl Default for Span {
    fn default() -> Self {
        Self {
            start: 0,
            end: 0,
            line: 1,
            column: 1,
        }
    }
}
/// Safe diagnostic category; default formatting never prints source literals.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    Loading,
    Lexical,
    Syntax,
    Type,
    Scope,
    Command,
    Unsupported,
    Limit,
    Addon,
    Capability,
}
/// Bounded compiler diagnostic. Source excerpts require explicit caller action.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub filename: String,
    pub span: Span,
    pub category: Category,
    pub explanation: &'static str,
}
impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}:{}:{}: {:?}: {}",
            self.filename.escape_debug(),
            self.span.line,
            self.span.column,
            self.category,
            self.explanation
        )
    }
}
impl Diagnostic {
    /// Explicit opt-in to showing the implicated source line (up to 256 chars).
    /// Source can contain private literals; do not use this in default logging.
    pub fn with_excerpt(&self, source: &str) -> String {
        let line = source
            .lines()
            .nth(self.span.line.saturating_sub(1))
            .unwrap_or("");
        let excerpt: String = line
            .chars()
            .take(256)
            .flat_map(char::escape_debug)
            .collect();
        format!(
            "{self}\n{} | {excerpt}\n    {}^",
            self.span.line,
            " ".repeat(self.span.column.saturating_sub(1).min(256))
        )
    }
}
/// Local policy with hard ceilings; aggregate instruction count covers a bundle.
#[derive(Clone, Debug)]
pub struct Limits {
    pub file_bytes: usize,
    pub scripts: usize,
    pub depth: usize,
    pub instructions: usize,
    pub steps: usize,
    pub string_bytes: usize,
    pub interpolation_bytes: usize,
    pub locals: usize,
    pub handler_time: Duration,
    pub diagnostics: usize,
    /// Empty command prefixes are refused unless the host explicitly opts in.
    pub allow_prefixless: bool,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            file_bytes: 65536,
            scripts: 16,
            depth: 16,
            instructions: 4096,
            steps: 8192,
            string_bytes: 16384,
            interpolation_bytes: 16384,
            locals: 64,
            handler_time: Duration::from_millis(100),
            diagnostics: 32,
            allow_prefixless: false,
        }
    }
}
impl Limits {
    pub(crate) fn valid(&self) -> bool {
        (1..=1048576).contains(&self.file_bytes)
            && (1..=128).contains(&self.scripts)
            && (1..=64).contains(&self.depth)
            && (1..=16384).contains(&self.instructions)
            && (1..=100000).contains(&self.steps)
            && (1..=65536).contains(&self.string_bytes)
            && (1..=16384).contains(&self.interpolation_bytes)
            && (1..=256).contains(&self.locals)
            && !self.handler_time.is_zero()
            && self.handler_time <= Duration::from_secs(30)
            && (1..=128).contains(&self.diagnostics)
    }
}
pub(crate) fn supported(filename: &str) -> bool {
    matches!(
        Path::new(filename).extension().and_then(|s| s.to_str()),
        Some("sculk" | "sc")
    )
}
pub(crate) struct Diagnostics<'a> {
    pub file: &'a str,
    pub limits: &'a Limits,
    pub errors: Vec<Diagnostic>,
}
impl Diagnostics<'_> {
    pub fn push(&mut self, span: Span, category: Category, explanation: &'static str) {
        if self.errors.len() < self.limits.diagnostics {
            self.errors.push(Diagnostic {
                filename: self.file.into(),
                span,
                category,
                explanation,
            });
        }
    }
}
