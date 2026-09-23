//! Restricted CEL option maps. No functions, member access or external IO.
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Map, Value};

#[derive(Clone, Debug, PartialEq)]
enum Token {
    Literal(Value),
    Name(String),
    Op(String),
    End,
}
enum Expr {
    Literal(Value),
    Input,
    Array(Vec<Expr>),
    Object(Vec<(String, Expr)>),
    Unary(String, Box<Expr>),
    Binary(String, Box<Expr>, Box<Expr>),
    Conditional(Box<Expr>, Box<Expr>, Box<Expr>),
}

pub fn evaluate(source: &str, variable: &str, input: &Value) -> Result<Value> {
    ensure!(
        source.len() <= 65536 && (input.is_string() || input.is_number()),
        "Invalid option map"
    );
    let mut parser = Parser {
        tokens: tokenize(source)?,
        index: 0,
        variable,
        depth: 0,
    };
    let expr = parser.expression(0)?;
    ensure!(
        parser.current() == &Token::End && object_result(&expr),
        "Option map must return an object"
    );
    eval(&expr, input)
}
fn object_result(e: &Expr) -> bool {
    match e {
        Expr::Object(_) => true,
        Expr::Conditional(_, a, b) => object_result(a) && object_result(b),
        _ => false,
    }
}
fn tokenize(source: &str) -> Result<Vec<Token>> {
    let chars: Vec<char> = source.chars().collect();
    let mut i = 0;
    let mut out = Vec::new();
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if c == '\'' || c == '"' {
            i += 1;
            let mut value = String::new();
            while i < chars.len() && chars[i] != c {
                if chars[i] == '\\' {
                    i += 1;
                    let e = *chars.get(i).context("Invalid string escape")?;
                    match e {
                        'n' => value.push('\n'),
                        'r' => value.push('\r'),
                        't' => value.push('\t'),
                        'b' => value.push('\u{8}'),
                        'f' => value.push('\u{c}'),
                        '\\' | '\'' | '"' | '/' => value.push(e),
                        'u' => {
                            let digits: String = chars
                                .get(i + 1..i + 5)
                                .context("Invalid unicode escape")?
                                .iter()
                                .collect();
                            let first = u16::from_str_radix(&digits, 16)?;
                            i += 4;
                            let mut units = vec![first];
                            if (0xd800..=0xdbff).contains(&first) {
                                ensure!(
                                    chars.get(i + 1) == Some(&'\\')
                                        && chars.get(i + 2) == Some(&'u'),
                                    "Invalid surrogate"
                                );
                                let digits: String = chars
                                    .get(i + 3..i + 7)
                                    .context("Invalid surrogate")?
                                    .iter()
                                    .collect();
                                units.push(u16::from_str_radix(&digits, 16)?);
                                i += 6;
                            }
                            value.push_str(&String::from_utf16(&units)?);
                        }
                        _ => bail!("Invalid string escape"),
                    }
                } else {
                    ensure!(chars[i] >= ' ', "Invalid string control");
                    value.push(chars[i]);
                }
                i += 1;
            }
            ensure!(chars.get(i) == Some(&c), "Unterminated string");
            i += 1;
            out.push(Token::Literal(Value::String(value)));
        } else if c.is_ascii_digit() {
            let start = i;
            i += 1;
            while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                i += 1;
            }
            if matches!(chars.get(i), Some('e' | 'E')) {
                i += 1;
                if matches!(chars.get(i), Some('+' | '-')) {
                    i += 1;
                }
                while chars.get(i).is_some_and(char::is_ascii_digit) {
                    i += 1;
                }
            }
            let text: String = chars[start..i].iter().collect();
            out.push(Token::Literal(number(text.parse()?)?));
        } else if c.is_ascii_alphabetic() || c == '_' {
            let start = i;
            i += 1;
            while chars
                .get(i)
                .is_some_and(|c| c.is_ascii_alphanumeric() || *c == '_')
            {
                i += 1;
            }
            out.push(Token::Name(chars[start..i].iter().collect()));
        } else {
            let pair: String = chars[i..chars.len().min(i + 2)].iter().collect();
            if ["==", "!=", "<=", ">=", "&&", "||"].contains(&pair.as_str()) {
                out.push(Token::Op(pair));
                i += 2;
            } else {
                ensure!(
                    "{}[]():,?!+-*/%<>".contains(c),
                    "Unsupported option map token"
                );
                out.push(Token::Op(c.to_string()));
                i += 1;
            }
        }
        ensure!(out.len() <= 2048, "Option map exceeds token budget");
    }
    out.push(Token::End);
    Ok(out)
}
struct Parser<'a> {
    tokens: Vec<Token>,
    index: usize,
    variable: &'a str,
    depth: usize,
}
impl Parser<'_> {
    fn current(&self) -> &Token {
        &self.tokens[self.index]
    }
    fn take(&mut self) -> Token {
        let t = self.current().clone();
        if t != Token::End {
            self.index += 1;
        }
        t
    }
    fn consume(&mut self, op: &str) -> bool {
        if self.current() == &Token::Op(op.into()) {
            self.index += 1;
            true
        } else {
            false
        }
    }
    fn expect(&mut self, op: &str) -> Result<()> {
        ensure!(self.consume(op), "Invalid option map delimiter");
        Ok(())
    }
    fn expression(&mut self, min: u8) -> Result<Expr> {
        self.depth += 1;
        ensure!(self.depth <= 64, "Option map nesting limit");
        let mut left = match self.take() {
            Token::Literal(v) => Expr::Literal(v),
            Token::Name(n) if n == self.variable => Expr::Input,
            Token::Name(n) if ["true", "false", "null"].contains(&n.as_str()) => {
                Expr::Literal(serde_json::from_str(&n)?)
            }
            Token::Op(op) if ["!", "-", "+"].contains(&op.as_str()) => {
                Expr::Unary(op, Box::new(self.expression(8)?))
            }
            Token::Op(op) if op == "(" => {
                let e = self.expression(0)?;
                self.expect(")")?;
                e
            }
            Token::Op(op) if op == "[" => {
                let mut values = vec![];
                if !self.consume("]") {
                    loop {
                        values.push(self.expression(0)?);
                        if !self.consume(",") {
                            break;
                        }
                    }
                    self.expect("]")?;
                }
                Expr::Array(values)
            }
            Token::Op(op) if op == "{" => {
                let mut entries = vec![];
                if !self.consume("}") {
                    loop {
                        let Token::Literal(Value::String(key)) = self.take() else {
                            bail!("Object keys must be strings")
                        };
                        ensure!(
                            !entries.iter().any(|(k, _)| k == &key),
                            "Duplicate option map key"
                        );
                        self.expect(":")?;
                        entries.push((key, self.expression(0)?));
                        if !self.consume(",") {
                            break;
                        }
                    }
                    self.expect("}")?;
                }
                Expr::Object(entries)
            }
            _ => bail!("Invalid option map expression"),
        };
        while let Token::Op(op) = self.current() {
            let prec = match op.as_str() {
                "||" => 1,
                "&&" => 2,
                "==" | "!=" => 3,
                "<" | "<=" | ">" | ">=" => 4,
                "+" | "-" => 5,
                "*" | "/" | "%" => 6,
                _ => break,
            };
            if prec < min {
                break;
            }
            let op = op.clone();
            self.take();
            left = Expr::Binary(op, Box::new(left), Box::new(self.expression(prec + 1)?));
        }
        if min == 0 && self.consume("?") {
            let yes = self.expression(0)?;
            self.expect(":")?;
            let no = self.expression(0)?;
            left = Expr::Conditional(Box::new(left), Box::new(yes), Box::new(no));
        }
        self.depth -= 1;
        Ok(left)
    }
}
fn number(n: f64) -> Result<Value> {
    ensure!(
        n.is_finite() && (n.fract() != 0.0 || n.abs() <= 9_007_199_254_740_991.0),
        "Number is not JSON safe"
    );
    Ok(if n.fract() == 0.0 {
        Value::from(n as i64)
    } else {
        Value::from(n)
    })
}
fn boolean(v: &Value) -> Result<bool> {
    v.as_bool().context("Boolean operand required")
}
fn numeric(v: &Value) -> Result<f64> {
    v.as_f64().context("Numeric operand required")
}
fn eval(e: &Expr, input: &Value) -> Result<Value> {
    Ok(match e {
        Expr::Literal(v) => v.clone(),
        Expr::Input => input.clone(),
        Expr::Array(es) => Value::Array(es.iter().map(|e| eval(e, input)).collect::<Result<_>>()?),
        Expr::Object(es) => Value::Object(
            es.iter()
                .map(|(k, e)| Ok((k.clone(), eval(e, input)?)))
                .collect::<Result<Map<_, _>>>()?,
        ),
        Expr::Conditional(c, a, b) => eval(if boolean(&eval(c, input)?)? { a } else { b }, input)?,
        Expr::Unary(op, e) => {
            let v = eval(e, input)?;
            if op == "!" {
                (!boolean(&v)?).into()
            } else {
                number(numeric(&v)? * if op == "-" { -1.0 } else { 1.0 })?
            }
        }
        Expr::Binary(op, a, b) => {
            let a = eval(a, input)?;
            match op.as_str() {
                "&&" => (boolean(&a)? && boolean(&eval(b, input)?)?).into(),
                "||" => (boolean(&a)? || boolean(&eval(b, input)?)?).into(),
                _ => {
                    let b = eval(b, input)?;
                    match op.as_str() {
                        "==" => (a == b).into(),
                        "!=" => (a != b).into(),
                        "+" if a.is_string() && b.is_string() => {
                            format!("{}{}", a.as_str().unwrap(), b.as_str().unwrap()).into()
                        }
                        "<" | "<=" | ">" | ">=" => {
                            let cmp = if let (Some(a), Some(b)) = (a.as_str(), b.as_str()) {
                                a.encode_utf16().cmp(b.encode_utf16())
                            } else {
                                numeric(&a)?
                                    .partial_cmp(&numeric(&b)?)
                                    .context("Invalid comparison")?
                            };
                            match op.as_str() {
                                "<" => cmp.is_lt(),
                                "<=" => !cmp.is_gt(),
                                ">" => cmp.is_gt(),
                                _ => !cmp.is_lt(),
                            }
                            .into()
                        }
                        _ => {
                            let a = numeric(&a)?;
                            let b = numeric(&b)?;
                            number(match op.as_str() {
                                "+" => a + b,
                                "-" => a - b,
                                "*" => a * b,
                                "/" => a / b,
                                "%" => a % b,
                                _ => unreachable!(),
                            })?
                        }
                    }
                }
            }
        }
    })
}
pub fn merge_patch(target: &mut Value, patch: &Value) {
    if let Some(patch) = patch.as_object() {
        if !target.is_object() {
            *target = Value::Object(Map::new());
        }
        for (key, v) in patch {
            if v.is_null() {
                target.as_object_mut().unwrap().remove(key);
            } else {
                merge_patch(&mut target[key], v);
            }
        }
    } else {
        *target = patch.clone();
    }
}
pub fn validate_patches(patches: &[Value]) -> Result<()> {
    fn paths(v: &Value, prefix: Vec<String>, out: &mut Vec<Vec<String>>) {
        if let Some(o) = v.as_object().filter(|o| !o.is_empty()) {
            for (k, v) in o {
                let mut p = prefix.clone();
                p.push(k.clone());
                paths(v, p, out);
            }
        } else {
            out.push(prefix);
        }
    }
    let mut written: Vec<Vec<String>> = vec![];
    for patch in patches {
        let mut current = vec![];
        ensure!(patch.is_object(), "Option map must return object");
        if patch.as_object().unwrap().is_empty() {
            continue;
        }
        paths(patch, vec![], &mut current);
        for p in &current {
            ensure!(
                !written.iter().any(|w| w.starts_with(p) || p.starts_with(w)),
                "Conflicting option maps"
            );
        }
        written.extend(current);
    }
    Ok(())
}
