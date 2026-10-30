use crate::addon::{Group, Registry};
use crate::{
    ast::*,
    lexer::{self, Kind, Line, Piece, Token},
    Category, Diagnostics, Span,
};
fn word(t: &Token, s: &str) -> bool {
    matches!(&t.kind,Kind::Word(w) if w==s)
}
fn colon(t: &Token) -> bool {
    matches!(t.kind, Kind::Colon)
}
fn words(t: &[Token], names: &[&str]) -> bool {
    t.len() == names.len() && t.iter().zip(names).all(|(t, s)| word(t, s))
}
fn fail<T>(d: &mut Diagnostics<'_>, span: Span, msg: &'static str) -> Option<T> {
    d.push(span, Category::Syntax, msg);
    None
}
fn string(t: &Token, d: &mut Diagnostics<'_>) -> Option<String> {
    if let Kind::String(parts) = &t.kind {
        let mut s = String::new();
        for p in parts {
            if let Piece::Text(text) = p {
                s.push_str(text);
            } else {
                return fail(
                    d,
                    t.span,
                    "configuration strings cannot contain interpolation",
                );
            }
        }
        Some(s)
    } else {
        fail(d, t.span, "expected a quoted string")
    }
}
fn strings(t: &[Token], d: &mut Diagnostics<'_>, fallback: Span) -> Option<Vec<String>> {
    let mut out = Vec::new();
    for (i, token) in t.iter().enumerate() {
        if i % 2 == 0 {
            out.push(string(token, d)?);
        } else if !matches!(token.kind, Kind::Comma) {
            return fail(d, token.span, "expected comma-separated strings");
        }
    }
    if t.is_empty() || t.len().is_multiple_of(2) {
        return fail(
            d,
            t.first().map(|t| t.span).unwrap_or(fallback),
            "expected a nonempty string list",
        );
    }
    Some(out)
}
fn names(t: &[Token], d: &mut Diagnostics<'_>, fallback: Span) -> Option<Vec<String>> {
    let mut out = Vec::new();
    for (i, token) in t.iter().enumerate() {
        if i % 2 == 0 {
            if let Kind::Word(s) = &token.kind {
                out.push(s.clone());
            } else {
                return fail(d, token.span, "expected an alias name");
            }
        } else if !matches!(token.kind, Kind::Comma) {
            return fail(d, token.span, "expected comma-separated aliases");
        }
    }
    if t.is_empty() || t.len().is_multiple_of(2) {
        return fail(
            d,
            t.first().map(|t| t.span).unwrap_or(fallback),
            "expected a nonempty alias list",
        );
    }
    Some(out)
}
pub fn parse(lines: &[Line], d: &mut Diagnostics<'_>, registry: &Registry) -> Script {
    let mut p = Parser {
        lines,
        index: 0,
        d,
        registry,
    };
    let mut script = Script::default();
    while p.index < lines.len() {
        let line = &lines[p.index];
        let t = &line.tokens;
        let span = line.span;
        if line.indent != 0 {
            p.d.push(
                span,
                Category::Syntax,
                "top-level declaration must be unindented",
            );
            p.index += 1;
            continue;
        }
        if t.len() == 2 && word(&t[0], "options") && colon(&t[1]) {
            if script.prefixes.is_some() || !script.declarations.is_empty() {
                p.d.push(
                    span,
                    Category::Syntax,
                    "one options block may precede all handlers",
                );
            }
            p.index += 1;
            let begin = p.index;
            while p.index < lines.len() && lines[p.index].indent > 0 {
                let l = &lines[p.index];
                let t = &l.tokens;
                if l.indent == 4 && t.len() >= 3 && word(&t[0], "prefixes") && colon(&t[1]) {
                    if script.prefixes.is_some() {
                        p.d.push(l.span, Category::Command, "duplicate global prefixes");
                    }
                    if let Some(values) = strings(&t[2..], p.d, l.span) {
                        script.prefixes = Some(values);
                    }
                } else {
                    p.d.push(
                        l.span,
                        Category::Syntax,
                        "options supports only prefixes: followed by quoted strings",
                    );
                }
                p.index += 1;
            }
            if begin == p.index {
                p.d.push(span, Category::Syntax, "options requires an indented body");
            }
            continue;
        }
        let result = if word(&t[0], "on") && t.len() >= 3 && t.last().is_some_and(colon) {
            let header = &t[1..t.len() - 1];
            let builtin = if header.len() == 1 {
                match &header[0].kind {
                    Kind::Word(w) => match w.as_str() {
                        "message" => Some(sculk_bot::EventKind::Message),
                        "connection" => Some(sculk_bot::EventKind::Connection),
                        "synchronization" => Some(sculk_bot::EventKind::Synchronization),
                        "unknown" => Some(sculk_bot::EventKind::Unknown),
                        "lag" => Some(sculk_bot::EventKind::Lagged),
                        _ => None,
                    },
                    _ => None,
                }
            } else {
                None
            };
            let matched = p
                .registry
                .resolve(header, Group::Event)
                .filter(|m| m.consumed == header.len());
            let addon = matched.as_ref().map(|m| m.id);
            let kind = builtin.or_else(|| {
                addon.and_then(|id| match p.registry.entry(id).kind {
                    crate::addon::SyntaxKind::Event(kind) => Some(kind),
                    _ => None,
                })
            });
            p.index += 1;
            let body = p.block(4, 1);
            if let Some(kind) = kind {
                Some(Declaration::Event {
                    kind,
                    body,
                    span,
                    addon,
                })
            } else {
                p.d.push(
                    span,
                    Category::Unsupported,
                    "unsupported event; use message, connection, synchronization, unknown or lag",
                );
                None
            }
        } else if word(&t[0], "command") {
            p.command()
        } else {
            p.d.push(
                span,
                Category::Syntax,
                "expected options:, on event: or command name:",
            );
            p.index += 1;
            None
        };
        if let Some(decl) = result {
            script.declarations.push(decl);
        }
        while p.index < lines.len() && lines[p.index].indent > 0 {
            p.d.push(
                lines[p.index].span,
                Category::Syntax,
                "unexpected indentation",
            );
            p.index += 1;
        }
    }
    script
}
struct Parser<'a, 'b, 'c> {
    lines: &'a [Line],
    registry: &'a Registry,
    index: usize,
    d: &'b mut Diagnostics<'c>,
}
impl Parser<'_, '_, '_> {
    fn command(&mut self) -> Option<Declaration> {
        let line = &self.lines[self.index];
        let t = &line.tokens;
        let span = line.span;
        self.index += 1;
        if !t.last().is_some_and(colon) {
            return fail(self.d, span, "command header must end with a colon");
        }
        let mut i = 1;
        let mut prefix = String::new();
        while i < t.len() - 1 {
            match t[i].kind {
                Kind::Symbol(ch) => prefix.push(ch),
                Kind::Right => prefix.push('>'),
                _ => break,
            }
            i += 1;
        }
        let Some(Token {
            kind: Kind::Word(name),
            ..
        }) = t.get(i)
        else {
            return fail(
                self.d,
                span,
                "expected a command name after its optional prefix",
            );
        };
        i += 1;
        let name = name.clone();
        let mut arguments = Vec::new();
        while i < t.len() - 1 {
            if i + 2 >= t.len() - 1
                || !matches!(t[i].kind, Kind::Left)
                || !matches!(t[i + 2].kind, Kind::Right)
            {
                return fail(self.d, t[i].span, "required arguments use <name>");
            }
            let Kind::Word(arg) = &t[i + 1].kind else {
                return fail(self.d, t[i + 1].span, "expected a positional argument name");
            };
            arguments.push(arg.clone());
            i += 3;
        }
        let mut prefixes = None;
        let mut aliases = None;
        let mut permissions = None;
        let mut body = None;
        while self.index < self.lines.len() && self.lines[self.index].indent > 0 {
            let line = &self.lines[self.index];
            let t = &line.tokens;
            self.index += 1;
            if line.indent != 4 || t.len() < 2 || !colon(&t[1]) {
                self.d.push(
                    line.span,
                    Category::Syntax,
                    "command properties use four spaces and a colon",
                );
                continue;
            }
            if word(&t[0], "trigger") && t.len() == 2 {
                if body.is_some() {
                    self.d
                        .push(line.span, Category::Command, "duplicate trigger block");
                }
                body = Some(self.block(8, 1));
            } else if word(&t[0], "prefixes") {
                if prefixes.is_some() {
                    self.d
                        .push(line.span, Category::Command, "duplicate command prefixes");
                }
                prefixes = strings(&t[2..], self.d, line.span);
            } else if word(&t[0], "aliases") {
                if aliases.is_some() {
                    self.d
                        .push(line.span, Category::Command, "duplicate aliases");
                }
                aliases = names(&t[2..], self.d, line.span);
            } else if word(&t[0], "permissions") {
                if permissions.is_some() {
                    self.d
                        .push(line.span, Category::Command, "duplicate permissions");
                }
                permissions = strings(&t[2..], self.d, line.span);
            } else {
                self.d.push(
                    line.span,
                    Category::Unsupported,
                    "unsupported command property; use prefixes, aliases, permissions or trigger",
                );
            }
        }
        let Some(body) = body else {
            return fail(self.d, span, "command requires a trigger: block");
        };
        Some(Declaration::Command {
            name,
            explicit: if prefix.is_empty() {
                None
            } else {
                Some(prefix)
            },
            arguments,
            prefixes,
            aliases: aliases.unwrap_or_default(),
            permissions: permissions.unwrap_or_default(),
            body,
            span,
        })
    }
    fn block(&mut self, indent: usize, depth: usize) -> Vec<Statement> {
        let mut body = Vec::new();
        let start = self.index;
        if depth > self.d.limits.depth {
            if let Some(l) = self.lines.get(self.index) {
                self.d.push(
                    l.span,
                    Category::Limit,
                    "maximum block nesting depth exceeded",
                );
            }
            while self.index < self.lines.len() && self.lines[self.index].indent >= indent {
                self.index += 1;
            }
            return body;
        }
        while self.index < self.lines.len() && self.lines[self.index].indent >= indent {
            let line = &self.lines[self.index];
            let span = line.span;
            let t = &line.tokens;
            self.index += 1;
            if line.indent != indent {
                self.d.push(
                    span,
                    Category::Syntax,
                    "unexpected indentation; blocks add four spaces",
                );
                continue;
            }
            let kind = if word(&t[0], "if") && t.last().is_some_and(colon) {
                let condition = expression(&t[1..t.len() - 1], self.d, 1, span, self.registry);
                let yes = self.block(indent + 4, depth + 1);
                let mut no = Vec::new();
                if let Some(line) = self.lines.get(self.index) {
                    if line.indent == indent
                        && line.tokens.len() == 2
                        && word(&line.tokens[0], "else")
                        && colon(&line.tokens[1])
                    {
                        self.index += 1;
                        no = self.block(indent + 4, depth + 1);
                    }
                }
                condition.map(|c| StatementKind::If(c, yes, no))
            } else if word(&t[0], "reply") {
                expression(&t[1..], self.d, 1, span, self.registry).map(StatementKind::Reply)
            } else if word(&t[0], "send")
                && t.len() >= 4
                && words(&t[t.len() - 2..], &["to", "event-room"])
            {
                expression(&t[1..t.len() - 2], self.d, 1, span, self.registry)
                    .map(StatementKind::Reply)
            } else if word(&t[0], "set") && t.len() >= 4 && word(&t[2], "to") {
                if let Kind::Local(name) = &t[1].kind {
                    expression(&t[3..], self.d, 1, span, self.registry)
                        .map(|e| StatementKind::Set(name.clone(), e))
                } else {
                    self.d.push(
                        span,
                        Category::Scope,
                        "set requires an event-local variable such as {_name}",
                    );
                    None
                }
            } else if t.len() == 1 && word(&t[0], "stop") {
                Some(StatementKind::Stop)
            } else if let Some(matched) = self.registry.resolve(t, Group::Effect) {
                if matched.consumed != t.len() {
                    self.d.push(
                        t[matched.consumed].span,
                        Category::Syntax,
                        "unexpected tokens after addon effect",
                    );
                    None
                } else {
                    let arguments = matched
                        .arguments
                        .iter()
                        .map(|r| {
                            expression(&t[r.clone()], self.d, 1, t[r.start].span, self.registry)
                        })
                        .collect::<Option<Vec<_>>>();
                    arguments.map(|args| StatementKind::Addon(matched.id, args))
                }
            } else {
                self.d.push(
                    span,
                    Category::Unsupported,
                    "unknown statement; use reply, send, set, stop or if (did you mean reply?)",
                );
                None
            };
            if let Some(kind) = kind {
                body.push(Statement { kind, span });
            }
        }
        if self.index == start {
            self.d.push(
                self.lines
                    .get(start.saturating_sub(1))
                    .map(|l| l.span)
                    .unwrap_or_default(),
                Category::Syntax,
                "expected a nonempty indented block",
            );
        }
        body
    }
}
pub fn expression(
    tokens: &[Token],
    d: &mut Diagnostics<'_>,
    depth: usize,
    fallback: Span,
    registry: &Registry,
) -> Option<Expr> {
    if tokens.is_empty() {
        return fail(d, fallback, "expected an expression");
    }
    let mut p = Expression {
        tokens,
        index: 0,
        d,
        registry,
    };
    let e = p.or(depth)?;
    if p.index != tokens.len() {
        return fail(p.d, tokens[p.index].span, "unexpected tokens in expression");
    }
    Some(e)
}
struct Expression<'a, 'b, 'c> {
    tokens: &'a [Token],
    registry: &'a Registry,
    index: usize,
    d: &'b mut Diagnostics<'c>,
}
impl Expression<'_, '_, '_> {
    fn or(&mut self, depth: usize) -> Option<Expr> {
        let mut left = self.and(depth)?;
        let mut count = depth;
        while self.tokens.get(self.index).is_some_and(|t| word(t, "or")) {
            self.index += 1;
            count += 1;
            if count > self.d.limits.depth {
                return fail(
                    self.d,
                    left.span,
                    "boolean expression nesting limit exceeded",
                );
            }
            let span = left.span;
            left = Expr {
                kind: ExprKind::Or(Box::new(left), Box::new(self.and(depth)?)),
                span,
            };
        }
        Some(left)
    }
    fn and(&mut self, depth: usize) -> Option<Expr> {
        let mut left = self.compare(depth)?;
        let mut count = depth;
        while self.tokens.get(self.index).is_some_and(|t| word(t, "and")) {
            self.index += 1;
            count += 1;
            if count > self.d.limits.depth {
                return fail(
                    self.d,
                    left.span,
                    "boolean expression nesting limit exceeded",
                );
            }
            let span = left.span;
            left = Expr {
                kind: ExprKind::And(Box::new(left), Box::new(self.compare(depth)?)),
                span,
            };
        }
        Some(left)
    }
    fn compare(&mut self, depth: usize) -> Option<Expr> {
        let left = self.atom(depth)?;
        if self.tokens.get(self.index).is_some_and(|t| word(t, "is")) {
            self.index += 1;
            let neg = self.tokens.get(self.index).is_some_and(|t| word(t, "not"));
            if neg {
                self.index += 1;
            }
            let span = left.span;
            Some(Expr {
                kind: ExprKind::Equal(Box::new(left), Box::new(self.atom(depth)?), neg),
                span,
            })
        } else {
            Some(left)
        }
    }
    fn atom(&mut self, depth: usize) -> Option<Expr> {
        let Some(t) = self.tokens.get(self.index) else {
            return fail(
                self.d,
                self.tokens.last().map(|t| t.span).unwrap_or_default(),
                "expected an expression",
            );
        };
        if depth > self.d.limits.depth {
            self.d
                .push(t.span, Category::Limit, "expression depth limit exceeded");
            return None;
        }
        if self.registry.has_head(t, Group::Expression) {
            let Some(matched) = self
                .registry
                .resolve(&self.tokens[self.index..], Group::Expression)
            else {
                return fail(
                    self.d,
                    t.span,
                    "addon pattern does not match; use atomic parameters or parentheses",
                );
            };
            let base = self.index;
            let arguments = matched
                .arguments
                .iter()
                .map(|r| {
                    expression(
                        &self.tokens[base + r.start..base + r.end],
                        self.d,
                        depth + 1,
                        self.tokens[base + r.start].span,
                        self.registry,
                    )
                })
                .collect::<Option<Vec<_>>>()?;
            self.index += matched.consumed;
            return Some(Expr {
                kind: ExprKind::Addon(matched.id, arguments),
                span: t.span,
            });
        }
        self.index += 1;
        let span = t.span;
        let kind = match &t.kind {
            Kind::Integer(n) => ExprKind::Literal(Value::Integer(*n)),
            Kind::Local(name) => ExprKind::Local(name.clone()),
            Kind::Word(w) if w == "true" || w == "false" => {
                ExprKind::Literal(Value::Boolean(w == "true"))
            }
            Kind::Word(w) => ExprKind::Name(w.clone()),
            Kind::String(pieces) => {
                let mut expressions = Vec::new();
                for piece in pieces {
                    match piece {
                        Piece::Text(s) => expressions.push(Expr {
                            kind: ExprKind::Literal(Value::String(s.as_str().into())),
                            span,
                        }),
                        Piece::Expression(source, span) => {
                            let tokens = lexer::tokens(source, *span, self.d);
                            expressions.push(expression(
                                &tokens,
                                self.d,
                                depth + 1,
                                *span,
                                self.registry,
                            )?);
                        }
                    }
                }
                if expressions.len() == 1 {
                    if let ExprKind::Literal(Value::String(_)) = expressions[0].kind {
                        return Some(expressions.remove(0));
                    }
                }
                ExprKind::Template(expressions)
            }
            Kind::Open => {
                let inner = self.or(depth + 1)?;
                if !self
                    .tokens
                    .get(self.index)
                    .is_some_and(|t| matches!(t.kind, Kind::Close))
                {
                    return fail(self.d, span, "expected closing parenthesis");
                }
                self.index += 1;
                return Some(inner);
            }
            _ => {
                return fail(
                    self.d,
                    span,
                    "expected a literal, message, sender, room, argument or local variable",
                )
            }
        };
        Some(Expr { kind, span })
    }
}
