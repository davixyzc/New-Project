//! Tokenizer: turns fly source text into a flat list of tokens.

/// A single lexical token.
#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    Num(f64),
    Str(String),
    /// A template literal. `quasis` always has one more entry than `exprs`.
    /// Each substitution keeps its raw source and the line where it starts.
    Template {
        quasis: Vec<String>,
        exprs: Vec<(String, u32)>,
    },
    Ident(String),
    Punct(&'static str),
    Eof,
}

#[derive(Debug, Clone)]
pub struct Token {
    pub tok: Tok,
    pub line: u32,
    /// True when a line break separates this token from the previous one.
    pub nl_before: bool,
}

/// Longest punctuators first so that greedy matching works.
const PUNCTS: &[&str] = &[
    "...", "===", "!==", "**=", "**", "=>", "==", "!=", "<=", ">=", "&&", "||", "??", "++", "--",
    "+=", "-=", "*=", "/=", "%=", "(", ")", "{", "}", "[", "]", ";", ",", ".", ":", "?", "=", "<",
    ">", "+", "-", "*", "/", "%", "!",
];

pub fn tokenize(src: &str) -> Result<Vec<Token>, String> {
    let mut lx = Lexer {
        chars: src.chars().collect(),
        pos: 0,
        line: 1,
    };
    if src.starts_with("#!") {
        while lx.pos < lx.chars.len() && lx.chars[lx.pos] != '\n' {
            lx.pos += 1;
        }
    }
    let mut out = Vec::new();
    loop {
        let nl_before = lx.skip_space()?;
        let line = lx.line;
        if lx.pos >= lx.chars.len() {
            out.push(Token {
                tok: Tok::Eof,
                line,
                nl_before: true,
            });
            return Ok(out);
        }
        let tok = lx.next_tok()?;
        out.push(Token {
            tok,
            line,
            nl_before,
        });
    }
}

struct Lexer {
    chars: Vec<char>,
    pos: usize,
    line: u32,
}

impl Lexer {
    fn peek(&self, off: usize) -> char {
        self.chars.get(self.pos + off).copied().unwrap_or('\0')
    }

    fn bump(&mut self) -> char {
        let c = self.peek(0);
        if self.pos < self.chars.len() {
            self.pos += 1;
        }
        if c == '\n' {
            self.line += 1;
        }
        c
    }

    fn err(&self, msg: &str) -> String {
        format!("SyntaxError: {} at line {}", msg, self.line)
    }

    /// Skips whitespace and comments. Returns true if a line break was crossed.
    fn skip_space(&mut self) -> Result<bool, String> {
        let mut nl = false;
        loop {
            let c = self.peek(0);
            if c == '\n' {
                nl = true;
                self.bump();
            } else if c.is_whitespace() {
                self.bump();
            } else if c == '/' && self.peek(1) == '/' {
                while self.pos < self.chars.len() && self.peek(0) != '\n' {
                    self.bump();
                }
            } else if c == '/' && self.peek(1) == '*' {
                self.bump();
                self.bump();
                loop {
                    if self.pos >= self.chars.len() {
                        return Err(self.err("unterminated comment"));
                    }
                    if self.peek(0) == '*' && self.peek(1) == '/' {
                        self.bump();
                        self.bump();
                        break;
                    }
                    if self.bump() == '\n' {
                        nl = true;
                    }
                }
            } else {
                return Ok(nl);
            }
        }
    }

    fn next_tok(&mut self) -> Result<Tok, String> {
        let c = self.peek(0);
        if c.is_ascii_digit() || (c == '.' && self.peek(1).is_ascii_digit()) {
            return self.number();
        }
        if c == '"' || c == '\'' {
            return self.string(c);
        }
        if c == '`' {
            return self.template();
        }
        if is_ident_start(c) {
            let start = self.pos;
            while is_ident_part(self.peek(0)) {
                self.pos += 1;
            }
            return Ok(Tok::Ident(self.chars[start..self.pos].iter().collect()));
        }
        for &p in PUNCTS {
            if p.chars().enumerate().all(|(i, ch)| self.peek(i) == ch) {
                self.pos += p.len();
                return Ok(Tok::Punct(p));
            }
        }
        Err(self.err(&format!("unexpected character '{}'", c)))
    }

    fn number(&mut self) -> Result<Tok, String> {
        if self.peek(0) == '0' && matches!(self.peek(1), 'x' | 'X') {
            self.pos += 2;
            let start = self.pos;
            while self.peek(0).is_ascii_hexdigit() {
                self.pos += 1;
            }
            let digits: String = self.chars[start..self.pos].iter().collect();
            return u64::from_str_radix(&digits, 16)
                .map(|n| Tok::Num(n as f64))
                .map_err(|_| self.err("invalid hex literal"));
        }
        let start = self.pos;
        while self.peek(0).is_ascii_digit() {
            self.pos += 1;
        }
        if self.peek(0) == '.' {
            self.pos += 1;
            while self.peek(0).is_ascii_digit() {
                self.pos += 1;
            }
        }
        let after_e = self.peek(1);
        let exp_ok = matches!(self.peek(0), 'e' | 'E')
            && (after_e.is_ascii_digit()
                || (matches!(after_e, '+' | '-') && self.peek(2).is_ascii_digit()));
        if exp_ok {
            self.pos += 2;
            while self.peek(0).is_ascii_digit() {
                self.pos += 1;
            }
        }
        let mut text: String = self.chars[start..self.pos].iter().collect();
        if text.ends_with('.') {
            text.push('0');
        }
        text.parse::<f64>()
            .map(Tok::Num)
            .map_err(|_| self.err("invalid number"))
    }

    fn string(&mut self, quote: char) -> Result<Tok, String> {
        self.bump();
        let mut out = String::new();
        loop {
            if self.pos >= self.chars.len() || self.peek(0) == '\n' {
                return Err(self.err("unterminated string"));
            }
            let c = self.bump();
            if c == quote {
                return Ok(Tok::Str(out));
            }
            if c == '\\' {
                self.escape(&mut out)?;
            } else {
                out.push(c);
            }
        }
    }

    /// Handles one escape sequence; the backslash has already been consumed.
    fn escape(&mut self, out: &mut String) -> Result<(), String> {
        let c = self.bump();
        match c {
            'n' => out.push('\n'),
            't' => out.push('\t'),
            'r' => out.push('\r'),
            'b' => out.push('\u{8}'),
            'f' => out.push('\u{c}'),
            'v' => out.push('\u{b}'),
            '0' => out.push('\0'),
            '\n' => {}
            'x' => {
                let hex: String = (0..2).map(|_| self.bump()).collect();
                let code =
                    u32::from_str_radix(&hex, 16).map_err(|_| self.err("invalid \\x escape"))?;
                out.push(char::from_u32(code).unwrap_or('\u{fffd}'));
            }
            'u' => {
                let hex: String = if self.peek(0) == '{' {
                    self.bump();
                    let mut h = String::new();
                    while self.peek(0) != '}' && self.pos < self.chars.len() {
                        h.push(self.bump());
                    }
                    self.bump();
                    h
                } else {
                    (0..4).map(|_| self.bump()).collect()
                };
                let code =
                    u32::from_str_radix(&hex, 16).map_err(|_| self.err("invalid \\u escape"))?;
                out.push(char::from_u32(code).unwrap_or('\u{fffd}'));
            }
            other => out.push(other),
        }
        Ok(())
    }

    fn template(&mut self) -> Result<Tok, String> {
        self.bump();
        let mut quasis = Vec::new();
        let mut exprs = Vec::new();
        let mut cur = String::new();
        loop {
            if self.pos >= self.chars.len() {
                return Err(self.err("unterminated template literal"));
            }
            match self.bump() {
                '`' => break,
                '\\' => self.escape(&mut cur)?,
                '$' if self.peek(0) == '{' => {
                    self.bump();
                    let line = self.line;
                    let src = self.expr_source()?;
                    quasis.push(std::mem::take(&mut cur));
                    exprs.push((src, line));
                }
                c => cur.push(c),
            }
        }
        quasis.push(cur);
        Ok(Tok::Template { quasis, exprs })
    }

    /// Reads the source of a `${...}` substitution, up to and excluding its closing brace.
    fn expr_source(&mut self) -> Result<String, String> {
        let start = self.pos;
        let mut depth = 0usize;
        loop {
            if self.pos >= self.chars.len() {
                return Err(self.err("unterminated template substitution"));
            }
            match self.bump() {
                '{' => depth += 1,
                '}' if depth == 0 => return Ok(self.chars[start..self.pos - 1].iter().collect()),
                '}' => depth -= 1,
                q @ ('"' | '\'' | '`') => {
                    while self.pos < self.chars.len() && self.peek(0) != q {
                        if self.bump() == '\\' {
                            self.bump();
                        }
                    }
                    self.bump();
                }
                _ => {}
            }
        }
    }
}

fn is_ident_start(c: char) -> bool {
    c.is_alphabetic() || c == '_' || c == '$'
}

fn is_ident_part(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}
