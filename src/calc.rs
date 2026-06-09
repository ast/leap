//! A tiny dependency-free arithmetic evaluator for the Canon Cat's inline Calc.
//!
//! Supports `+ - * / %`, parentheses, unary `+`/`-`, and decimals, all over
//! `f64`. Recursive-descent with the usual precedence (`* / %` bind tighter than
//! `+ -`). Errors are human-readable strings shown on the echo line.

/// Evaluate an arithmetic expression. Whitespace is ignored.
pub fn eval(input: &str) -> Result<f64, String> {
    let tokens = tokenize(input)?;
    let mut parser = Parser { tokens: &tokens, pos: 0 };
    let value = parser.expr()?;
    if parser.pos != parser.tokens.len() {
        return Err("unexpected trailing input".into());
    }
    Ok(value)
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Tok {
    Num(f64),
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    LParen,
    RParen,
}

fn tokenize(s: &str) -> Result<Vec<Tok>, String> {
    let mut out = Vec::new();
    let mut chars = s.chars().peekable();
    while let Some(&c) = chars.peek() {
        match c {
            ' ' | '\t' => {
                chars.next();
            }
            '+' => {
                chars.next();
                out.push(Tok::Plus);
            }
            '-' => {
                chars.next();
                out.push(Tok::Minus);
            }
            '*' | '×' => {
                chars.next();
                out.push(Tok::Star);
            }
            '/' | '÷' => {
                chars.next();
                out.push(Tok::Slash);
            }
            '%' => {
                chars.next();
                out.push(Tok::Percent);
            }
            '(' => {
                chars.next();
                out.push(Tok::LParen);
            }
            ')' => {
                chars.next();
                out.push(Tok::RParen);
            }
            '0'..='9' | '.' => {
                let mut num = String::new();
                while let Some(&d) = chars.peek() {
                    if d.is_ascii_digit() || d == '.' {
                        num.push(d);
                        chars.next();
                    } else {
                        break;
                    }
                }
                let value: f64 = num.parse().map_err(|_| format!("bad number {num:?}"))?;
                out.push(Tok::Num(value));
            }
            _ => return Err(format!("unexpected character {c:?}")),
        }
    }
    Ok(out)
}

struct Parser<'a> {
    tokens: &'a [Tok],
    pos: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<Tok> {
        self.tokens.get(self.pos).copied()
    }

    fn bump(&mut self) -> Option<Tok> {
        let tok = self.peek();
        if tok.is_some() {
            self.pos += 1;
        }
        tok
    }

    /// `term (('+' | '-') term)*`
    fn expr(&mut self) -> Result<f64, String> {
        let mut value = self.term()?;
        while let Some(tok) = self.peek() {
            match tok {
                Tok::Plus => {
                    self.bump();
                    value += self.term()?;
                }
                Tok::Minus => {
                    self.bump();
                    value -= self.term()?;
                }
                _ => break,
            }
        }
        Ok(value)
    }

    /// `factor (('*' | '/' | '%') factor)*`
    fn term(&mut self) -> Result<f64, String> {
        let mut value = self.factor()?;
        while let Some(tok) = self.peek() {
            match tok {
                Tok::Star => {
                    self.bump();
                    value *= self.factor()?;
                }
                Tok::Slash => {
                    self.bump();
                    let d = self.factor()?;
                    if d == 0.0 {
                        return Err("divide by zero".into());
                    }
                    value /= d;
                }
                Tok::Percent => {
                    self.bump();
                    let d = self.factor()?;
                    if d == 0.0 {
                        return Err("divide by zero".into());
                    }
                    value %= d;
                }
                _ => break,
            }
        }
        Ok(value)
    }

    /// `number | '(' expr ')' | ('+' | '-') factor`
    fn factor(&mut self) -> Result<f64, String> {
        match self.bump() {
            Some(Tok::Num(n)) => Ok(n),
            Some(Tok::Minus) => Ok(-self.factor()?),
            Some(Tok::Plus) => self.factor(),
            Some(Tok::LParen) => {
                let value = self.expr()?;
                match self.bump() {
                    Some(Tok::RParen) => Ok(value),
                    _ => Err("expected ')'".into()),
                }
            }
            Some(_) => Err("expected a number".into()),
            None => Err("unexpected end of expression".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::eval;

    fn ok(s: &str) -> f64 {
        eval(s).unwrap()
    }

    #[test]
    fn precedence_and_parens() {
        assert_eq!(ok("2 + 2"), 4.0);
        assert_eq!(ok("2 + 3 * 4"), 14.0);
        assert_eq!(ok("12 * (3 + 4)"), 84.0);
        assert_eq!(ok("(1 + 2) * (3 + 4)"), 21.0);
    }

    #[test]
    fn division_modulo_decimals() {
        assert_eq!(ok("10 / 4"), 2.5);
        assert_eq!(ok("10 % 3"), 1.0);
        assert!((ok("0.1 + 0.2") - 0.3).abs() < 1e-9);
    }

    #[test]
    fn unary_minus() {
        assert_eq!(ok("-3 + 5"), 2.0);
        assert_eq!(ok("-(2 * 3)"), -6.0);
        assert_eq!(ok("2 * -3"), -6.0);
    }

    #[test]
    fn errors() {
        assert!(eval("2 +").is_err());
        assert!(eval("(1 + 2").is_err());
        assert!(eval("1 / 0").is_err());
        assert!(eval("2 3").is_err());
        assert!(eval("abc").is_err());
        assert!(eval("").is_err());
    }
}
