//! A small regular expression engine for extracting values from document text.
//!
//! It runs as a Pike VM (Thompson NFA with captures): matching time grows linearly with the
//! text, whatever the pattern, so a rule can never hang on a hostile expression. Matching
//! ignores letter case, like every text search in FilePilot.
//!
//! Supported: literals, `.`, classes `[a-z]` `[^…]`, `\d \D \w \W \s \S`, anchors `^ $`
//! (per line) and `\b \B`, groups `(…)` and `(?:…)`, alternation `|`, quantifiers
//! `* + ? {n} {n,} {n,m}` and their lazy forms (`*?` …). Not supported: backreferences
//! and lookaround.

use std::fmt;

/// Upper bound for the compiled program, so `(a{1000}){1000}` is refused instead of
/// taking memory.
const MAX_INSTRUCTIONS: usize = 20_000;
/// Upper bound for one `{n,m}` repetition.
const MAX_REPEAT: u32 = 1000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegexError(pub String);

impl fmt::Display for RegexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for RegexError {}

fn err<T>(msg: impl Into<String>) -> Result<T, RegexError> {
    Err(RegexError(msg.into()))
}

/// A parsed expression.
#[derive(Debug, Clone)]
pub(crate) enum Node {
    Empty,
    Char(char),
    Any,
    Class(Class),
    Start,
    End,
    WordBoundary(bool),
    Group(Box<Node>, Option<usize>),
    Concat(Vec<Node>),
    Alt(Vec<Node>),
    Repeat { node: Box<Node>, min: u32, max: Option<u32>, greedy: bool },
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Class {
    negated: bool,
    items: Vec<ClassItem>,
}

#[derive(Debug, Clone, PartialEq)]
enum ClassItem {
    Range(char, char),
    Alpha,
    Digit(bool),
    Word(bool),
    Space(bool),
}

impl Class {
    pub(crate) fn digit() -> Class {
        Class { negated: false, items: vec![ClassItem::Digit(true)] }
    }
    pub(crate) fn letter() -> Class {
        Class { negated: false, items: vec![ClassItem::Alpha] }
    }
    pub(crate) fn space() -> Class {
        Class { negated: false, items: vec![ClassItem::Space(true)] }
    }
    pub(crate) fn not_newline() -> Class {
        Class { negated: true, items: vec![ClassItem::Range('\n', '\n')] }
    }

    fn matches(&self, c: char) -> bool {
        let lc = fold(c);
        let hit = self.items.iter().any(|it| match *it {
            ClassItem::Range(a, b) => {
                (a..=b).contains(&c) || (a..=b).contains(&lc) || (fold(a)..=fold(b)).contains(&lc)
            }
            ClassItem::Alpha => c.is_alphabetic(),
            ClassItem::Digit(yes) => c.is_ascii_digit() == yes,
            ClassItem::Word(yes) => is_word(c) == yes,
            ClassItem::Space(yes) => c.is_whitespace() == yes,
        });
        hit != self.negated
    }
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Case folding for comparisons: the first character of the lowercase form.
fn fold(c: char) -> char {
    if c.is_ascii() {
        c.to_ascii_lowercase()
    } else {
        c.to_lowercase().next().unwrap_or(c)
    }
}

struct Parser<'a> {
    chars: std::iter::Peekable<std::str::Chars<'a>>,
    groups: usize,
}

impl Parser<'_> {
    fn parse_alt(&mut self, depth: usize) -> Result<Node, RegexError> {
        if depth > 100 {
            return err("too many nested groups");
        }
        let mut alts = vec![self.parse_concat(depth)?];
        while self.chars.peek() == Some(&'|') {
            self.chars.next();
            alts.push(self.parse_concat(depth)?);
        }
        Ok(if alts.len() == 1 { alts.pop().expect("one") } else { Node::Alt(alts) })
    }

    fn parse_concat(&mut self, depth: usize) -> Result<Node, RegexError> {
        let mut items = Vec::new();
        while let Some(&c) = self.chars.peek() {
            if c == '|' || c == ')' {
                break;
            }
            let atom = self.parse_atom(depth)?;
            items.push(self.parse_quantifier(atom)?);
        }
        Ok(match items.len() {
            0 => Node::Empty,
            1 => items.pop().expect("one"),
            _ => Node::Concat(items),
        })
    }

    fn parse_atom(&mut self, depth: usize) -> Result<Node, RegexError> {
        let c = self.chars.next().expect("peeked");
        Ok(match c {
            '.' => Node::Any,
            '^' => Node::Start,
            '$' => Node::End,
            '(' => {
                let capture = if self.chars.peek() == Some(&'?') {
                    self.chars.next();
                    if self.chars.next() != Some(':') {
                        return err("only (?:…) groups are supported");
                    }
                    None
                } else {
                    self.groups += 1;
                    Some(self.groups)
                };
                let inner = self.parse_alt(depth + 1)?;
                if self.chars.next() != Some(')') {
                    return err("missing )");
                }
                Node::Group(Box::new(inner), capture)
            }
            '[' => Node::Class(self.parse_class()?),
            '\\' => self.parse_escape(false)?,
            '*' | '+' | '?' => return err(format!("nothing to repeat before {c}")),
            '{' if self.looks_like_repeat() => return err("nothing to repeat before {"),
            c => Node::Char(c),
        })
    }

    /// `{` starts a repetition only when followed by digits and `}` (as in most engines);
    /// otherwise it is a literal brace.
    fn looks_like_repeat(&self) -> bool {
        let mut ahead = self.chars.clone();
        // Called both before and after the `{` is taken.
        if ahead.peek() == Some(&'{') {
            ahead.next();
        }
        let rest: String = ahead.take(12).collect();
        let Some(end) = rest.find('}') else { return false };
        let body = &rest[..end];
        !body.is_empty() && body.split(',').count() <= 2 && body.chars().all(|c| c.is_ascii_digit() || c == ',') && !body.starts_with(',')
    }

    fn parse_escape(&mut self, in_class: bool) -> Result<Node, RegexError> {
        let Some(c) = self.chars.next() else { return err("\\ at the end") };
        let class = |item| Node::Class(Class { negated: false, items: vec![item] });
        Ok(match c {
            'd' => class(ClassItem::Digit(true)),
            'D' => class(ClassItem::Digit(false)),
            'w' => class(ClassItem::Word(true)),
            'W' => class(ClassItem::Word(false)),
            's' => class(ClassItem::Space(true)),
            'S' => class(ClassItem::Space(false)),
            'b' if !in_class => Node::WordBoundary(true),
            'B' if !in_class => Node::WordBoundary(false),
            'n' => Node::Char('\n'),
            't' => Node::Char('\t'),
            'r' => Node::Char('\r'),
            c if c.is_alphanumeric() => return err(format!("unknown escape \\{c}")),
            c => Node::Char(c),
        })
    }

    fn parse_class(&mut self) -> Result<Class, RegexError> {
        let mut class = Class { negated: false, items: Vec::new() };
        if self.chars.peek() == Some(&'^') {
            self.chars.next();
            class.negated = true;
        }
        let mut first = true;
        loop {
            let Some(c) = self.chars.next() else { return err("missing ]") };
            if c == ']' && !first {
                break;
            }
            first = false;
            let lo = if c == '\\' {
                match self.parse_escape(true)? {
                    Node::Char(ch) => ch,
                    Node::Class(inner) => {
                        class.items.extend(inner.items);
                        continue;
                    }
                    _ => unreachable!("escape in a class"),
                }
            } else {
                c
            };
            // A range `a-z`, unless `-` is the last character before `]`.
            let mut ahead = self.chars.clone();
            if ahead.next() == Some('-') && ahead.peek().is_some_and(|&n| n != ']') {
                self.chars.next();
                let mut hi = self.chars.next().expect("peeked");
                if hi == '\\' {
                    match self.parse_escape(true)? {
                        Node::Char(ch) => hi = ch,
                        _ => return err("invalid range"),
                    }
                }
                if hi < lo {
                    return err(format!("invalid range {lo}-{hi}"));
                }
                class.items.push(ClassItem::Range(lo, hi));
            } else {
                class.items.push(ClassItem::Range(lo, lo));
            }
        }
        Ok(class)
    }

    fn parse_quantifier(&mut self, atom: Node) -> Result<Node, RegexError> {
        let next = self.chars.peek().copied();
        let (min, max) = match next {
            Some('*') => (0, None),
            Some('+') => (1, None),
            Some('?') => (0, Some(1)),
            Some('{') if self.looks_like_repeat() => {
                self.chars.next();
                let body: String = self.chars.by_ref().take_while(|&c| c != '}').collect();
                let num = |s: &str| s.parse::<u32>().ok().filter(|n| *n <= MAX_REPEAT);
                let (min, max) = match body.split_once(',') {
                    None => {
                        let n = num(&body).ok_or_else(|| RegexError(format!("repetition {{{body}}} too large")))?;
                        (n, Some(n))
                    }
                    Some((a, "")) => (num(a).ok_or_else(|| RegexError("repetition too large".into()))?, None),
                    Some((a, b)) => {
                        let (a, b) = (num(a), num(b));
                        match (a, b) {
                            (Some(a), Some(b)) if a <= b => (a, Some(b)),
                            _ => return err(format!("invalid repetition {{{body}}}")),
                        }
                    }
                };
                return self.finish_quantifier(atom, min, max);
            }
            _ => return Ok(atom),
        };
        self.chars.next();
        self.finish_quantifier(atom, min, max)
    }

    fn finish_quantifier(&mut self, atom: Node, min: u32, max: Option<u32>) -> Result<Node, RegexError> {
        if matches!(atom, Node::Start | Node::End | Node::WordBoundary(_)) {
            return err("an anchor cannot be repeated");
        }
        let greedy = if self.chars.peek() == Some(&'?') {
            self.chars.next();
            false
        } else {
            true
        };
        if matches!(self.chars.peek(), Some('*' | '+' | '?')) {
            return err("two quantifiers in a row");
        }
        Ok(Node::Repeat { node: Box::new(atom), min, max, greedy })
    }
}

#[derive(Debug, Clone)]
enum Inst {
    Char(char),
    Any,
    Class(usize),
    Start,
    End,
    WordBoundary(bool),
    /// Try `.0` first, then `.1`.
    Split(usize, usize),
    Jmp(usize),
    Save(usize),
    Match,
}

/// A compiled expression.
#[derive(Debug, Clone)]
pub struct Regex {
    prog: Vec<Inst>,
    classes: Vec<Class>,
    /// Capture groups, not counting the whole match.
    groups: usize,
}

/// A match: byte offsets of the whole match and of each group (`None` when the group did
/// not take part).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    pub start: usize,
    pub end: usize,
    pub groups: Vec<Option<(usize, usize)>>,
}

impl Match {
    /// The first group that took part, or the whole match.
    pub fn value_range(&self) -> (usize, usize) {
        self.groups.iter().flatten().next().copied().unwrap_or((self.start, self.end))
    }
}

struct Compiler {
    prog: Vec<Inst>,
    classes: Vec<Class>,
}

impl Compiler {
    fn push(&mut self, i: Inst) -> Result<usize, RegexError> {
        if self.prog.len() >= MAX_INSTRUCTIONS {
            return err("the expression is too large");
        }
        self.prog.push(i);
        Ok(self.prog.len() - 1)
    }

    fn compile(&mut self, n: &Node) -> Result<(), RegexError> {
        match n {
            Node::Empty => {}
            Node::Char(c) => {
                self.push(Inst::Char(fold(*c)))?;
            }
            Node::Any => {
                self.push(Inst::Any)?;
            }
            Node::Class(c) => {
                self.classes.push(c.clone());
                self.push(Inst::Class(self.classes.len() - 1))?;
            }
            Node::Start => {
                self.push(Inst::Start)?;
            }
            Node::End => {
                self.push(Inst::End)?;
            }
            Node::WordBoundary(b) => {
                self.push(Inst::WordBoundary(*b))?;
            }
            Node::Group(inner, cap) => {
                if let Some(k) = cap {
                    self.push(Inst::Save(2 * k))?;
                    self.compile(inner)?;
                    self.push(Inst::Save(2 * k + 1))?;
                } else {
                    self.compile(inner)?;
                }
            }
            Node::Concat(items) => {
                for i in items {
                    self.compile(i)?;
                }
            }
            Node::Alt(alts) => {
                let mut jumps = Vec::new();
                for (k, a) in alts.iter().enumerate() {
                    if k + 1 < alts.len() {
                        let split = self.push(Inst::Split(0, 0))?;
                        self.compile(a)?;
                        jumps.push(self.push(Inst::Jmp(0))?);
                        let next = self.prog.len();
                        self.prog[split] = Inst::Split(split + 1, next);
                    } else {
                        self.compile(a)?;
                    }
                }
                let end = self.prog.len();
                for j in jumps {
                    self.prog[j] = Inst::Jmp(end);
                }
            }
            Node::Repeat { node, min, max, greedy } => {
                for _ in 0..*min {
                    self.compile(node)?;
                }
                match max {
                    None => {
                        // x*: L1: split L2, L3; L2: x; jmp L1; L3:
                        let split = self.push(Inst::Split(0, 0))?;
                        self.compile(node)?;
                        self.push(Inst::Jmp(split))?;
                        let end = self.prog.len();
                        self.prog[split] = if *greedy { Inst::Split(split + 1, end) } else { Inst::Split(end, split + 1) };
                    }
                    Some(max) => {
                        let mut splits = Vec::new();
                        for _ in *min..*max {
                            splits.push(self.push(Inst::Split(0, 0))?);
                            self.compile(node)?;
                        }
                        let end = self.prog.len();
                        for s in splits {
                            self.prog[s] = if *greedy { Inst::Split(s + 1, end) } else { Inst::Split(end, s + 1) };
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

impl Regex {
    pub fn new(pattern: &str) -> Result<Regex, RegexError> {
        let mut p = Parser { chars: pattern.chars().peekable(), groups: 0 };
        let node = p.parse_alt(0)?;
        if p.chars.next().is_some() {
            return err("unmatched )");
        }
        Regex::from_node(&node, p.groups)
    }

    pub(crate) fn from_node(node: &Node, groups: usize) -> Result<Regex, RegexError> {
        let mut c = Compiler { prog: Vec::new(), classes: Vec::new() };
        c.compile(node)?;
        c.push(Inst::Match)?;
        Ok(Regex { prog: c.prog, classes: c.classes, groups })
    }

    /// The first match that starts at or after byte `from`.
    pub fn find_at(&self, text: &str, from: usize) -> Option<Match> {
        let slots = 2 * (self.groups + 1);
        let n = self.prog.len();
        let mut clist = Threads::new(n);
        let mut nlist = Threads::new(n);
        let mut matched: Option<Vec<Option<usize>>> = None;
        let mut pos = from;
        let mut prev = text[..from].chars().next_back();
        let mut iter = text[from..].char_indices().map(|(i, c)| (i + from, c)).peekable();
        loop {
            let cur = iter.peek().map(|&(_, c)| c);
            // A new thread may start here, with the lowest priority, until a match is found.
            if matched.is_none() {
                let mut caps = vec![None; slots];
                caps[0] = Some(pos);
                self.add(&mut clist, 0, caps, pos, prev, cur);
            }
            if clist.is_empty() && matched.is_some() {
                break;
            }
            for k in 0..clist.len() {
                let (pc, caps) = (clist.pcs[k], clist.caps[k].clone());
                match &self.prog[pc] {
                    Inst::Match => {
                        let mut caps = caps;
                        caps[1] = Some(pos);
                        matched = Some(caps);
                        // Threads after this one have lower priority: drop them.
                        break;
                    }
                    inst => {
                        let Some(c) = cur else { continue };
                        let ok = match inst {
                            Inst::Char(x) => fold(c) == *x,
                            Inst::Any => c != '\n',
                            Inst::Class(i) => self.classes[*i].matches(c),
                            _ => false,
                        };
                        if ok {
                            let next_pos = pos + c.len_utf8();
                            let after = text[next_pos..].chars().next();
                            self.add(&mut nlist, pc + 1, caps, next_pos, Some(c), after);
                        }
                    }
                }
            }
            let Some((_, c)) = iter.next() else { break };
            prev = Some(c);
            pos += c.len_utf8();
            std::mem::swap(&mut clist, &mut nlist);
            nlist.clear();
        }
        matched.map(|caps| Match {
            start: caps[0].expect("start"),
            end: caps[1].expect("end"),
            groups: (1..=self.groups).map(|g| caps[2 * g].zip(caps[2 * g + 1])).collect(),
        })
    }

    /// All non-overlapping matches, left to right.
    pub fn find_iter<'r, 't>(&'r self, text: &'t str) -> impl Iterator<Item = Match> + 'r
    where
        't: 'r,
    {
        let mut from = 0;
        std::iter::from_fn(move || {
            if from > text.len() {
                return None;
            }
            let m = self.find_at(text, from)?;
            from = if m.end > m.start {
                m.end
            } else {
                // An empty match: continue after the next character.
                m.end + text[m.end..].chars().next().map_or(1, char::len_utf8)
            };
            Some(m)
        })
    }

    pub fn is_match(&self, text: &str) -> bool {
        self.find_at(text, 0).is_some()
    }

    /// Follows jumps, splits, saves and assertions from `pc`, adding the threads that wait
    /// for a character (and `Match`) in priority order.
    fn add(&self, list: &mut Threads, pc: usize, caps: Vec<Option<usize>>, pos: usize, prev: Option<char>, cur: Option<char>) {
        if list.seen[pc] == list.gen {
            return;
        }
        list.seen[pc] = list.gen;
        match &self.prog[pc] {
            Inst::Jmp(t) => self.add(list, *t, caps, pos, prev, cur),
            Inst::Split(a, b) => {
                self.add(list, *a, caps.clone(), pos, prev, cur);
                self.add(list, *b, caps, pos, prev, cur);
            }
            Inst::Save(slot) => {
                let mut caps = caps;
                caps[*slot] = Some(pos);
                self.add(list, pc + 1, caps, pos, prev, cur);
            }
            Inst::Start => {
                if prev.is_none_or(|c| c == '\n') {
                    self.add(list, pc + 1, caps, pos, prev, cur);
                }
            }
            Inst::End => {
                if cur.is_none_or(|c| c == '\n' || c == '\r') {
                    self.add(list, pc + 1, caps, pos, prev, cur);
                }
            }
            Inst::WordBoundary(want) => {
                let edge = prev.is_some_and(is_word) != cur.is_some_and(is_word);
                if edge == *want {
                    self.add(list, pc + 1, caps, pos, prev, cur);
                }
            }
            _ => {
                list.pcs.push(pc);
                list.caps.push(caps);
            }
        }
    }
}

/// The threads of one step, with a generation counter so `seen` needs no clearing.
struct Threads {
    pcs: Vec<usize>,
    caps: Vec<Vec<Option<usize>>>,
    seen: Vec<u32>,
    gen: u32,
}

impl Threads {
    fn new(n: usize) -> Threads {
        Threads { pcs: Vec::new(), caps: Vec::new(), seen: vec![0; n], gen: 1 }
    }
    fn len(&self) -> usize {
        self.pcs.len()
    }
    fn is_empty(&self) -> bool {
        self.pcs.is_empty()
    }
    fn clear(&mut self) {
        self.pcs.clear();
        self.caps.clear();
        self.gen = self.gen.wrapping_add(1);
        if self.gen == 0 {
            self.seen.iter_mut().for_each(|s| *s = 0);
            self.gen = 1;
        }
    }
}
