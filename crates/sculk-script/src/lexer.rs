use crate::{Category, Diagnostics, Span};
#[derive(Clone, Debug)]
pub enum Piece {
    Text(String),
    Expression(String, Span),
}
#[derive(Clone, Debug)]
pub enum Kind {
    Word(String),
    Integer(i64),
    String(Vec<Piece>),
    Local(String),
    Colon,
    Comma,
    Left,
    Right,
    Open,
    Close,
    Symbol(char),
}
#[derive(Clone, Debug)]
pub struct Token {
    pub kind: Kind,
    pub span: Span,
}
pub struct Line {
    pub indent: usize,
    pub tokens: Vec<Token>,
    pub span: Span,
}
fn at(columns: &[usize], base: Span, start: usize, end: usize) -> Span {
    Span {
        start: base.start + start,
        end: base.start + end,
        line: base.line,
        column: base.column + columns.partition_point(|i| *i < start),
    }
}
pub fn tokens(text: &str, base: Span, d: &mut Diagnostics<'_>) -> Vec<Token> {
    let columns: Vec<usize> = text.char_indices().map(|(i, _)| i).collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < text.len() {
        let start = i;
        let c = text[i..].chars().next().unwrap();
        i += c.len_utf8();
        if c.is_whitespace() {
            continue;
        }
        if c == '#' {
            break;
        }
        let kind = match c {
            ':' => Kind::Colon,
            ',' => Kind::Comma,
            '<' => Kind::Left,
            '>' => Kind::Right,
            '(' => Kind::Open,
            ')' => Kind::Close,
            '{' => {
                let Some(end) = text[i..].find('}') else {
                    d.push(
                        at(&columns, base, start, i),
                        Category::Lexical,
                        "unterminated event-local variable",
                    );
                    break;
                };
                let name = &text[i..i + end];
                i += end + 1;
                if !name.starts_with('_')
                    || name.len() < 2
                    || name.len() > 64
                    || !name.chars().all(|c| c.is_alphanumeric() || c == '_')
                {
                    d.push(
                        at(&columns, base, start, i),
                        Category::Scope,
                        "use an event-local variable such as {_name}",
                    );
                    continue;
                }
                Kind::Local(name.into())
            }
            '"' => {
                let mut pieces = Vec::new();
                let mut literal = String::new();
                let mut closed = false;
                while i < text.len() {
                    let ch = text[i..].chars().next().unwrap();
                    i += ch.len_utf8();
                    match ch {
                        '"' => {
                            closed = true;
                            break;
                        }
                        '\\' => {
                            let Some(next) = text[i..].chars().next() else {
                                break;
                            };
                            i += next.len_utf8();
                            match next {
                                'n' => literal.push('\n'),
                                'r' => literal.push('\r'),
                                't' => literal.push('\t'),
                                '"' | '\\' | '%' => literal.push(next),
                                _ => d.push(
                                    at(&columns, base, i - next.len_utf8(), i),
                                    Category::Lexical,
                                    "unsupported string escape",
                                ),
                            }
                        }
                        '%' => {
                            if !literal.is_empty() {
                                pieces.push(Piece::Text(std::mem::take(&mut literal)));
                            }
                            let begin = i;
                            let Some(end) = text[i..].find('%') else {
                                d.push(
                                    at(&columns, base, begin - 1, i),
                                    Category::Syntax,
                                    "unterminated interpolation placeholder",
                                );
                                break;
                            };
                            let value = &text[i..i + end];
                            i += end + 1;
                            pieces.push(Piece::Expression(
                                value.into(),
                                at(&columns, base, begin, i - 1),
                            ));
                        }
                        _ => literal.push(ch),
                    }
                    if literal.len() > d.limits.string_bytes || pieces.len() > d.limits.instructions
                    {
                        d.push(
                            at(&columns, base, start, i),
                            Category::Limit,
                            "string or interpolation segment limit exceeded",
                        );
                        break;
                    }
                }
                if !closed {
                    d.push(
                        at(&columns, base, start, i),
                        Category::Lexical,
                        "unterminated string literal",
                    );
                }
                if !literal.is_empty() || pieces.is_empty() {
                    pieces.push(Piece::Text(literal));
                }
                Kind::String(pieces)
            }
            c if c.is_ascii_digit()
                || c == '-' && text[i..].starts_with(|c: char| c.is_ascii_digit()) =>
            {
                while i < text.len() && text.as_bytes()[i].is_ascii_digit() {
                    i += 1;
                }
                match text[start..i].parse::<i64>() {
                    Ok(n) => Kind::Integer(n),
                    Err(_) => {
                        d.push(
                            at(&columns, base, start, i),
                            Category::Type,
                            "integer outside signed 64-bit range",
                        );
                        continue;
                    }
                }
            }
            c if c.is_alphanumeric() || c == '_' || c == '-' => {
                while let Some(next) = text[i..].chars().next() {
                    if !(next.is_alphanumeric() || next == '_' || next == '-') {
                        break;
                    }
                    i += next.len_utf8();
                }
                Kind::Word(text[start..i].into())
            }
            other => Kind::Symbol(other),
        };
        out.push(Token {
            kind,
            span: at(&columns, base, start, i),
        });
        if out.len() > d.limits.instructions.saturating_mul(32) {
            d.push(base, Category::Limit, "token count limit exceeded");
            break;
        }
    }
    out
}
pub fn lex(source: &str, d: &mut Diagnostics<'_>) -> Vec<Line> {
    let mut lines = Vec::new();
    let mut offset = 0;
    for (index, raw) in source.split_inclusive('\n').enumerate() {
        let line = raw.trim_end_matches(['\n', '\r']);
        let indent = line.len() - line.trim_start_matches(' ').len();
        let base = Span {
            start: offset + indent,
            end: offset + line.len(),
            line: index + 1,
            column: indent + 1,
        };
        offset += raw.len();
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        if line[indent..]
            .chars()
            .next()
            .is_some_and(char::is_whitespace)
        {
            d.push(
                base,
                Category::Lexical,
                "code indentation uses ASCII spaces, not tabs or other whitespace",
            );
            continue;
        }
        let t = tokens(&line[indent..], base, d);
        if t.is_empty() {
            continue;
        }
        if indent % 4 != 0 {
            d.push(
                base,
                Category::Syntax,
                "indentation must use groups of four spaces",
            );
        }
        lines.push(Line {
            indent,
            tokens: t,
            span: base,
        });
        if lines.len() > d.limits.instructions.saturating_mul(4) {
            d.push(base, Category::Limit, "source statement limit exceeded");
            break;
        }
    }
    lines
}
