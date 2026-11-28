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
                        "scheduled" => Some(sculk_bot::EventKind::Scheduled),
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
        } else if word(&t[0], "function") {
            p.function()
        } else {
            p.d.push(
                span,
                Category::Syntax,
                "expected options, an event, a command or a function",
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
fn scalar_type(token: &Token, d: &mut Diagnostics<'_>) -> Option<Type> {
    if let Kind::Word(name) = &token.kind {
        if let Some(ty) = Type::named(name) {
            return Some(ty);
        }
    }
    d.push(
        token.span,
        Category::Type,
        "expected a scalar, list or map type",
    );
    None
}

struct Parser<'a, 'b, 'c> {
    lines: &'a [Line],
    registry: &'a Registry,
    index: usize,
    d: &'b mut Diagnostics<'c>,
}
impl Parser<'_, '_, '_> {
    fn function(&mut self) -> Option<Declaration> {
        let line = &self.lines[self.index];
        self.index += 1;
        let span = line.span;
        let tokens = &line.tokens;
        let [_, Token {
            kind: Kind::Word(name),
            ..
        }, Token {
            kind: Kind::Open, ..
        }, tail @ ..] = tokens.as_slice()
        else {
            return fail(
                self.d,
                span,
                "expected function name followed by typed parameters",
            );
        };
        let Some(end) = tail
            .iter()
            .position(|token| matches!(token.kind, Kind::Close))
        else {
            return fail(
                self.d,
                span,
                "expected closing parenthesis in function declaration",
            );
        };
        let mut parameters = Vec::new();
        for (index, parameter) in tail[..end].chunks(4).enumerate() {
            let [Token {
                kind: Kind::Word(parameter_name),
                ..
            }, separator, ty, rest @ ..] = parameter
            else {
                return fail(self.d, span, "expected parameter name and type");
            };
            if !colon(separator)
                || (index * 4 + 3 == end && !rest.is_empty())
                || (index * 4 + 3 != end
                    && !matches!(
                        rest,
                        [Token {
                            kind: Kind::Comma,
                            ..
                        }]
                    ))
                || index * 4 + 4 == end
            {
                return fail(self.d, span, "expected comma-separated typed parameters");
            }
            if parameters.len() >= self.d.limits.locals.min(16) {
                self.d
                    .push(span, Category::Limit, "function parameter limit exceeded");
                return None;
            }
            parameters.push((parameter_name.clone(), scalar_type(ty, self.d)?));
        }
        let result = match &tail[end + 1..] {
            [last] if colon(last) => None,
            [dash, Token {
                kind: Kind::Right, ..
            }, ty, last]
                if word(dash, "-") && colon(last) =>
            {
                if word(ty, "void") {
                    None
                } else {
                    Some(scalar_type(ty, self.d)?)
                }
            }
            _ => return fail(self.d, span, "expected function return type or colon"),
        };
        let body = self.block(4, 1);
        Some(Declaration::Function {
            name: name.clone(),
            parameters,
            result,
            body,
            span,
        })
    }

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
            let end = t[i..t.len() - 1]
                .iter()
                .position(|token| matches!(token.kind, Kind::Right));
            let Some(end) = end.map(|offset| i + offset) else {
                return fail(self.d, t[i].span, "expected an argument enclosed in < >");
            };
            if !matches!(t[i].kind, Kind::Left) {
                return fail(self.d, t[i].span, "expected an argument enclosed in < >");
            }
            arguments.push(argument(&t[i + 1..end], t[i].span, self.d)?);
            i = end + 1;
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
            let global = match t.last().map(|token| &token.kind) {
                Some(Kind::Global(name)) => Some(name),
                _ => None,
            };
            let kind = if word(&t[0], "if") && t.last().is_some_and(colon) {
                self.conditional(&t[1..t.len() - 1], indent, depth, span)
            } else if word(&t[0], "repeat")
                && t.len() >= 4
                && word(&t[t.len() - 2], "times")
                && t.last().is_some_and(colon)
            {
                let count = expression(&t[1..t.len() - 2], self.d, 1, span, self.registry);
                let body = self.block(indent + 4, depth + 1);
                count.map(|count| StatementKind::Repeat(count, body))
            } else if word(&t[0], "while") && t.last().is_some_and(colon) {
                let condition = expression(&t[1..t.len() - 1], self.d, 1, span, self.registry);
                let body = self.block(indent + 4, depth + 1);
                condition.map(|condition| StatementKind::While(condition, body))
            } else if word(&t[0], "return") {
                if t.len() == 1 {
                    Some(StatementKind::Return(None))
                } else {
                    expression(&t[1..], self.d, 1, span, self.registry)
                        .map(|result| StatementKind::Return(Some(result)))
                }
            } else if word(&t[0], "call") {
                expression(&t[1..], self.d, 1, span, self.registry).map(StatementKind::Call)
            } else if word(&t[0], "reply") {
                let (value, kind) = if t
                    .get(1..4)
                    .is_some_and(|head| words(head, &["to", "event-message", "with"]))
                {
                    (&t[4..], ReplyKind::Quote)
                } else {
                    (&t[1..], ReplyKind::Room)
                };
                expression(value, self.d, 1, span, self.registry)
                    .map(|expr| StatementKind::Reply(expr, kind))
            } else if word(&t[0], "send")
                && t.len() >= 4
                && words(&t[t.len() - 2..], &["to", "event-room"])
            {
                expression(&t[1..t.len() - 2], self.d, 1, span, self.registry)
                    .map(|expr| StatementKind::Reply(expr, ReplyKind::Room))
            } else if word(&t[0], "set") && t.len() >= 4 && word(&t[2], "to") {
                let value = expression(&t[3..], self.d, 1, span, self.registry);
                match &t[1].kind {
                    Kind::Local(name) => value.map(|e| StatementKind::Set(name.clone(), e)),
                    Kind::Global(name) => {
                        value.map(|e| StatementKind::UpdateGlobal(name.clone(), e, false))
                    }
                    Kind::State(name, ty) => {
                        value.map(|expr| StatementKind::SetState(name.clone(), *ty, expr))
                    }
                    _ => fail(
                        self.d,
                        span,
                        "set requires a local variable or global counter",
                    ),
                }
            } else if word(&t[0], "add")
                && t.len() >= 4
                && word(&t[t.len() - 2], "to")
                && global.is_some()
            {
                expression(&t[1..t.len() - 2], self.d, 1, span, self.registry)
                    .zip(global)
                    .map(|(value, name)| StatementKind::UpdateGlobal(name.clone(), value, true))
            } else if word(&t[0], "delete") && t.len() == 2 {
                match &t[1].kind {
                    Kind::Global(name) => Some(StatementKind::DeleteGlobal(name.clone())),
                    Kind::State(name, ty) => Some(StatementKind::DeleteState(name.clone(), *ty)),
                    _ => fail(
                        self.d,
                        span,
                        "delete requires a global counter or a state variable",
                    ),
                }
            } else if word(&t[0], "wait") && t.len() >= 3 && word(&t[t.len() - 1], "milliseconds") {
                expression(&t[1..t.len() - 1], self.d, 1, span, self.registry)
                    .map(StatementKind::Wait)
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

    fn conditional(
        &mut self,
        tokens: &[Token],
        indent: usize,
        depth: usize,
        span: Span,
    ) -> Option<StatementKind> {
        if depth > self.d.limits.depth {
            self.d.push(
                span,
                Category::Limit,
                "maximum conditional nesting depth exceeded",
            );
            return None;
        }
        let condition = expression(tokens, self.d, 1, span, self.registry);
        let yes = self.block(indent + 4, depth + 1);
        let mut no = Vec::new();
        if let Some(line) = self.lines.get(self.index) {
            let tail = &line.tokens;
            if line.indent == indent && word(&tail[0], "else") && tail.last().is_some_and(colon) {
                if tail.len() == 2 {
                    self.index += 1;
                    no = self.block(indent + 4, depth + 1);
                } else if tail.len() >= 4 && word(&tail[1], "if") {
                    self.index += 1;
                    let span = line.span;
                    if let Some(kind) =
                        self.conditional(&tail[2..tail.len() - 1], indent, depth + 1, span)
                    {
                        no.push(Statement { kind, span });
                    }
                }
            }
        }
        condition.map(|condition| StatementKind::If(condition, yes, no))
    }
}

fn argument(tokens: &[Token], span: Span, d: &mut Diagnostics<'_>) -> Option<ArgumentSpec> {
    let [Token {
        kind: Kind::Word(name),
        ..
    }, rest @ ..] = tokens
    else {
        return fail(d, span, "expected a positional argument name");
    };
    let (kind, default) = match rest {
        [] => (ArgumentKind::Text, None),
        [separator, Token {
            kind: Kind::Word(ty),
            ..
        }, tail @ ..]
            if colon(separator) =>
        {
            let kind = match ty.as_str() {
                "text" | "string" => ArgumentKind::Text,
                "remaining-text" => ArgumentKind::RemainingText,
                "integer" => ArgumentKind::Integer,
                "boolean" => ArgumentKind::Boolean,
                "duration" => ArgumentKind::Duration,
                "room" => ArgumentKind::Room,
                "member" => ArgumentKind::Member,
                "message" => ArgumentKind::Message,
                _ => return fail(d, span, "unknown command argument type"),
            };
            let default = match tail {
                [] => None,
                [equals, value] if matches!(equals.kind, Kind::Symbol('=')) => {
                    Some(match (&value.kind, kind) {
                        (Kind::String(_), ArgumentKind::Text | ArgumentKind::RemainingText) => {
                            Value::String(string(value, d)?.into())
                        }
                        (Kind::Integer(value), ArgumentKind::Integer) => Value::Integer(*value),
                        _ if kind == ArgumentKind::Boolean && word(value, "true") => {
                            Value::Boolean(true)
                        }
                        _ if kind == ArgumentKind::Boolean && word(value, "false") => {
                            Value::Boolean(false)
                        }
                        _ => {
                            return fail(
                                d,
                                span,
                                "expected a literal text, integer or boolean default",
                            )
                        }
                    })
                }
                _ => return fail(d, span, "expected <name: type> or <name: type = literal>"),
            };
            (kind, default)
        }
        _ => return fail(d, span, "expected <name> or <name: type>"),
    };
    Some(ArgumentSpec {
        name: name.clone(),
        kind,
        default,
    })
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
        let mut left = self.negation(depth)?;
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
                kind: ExprKind::And(Box::new(left), Box::new(self.negation(depth)?)),
                span,
            };
        }
        Some(left)
    }
    fn negation(&mut self, depth: usize) -> Option<Expr> {
        if let Some(token) = self
            .tokens
            .get(self.index)
            .filter(|token| word(token, "not") && !self.registry.has_head(token, Group::Expression))
        {
            let span = token.span;
            if depth >= self.d.limits.depth {
                return fail(self.d, span, "expression depth limit exceeded");
            }
            self.index += 1;
            let value = Box::new(self.negation(depth + 1)?);
            return Some(Expr {
                kind: ExprKind::Not(value),
                span,
            });
        }
        self.compare(depth)
    }

    fn compare(&mut self, depth: usize) -> Option<Expr> {
        let left = self.arithmetic(depth, false)?;
        if self.tokens.get(self.index).is_some_and(|t| word(t, "is")) {
            self.index += 1;
            let neg = self.tokens.get(self.index).is_some_and(|t| word(t, "not"));
            self.index += usize::from(neg);
            let span = left.span;
            return Some(Expr {
                kind: ExprKind::Equal(
                    Box::new(left),
                    Box::new(self.arithmetic(depth, false)?),
                    neg,
                ),
                span,
            });
        }
        let operator = match &self.tokens.get(self.index..).unwrap_or_default() {
            [Token {
                kind: Kind::Left, ..
            }, Token {
                kind: Kind::Symbol('='),
                ..
            }, ..] => Some((Binary::LessEqual, 2)),
            [Token {
                kind: Kind::Right, ..
            }, Token {
                kind: Kind::Symbol('='),
                ..
            }, ..] => Some((Binary::GreaterEqual, 2)),
            [Token {
                kind: Kind::Left, ..
            }, ..] => Some((Binary::Less, 1)),
            [Token {
                kind: Kind::Right, ..
            }, ..] => Some((Binary::Greater, 1)),
            [first, ..] if word(first, "contains") => Some((Binary::Contains, 1)),
            [first, second, ..] if word(second, "with") && word(first, "starts") => {
                Some((Binary::StartsWith, 2))
            }
            [first, second, ..] if word(second, "with") && word(first, "ends") => {
                Some((Binary::EndsWith, 2))
            }
            _ => None,
        };
        let Some((operator, width)) = operator else {
            return Some(left);
        };
        self.index += width;
        let span = left.span;
        let right = self.arithmetic(depth, false)?;
        Some(Expr {
            kind: ExprKind::Binary(operator, Box::new(left), Box::new(right)),
            span,
        })
    }

    fn arithmetic(&mut self, depth: usize, product: bool) -> Option<Expr> {
        let mut left = if product {
            self.atom(depth)?
        } else {
            self.arithmetic(depth, true)?
        };
        let mut count = depth;
        while let Some(token) = self.tokens.get(self.index) {
            let operator = match (&token.kind, product) {
                (Kind::Symbol('+'), false) => Binary::Add,
                (Kind::Word(value), false) if value == "-" => Binary::Subtract,
                (Kind::Symbol('*'), true) => Binary::Multiply,
                (Kind::Symbol('/'), true) => Binary::Divide,
                (Kind::Word(value), true) if value == "mod" => Binary::Remainder,
                _ => break,
            };
            count += 1;
            if count > self.d.limits.depth {
                return fail(self.d, left.span, "arithmetic nesting limit exceeded");
            }
            self.index += 1;
            let right = if product {
                self.atom(depth)?
            } else {
                self.arithmetic(depth, true)?
            };
            let span = left.span;
            left = Expr {
                kind: ExprKind::Binary(operator, Box::new(left), Box::new(right)),
                span,
            };
        }
        Some(left)
    }

    fn call(&mut self, name: &str, depth: usize, span: Span) -> Option<ExprKind> {
        self.index += 1;
        let mut arguments = Vec::new();
        if self
            .tokens
            .get(self.index)
            .is_some_and(|t| matches!(t.kind, Kind::Close))
        {
            self.index += 1;
            return Some(ExprKind::Call(name.into(), arguments));
        }
        loop {
            if arguments.len() == 16 {
                self.d
                    .push(span, Category::Limit, "function argument limit exceeded");
                return None;
            }
            arguments.push(self.or(depth + 1)?);
            match self.tokens.get(self.index).map(|t| &t.kind) {
                Some(Kind::Close) => {
                    self.index += 1;
                    return Some(ExprKind::Call(name.into(), arguments));
                }
                Some(Kind::Comma) => self.index += 1,
                _ => return fail(self.d, span, "expected comma or closing parenthesis"),
            }
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
        if let Kind::Word(name) = &t.kind {
            if self.tokens.get(self.index + 1).is_some_and(|next| {
                matches!(next.kind, Kind::Open) && t.span.end == next.span.start
            }) {
                self.index += 1;
                return Some(Expr {
                    kind: self.call(name, depth, t.span)?,
                    span: t.span,
                });
            }
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
            Kind::Global(name) => ExprKind::Global(name.clone()),
            Kind::State(name, ty) => ExprKind::State(name.clone(), *ty),
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
                    "expected a literal, context value, variable or function call",
                )
            }
        };
        Some(Expr { kind, span })
    }
}
