//! The Bases expression language: lexer, parser and a static check that
//! every function and method an expression calls is one TurboVault
//! evaluates.
//!
//! Grammar, lowest precedence first: `||`, `&&`, `==` `!=`, `<` `<=` `>`
//! `>=`, `+` `-`, `*` `/` `%`, unary `!` `-`, then postfix member access
//! (`a.b`), indexing (`a[0]`, `note["my key"]`) and calls (`f(x)`,
//! `a.m(x)`). Literals are numbers, `'single'` or `"double"` quoted strings,
//! `true`, `false` and `null`.

use std::fmt;

/// A parsed expression.
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Number(f64),
    Str(String),
    Bool(bool),
    Null,
    /// A bare name: a note property, or `file`, `note`, `formula`, `this`,
    /// or a lambda variable (`value`, `index`, `acc`).
    Ident(String),
    Member(Box<Expr>, String),
    Index(Box<Expr>, Box<Expr>),
    /// `name(args)` when `target` is `None`, otherwise `target.name(args)`.
    Call {
        target: Option<Box<Expr>>,
        name: String,
        args: Vec<Expr>,
    },
    Unary(UnaryOp, Box<Expr>),
    Binary(BinaryOp, Box<Expr>, Box<Expr>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Not,
    Neg,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    Or,
    And,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Add,
    Sub,
    Mul,
    Div,
    Rem,
}

impl fmt::Display for BinaryOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Or => "||",
            Self::And => "&&",
            Self::Eq => "==",
            Self::Ne => "!=",
            Self::Lt => "<",
            Self::Le => "<=",
            Self::Gt => ">",
            Self::Ge => ">=",
            Self::Add => "+",
            Self::Sub => "-",
            Self::Mul => "*",
            Self::Div => "/",
            Self::Rem => "%",
        })
    }
}

/// Global functions TurboVault evaluates.
pub const FUNCTIONS: &[&str] = &[
    "if", "now", "today", "date", "duration", "list", "number", "min", "max", "link", "file",
];

/// Methods TurboVault evaluates, on whichever types have them.
pub const METHODS: &[&str] = &[
    // any
    "isTruthy", "isType", "toString",
    // string
    "contains", "containsAll", "containsAny", "startsWith", "endsWith", "isEmpty", "lower",
    "title", "trim", "replace", "repeat", "reverse", "slice", "split",
    // number
    "abs", "ceil", "floor", "round", "toFixed",
    // list
    "filter", "map", "reduce", "join", "sort", "unique", "flat",
    // date
    "date", "format", "time",
    // file
    "hasTag", "inFolder", "hasLink", "hasProperty", "asLink",
    // link
    "asFile", "linksTo",
    // object
    "keys", "values",
];

/// Bases functions and methods TurboVault does not evaluate. Named here so
/// the error can say so rather than calling them unknown.
const UNSUPPORTED: &[&str] = &[
    "escapeHTML", "html", "image", "icon", "random", "relative", "matches",
];

/// Parse `source` into an expression.
pub fn parse(source: &str) -> Result<Expr, String> {
    let tokens = lex(source)?;
    let mut parser = Parser { tokens, pos: 0 };
    let expr = parser.or()?;
    match parser.peek() {
        None => Ok(expr),
        Some(token) => Err(format!("unexpected {token} in {source:?}")),
    }
}

/// Refuse an expression that calls a function or method TurboVault doesn't
/// evaluate, naming it.
pub fn check(expr: &Expr, source: &str) -> Result<(), String> {
    let mut problem = None;
    walk(expr, &mut |node| {
        if problem.is_some() {
            return;
        }
        if let Expr::Call { target, name, .. } = node {
            let known = if target.is_some() {
                METHODS.contains(&name.as_str())
            } else {
                FUNCTIONS.contains(&name.as_str())
            };
            if !known {
                problem = Some(if UNSUPPORTED.contains(&name.as_str()) {
                    format!("{name}() in {source:?} is a Bases function TurboVault doesn't evaluate")
                } else if target.is_some() {
                    format!("{name}() in {source:?} is not a Bases method TurboVault knows")
                } else {
                    format!("{name}() in {source:?} is not a Bases function TurboVault knows")
                });
            }
        }
    });
    problem.map_or(Ok(()), Err)
}

fn walk(expr: &Expr, visit: &mut impl FnMut(&Expr)) {
    visit(expr);
    match expr {
        Expr::Member(inner, _) | Expr::Unary(_, inner) => walk(inner, visit),
        Expr::Index(a, b) | Expr::Binary(_, a, b) => {
            walk(a, visit);
            walk(b, visit);
        }
        Expr::Call { target, args, .. } => {
            if let Some(target) = target {
                walk(target, visit);
            }
            for arg in args {
                walk(arg, visit);
            }
        }
        _ => {}
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Number(f64),
    Str(String),
    Ident(String),
    Op(&'static str),
}

impl fmt::Display for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Number(n) => write!(f, "{n}"),
            Self::Str(s) => write!(f, "{s:?}"),
            Self::Ident(s) => write!(f, "{s}"),
            Self::Op(op) => write!(f, "{op:?}"),
        }
    }
}

const OPERATORS: &[&str] = &[
    "||", "&&", "==", "!=", "<=", ">=", "<", ">", "+", "-", "*", "/", "%", "!", "(", ")", "[",
    "]", ",", ".",
];

fn lex(source: &str) -> Result<Vec<Token>, String> {
    let mut tokens = Vec::new();
    let chars: Vec<char> = source.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
        } else if c.is_ascii_digit() {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                // `1.toFixed` isn't a number with a fraction.
                if chars[i] == '.' && !chars.get(i + 1).is_some_and(char::is_ascii_digit) {
                    break;
                }
                i += 1;
            }
            let text: String = chars[start..i].iter().collect();
            tokens.push(Token::Number(
                text.parse()
                    .map_err(|_| format!("{text:?} is not a number in {source:?}"))?,
            ));
        } else if c == '"' || c == '\'' {
            let mut text = String::new();
            i += 1;
            loop {
                match chars.get(i) {
                    None => return Err(format!("unclosed string in {source:?}")),
                    Some(&q) if q == c => {
                        i += 1;
                        break;
                    }
                    Some('\\') => {
                        let escaped = chars
                            .get(i + 1)
                            .ok_or_else(|| format!("unclosed string in {source:?}"))?;
                        text.push(match escaped {
                            'n' => '\n',
                            't' => '\t',
                            other => *other,
                        });
                        i += 2;
                    }
                    Some(&other) => {
                        text.push(other);
                        i += 1;
                    }
                }
            }
            tokens.push(Token::Str(text));
        } else if c.is_alphabetic() || c == '_' || c == '$' {
            let start = i;
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_' || chars[i] == '$')
            {
                i += 1;
            }
            tokens.push(Token::Ident(chars[start..i].iter().collect()));
        } else {
            let rest: String = chars[i..chars.len().min(i + 2)].iter().collect();
            let op = OPERATORS
                .iter()
                .find(|op| rest.starts_with(**op))
                .ok_or_else(|| format!("unexpected {c:?} in {source:?}"))?;
            tokens.push(Token::Op(op));
            i += op.chars().count();
        }
    }
    Ok(tokens)
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn eat(&mut self, op: &str) -> bool {
        if matches!(self.peek(), Some(Token::Op(o)) if *o == op) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, op: &str) -> Result<(), String> {
        if self.eat(op) {
            Ok(())
        } else {
            Err(match self.peek() {
                Some(token) => format!("expected {op:?} but found {token}"),
                None => format!("expected {op:?} at the end"),
            })
        }
    }

    fn binary(
        &mut self,
        ops: &[(&str, BinaryOp)],
        next: fn(&mut Self) -> Result<Expr, String>,
    ) -> Result<Expr, String> {
        let mut left = next(self)?;
        'outer: loop {
            for (text, op) in ops {
                if self.eat(text) {
                    let right = next(self)?;
                    left = Expr::Binary(*op, Box::new(left), Box::new(right));
                    continue 'outer;
                }
            }
            return Ok(left);
        }
    }

    fn or(&mut self) -> Result<Expr, String> {
        self.binary(&[("||", BinaryOp::Or)], Self::and)
    }

    fn and(&mut self) -> Result<Expr, String> {
        self.binary(&[("&&", BinaryOp::And)], Self::equality)
    }

    fn equality(&mut self) -> Result<Expr, String> {
        self.binary(&[("==", BinaryOp::Eq), ("!=", BinaryOp::Ne)], Self::comparison)
    }

    fn comparison(&mut self) -> Result<Expr, String> {
        self.binary(
            &[
                ("<=", BinaryOp::Le),
                (">=", BinaryOp::Ge),
                ("<", BinaryOp::Lt),
                (">", BinaryOp::Gt),
            ],
            Self::additive,
        )
    }

    fn additive(&mut self) -> Result<Expr, String> {
        self.binary(&[("+", BinaryOp::Add), ("-", BinaryOp::Sub)], Self::multiplicative)
    }

    fn multiplicative(&mut self) -> Result<Expr, String> {
        self.binary(
            &[("*", BinaryOp::Mul), ("/", BinaryOp::Div), ("%", BinaryOp::Rem)],
            Self::unary,
        )
    }

    fn unary(&mut self) -> Result<Expr, String> {
        if self.eat("!") {
            return Ok(Expr::Unary(UnaryOp::Not, Box::new(self.unary()?)));
        }
        if self.eat("-") {
            return Ok(Expr::Unary(UnaryOp::Neg, Box::new(self.unary()?)));
        }
        self.postfix()
    }

    fn args(&mut self) -> Result<Vec<Expr>, String> {
        let mut args = Vec::new();
        if self.eat(")") {
            return Ok(args);
        }
        loop {
            args.push(self.or()?);
            if self.eat(")") {
                return Ok(args);
            }
            self.expect(",")?;
        }
    }

    fn postfix(&mut self) -> Result<Expr, String> {
        let mut expr = self.primary()?;
        loop {
            if self.eat(".") {
                let name = match self.tokens.get(self.pos).cloned() {
                    Some(Token::Ident(name)) => {
                        self.pos += 1;
                        name
                    }
                    other => {
                        return Err(match other {
                            Some(token) => format!("expected a name after \".\" but found {token}"),
                            None => "expected a name after \".\"".to_string(),
                        });
                    }
                };
                if self.eat("(") {
                    expr = Expr::Call {
                        target: Some(Box::new(expr)),
                        name,
                        args: self.args()?,
                    };
                } else {
                    expr = Expr::Member(Box::new(expr), name);
                }
            } else if self.eat("[") {
                let index = self.or()?;
                self.expect("]")?;
                expr = Expr::Index(Box::new(expr), Box::new(index));
            } else if self.eat("(") {
                let Expr::Ident(name) = expr else {
                    return Err("only a function name can be called".to_string());
                };
                expr = Expr::Call {
                    target: None,
                    name,
                    args: self.args()?,
                };
            } else {
                return Ok(expr);
            }
        }
    }

    fn primary(&mut self) -> Result<Expr, String> {
        let token = self
            .tokens
            .get(self.pos)
            .cloned()
            .ok_or_else(|| "the expression ends too soon".to_string())?;
        self.pos += 1;
        match token {
            Token::Number(n) => Ok(Expr::Number(n)),
            Token::Str(s) => Ok(Expr::Str(s)),
            Token::Ident(name) => Ok(match name.as_str() {
                "true" => Expr::Bool(true),
                "false" => Expr::Bool(false),
                "null" => Expr::Null,
                _ => Expr::Ident(name),
            }),
            Token::Op("(") => {
                let inner = self.or()?;
                self.expect(")")?;
                Ok(inner)
            }
            Token::Op(op) => Err(format!("unexpected {op:?}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ident(name: &str) -> Box<Expr> {
        Box::new(Expr::Ident(name.into()))
    }

    #[test]
    fn precedence_and_postfix() {
        let expr = parse("status != \"done\" && file.hasTag('a', \"b\") || !x").unwrap();
        let Expr::Binary(BinaryOp::Or, left, right) = expr else {
            panic!("{expr:?}")
        };
        assert_eq!(*right, Expr::Unary(UnaryOp::Not, ident("x")));
        let Expr::Binary(BinaryOp::And, eq, call) = *left else {
            panic!()
        };
        assert_eq!(
            *eq,
            Expr::Binary(BinaryOp::Ne, ident("status"), Box::new(Expr::Str("done".into())))
        );
        assert_eq!(
            *call,
            Expr::Call {
                target: Some(ident("file")),
                name: "hasTag".into(),
                args: vec![Expr::Str("a".into()), Expr::Str("b".into())],
            }
        );
    }

    #[test]
    fn arithmetic_binds_tighter_than_comparison() {
        assert_eq!(
            parse("1 + 2 * 3 > 6").unwrap(),
            Expr::Binary(
                BinaryOp::Gt,
                Box::new(Expr::Binary(
                    BinaryOp::Add,
                    Box::new(Expr::Number(1.0)),
                    Box::new(Expr::Binary(
                        BinaryOp::Mul,
                        Box::new(Expr::Number(2.0)),
                        Box::new(Expr::Number(3.0))
                    ))
                )),
                Box::new(Expr::Number(6.0))
            )
        );
        assert_eq!(
            parse("price.toFixed(2)").unwrap(),
            Expr::Call {
                target: Some(ident("price")),
                name: "toFixed".into(),
                args: vec![Expr::Number(2.0)],
            }
        );
        assert!(matches!(parse("note[\"my key\"]").unwrap(), Expr::Index(..)));
    }

    #[test]
    fn errors_say_where() {
        assert!(parse("a +").unwrap_err().contains("ends too soon"));
        assert!(parse("'open").unwrap_err().contains("unclosed"));
        assert!(parse("a b").unwrap_err().contains("unexpected b"));
    }

    #[test]
    fn check_names_unsupported_calls() {
        let source = "file.name.matches(x)";
        let error = check(&parse(source).unwrap(), source).unwrap_err();
        assert!(error.contains("matches()") && error.contains("doesn't evaluate"), "{error}");
        let source = "frobnicate(1)";
        let error = check(&parse(source).unwrap(), source).unwrap_err();
        assert!(error.contains("not a Bases function"), "{error}");
        let source = "list(tags).filter(value.contains(\"a\")).length > 0";
        check(&parse(source).unwrap(), source).unwrap();
    }
}
