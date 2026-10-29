//! Startup command metadata and bounded argument parsing. No protocol dependence.
use std::ops::Range;

/// Command registration compiled once at startup.
#[derive(Clone, Debug)]
pub struct Command {
    pub(crate) name: String,
    pub(crate) aliases: Vec<String>,
    pub(crate) permissions: Vec<crate::PermissionId>,
}
impl Command {
    /// Command names/aliases are ASCII letters, digits, `_` or `-`, 1..64 bytes.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            aliases: Vec::new(),
            permissions: Vec::new(),
        }
    }
    /// Add a startup alias, checked for collisions on registration.
    pub fn alias(mut self, alias: impl Into<String>) -> Self {
        self.aliases.push(alias.into());
        self
    }
    /// All required permissions must pass before invoking the command.
    pub fn require(mut self, permission: crate::PermissionId) -> Self {
        self.permissions.push(permission);
        self
    }
}
pub(crate) fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}
pub(crate) fn key(name: &str, case_sensitive: bool) -> String {
    if case_sensitive {
        name.into()
    } else {
        name.to_ascii_lowercase()
    }
}
/// Invalid quoting or an explicit local work bound. No input in diagnostics.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    /// An unterminated quote or trailing escape was encountered.
    #[error("invalid command quoting")]
    Quoting,
    /// At most 64 arguments and 16 KiB command text are accepted.
    #[error("command input limit exceeded")]
    Limit,
    /// Command token does not follow the registration name grammar.
    #[error("invalid command name")]
    Name,
}
#[derive(Debug)]
enum Argument {
    Slice(Range<usize>),
    Decoded(String),
}
/// Parsed invocation referencing the original text by byte range. Unescaped,
/// unquoted arguments allocate no individual strings. Debug redacts arguments.
pub(crate) struct Invocation {
    name: Range<usize>,
    arguments: Vec<Argument>,
}
impl std::fmt::Debug for Invocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Invocation([REDACTED])")
    }
}
impl Invocation {
    /// Name as entered, without the prefix.
    pub fn name<'a>(&self, text: &'a str) -> &'a str {
        &text[self.name.clone()]
    }
    /// Borrow arguments using the same original text supplied to `parse`.
    /// Prefer Context::arguments, which binds the correct message automatically.
    pub fn arguments<'a>(&'a self, text: &'a str) -> impl ExactSizeIterator<Item = &'a str> {
        self.arguments.iter().map(move |a| match a {
            Argument::Slice(r) => &text[r.clone()],
            Argument::Decoded(s) => s.as_str(),
        })
    }
}
/// Parse an exact prefix at the beginning of a message. Whitespace separates
/// arguments; single/double quotes concatenate segments, backslash escapes the
/// next Unicode scalar. This is a command grammar, never a shell expansion.
pub(crate) fn parse_invocation(text: &str, prefix: &str) -> Result<Option<Invocation>, ParseError> {
    if prefix.is_empty() || !text.starts_with(prefix) {
        return Ok(None);
    }
    if text.len() > 16384 {
        return Err(ParseError::Limit);
    }
    let rest = &text[prefix.len()..];
    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    if end == 0 {
        return Ok(None);
    }
    if !valid_name(&rest[..end]) {
        return Err(ParseError::Name);
    }
    let name = prefix.len()..prefix.len() + end;
    let mut chars = text[name.end..].char_indices().peekable();
    let base = name.end;
    let mut arguments = Vec::new();
    while let Some((offset, first)) = chars.next() {
        if first.is_whitespace() {
            continue;
        }
        if arguments.len() == 64 {
            return Err(ParseError::Limit);
        }
        let start = base + offset;
        let mut finish = start;
        let mut decoded = None::<String>;
        let mut quote = None;
        let mut current = Some((offset, first));
        while let Some((i, c)) = current {
            let position = base + i;
            finish = position + c.len_utf8();
            if c == '\\' {
                let target = decoded.get_or_insert_with(|| text[start..position].to_owned());
                let (next_i, next) = chars.next().ok_or(ParseError::Quoting)?;
                target.push(next);
                finish = base + next_i + next.len_utf8();
            } else if quote == Some(c) {
                quote = None;
            } else if quote.is_none() && (c == '\'' || c == '"') {
                decoded.get_or_insert_with(|| text[start..position].to_owned());
                quote = Some(c);
            } else if let Some(target) = &mut decoded {
                target.push(c);
            }
            if chars.peek().is_some_and(|(_, next)| next.is_whitespace()) && quote.is_none() {
                break;
            }
            current = chars.next();
        }
        if quote.is_some() {
            return Err(ParseError::Quoting);
        }
        arguments.push(match decoded {
            Some(s) => Argument::Decoded(s),
            None => Argument::Slice(start..finish),
        });
    }
    Ok(Some(Invocation { name, arguments }))
}

/// Borrowed parsed command, bound to the input text so argument ranges cannot be
/// accidentally evaluated against a different message. Debug redacts all input.
pub struct Parsed<'a> {
    text: &'a str,
    invocation: Invocation,
}
impl std::fmt::Debug for Parsed<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Parsed([REDACTED])")
    }
}
impl Parsed<'_> {
    /// Command token without prefix.
    pub fn name(&self) -> &str {
        self.invocation.name(self.text)
    }
    /// Decoded arguments, borrowing unquoted/unescaped input directly.
    pub fn arguments(&self) -> impl ExactSizeIterator<Item = &str> {
        self.invocation.arguments(self.text)
    }
}
/// Parse a bounded command without registration lookup or permission execution.
pub fn parse<'a>(text: &'a str, prefix: &str) -> Result<Option<Parsed<'a>>, ParseError> {
    Ok(parse_invocation(text, prefix)?.map(|invocation| Parsed { text, invocation }))
}
