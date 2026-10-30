//! Startup-compiled byte trie; at most 128 unique prefixes of at most 64 bytes.
use std::collections::{BTreeMap, HashMap};
#[derive(Default)]
struct Node {
    next: BTreeMap<u8, usize>,
    terminal: Option<usize>,
}
pub(crate) struct Prefixes {
    nodes: Vec<Node>,
    pub tables: Vec<(String, HashMap<String, usize>)>,
}
/// Validate a string prefix. Meaningful whitespace is preserved, NUL/control
/// characters are refused. Empty prefixes require explicit operator opt-in.
pub fn valid_prefix(prefix: &str, allow_empty: bool) -> bool {
    (allow_empty || !prefix.is_empty())
        && prefix.len() <= 64
        && !prefix.chars().any(char::is_control)
}
impl Prefixes {
    pub fn new(default: &str) -> Self {
        let mut this = Self {
            nodes: vec![Node::default()],
            tables: Vec::new(),
        };
        this.add(default);
        this
    }
    pub fn index(&self, prefix: &str) -> Option<usize> {
        self.tables.iter().position(|(p, _)| p == prefix)
    }
    pub fn add(&mut self, prefix: &str) -> usize {
        if let Some(i) = self.index(prefix) {
            return i;
        }
        let index = self.tables.len();
        self.tables.push((prefix.into(), HashMap::new()));
        let mut node = 0;
        for byte in prefix.bytes() {
            node = if let Some(next) = self.nodes[node].next.get(&byte) {
                *next
            } else {
                let next = self.nodes.len();
                self.nodes.push(Node::default());
                self.nodes[node].next.insert(byte, next);
                next
            };
        }
        self.nodes[node].terminal = Some(index);
        index
    }
    pub fn longest(&self, text: &str) -> Option<usize> {
        let mut node = 0;
        let mut found = self.nodes[0].terminal;
        for byte in text.bytes().take(64) {
            let Some(next) = self.nodes[node].next.get(&byte) else {
                break;
            };
            node = *next;
            if let Some(index) = self.nodes[node].terminal {
                found = Some(index);
            }
        }
        found
    }
}
