//! Inline calculator via `meval` expression evaluation.

/// Returns `Some(result)` when `s` looks like a math expression and evaluates
/// cleanly, `None` otherwise.
pub fn evaluate(s: &str) -> Option<f64> {
    let s = s.trim();
    if s.is_empty() || !looks_like_expression(s) {
        return None;
    }
    match meval::eval_str(s) {
        Ok(val) if val.is_finite() => Some(val),
        _ => None,
    }
}

/// Heuristic: the string must start with a digit, `(`, or `-` followed by a
/// digit, and must contain at least one arithmetic operator.
fn looks_like_expression(s: &str) -> bool {
    let starts_ok = s.starts_with(|c: char| c.is_ascii_digit() || c == '(')
        || (s.starts_with('-') && s.chars().nth(1).is_some_and(|c| c.is_ascii_digit()));

    let has_operator = s.chars().any(|c| "+-*/^%".contains(c));

    starts_ok && has_operator
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_addition() {
        let result = evaluate("2 + 2");
        assert_eq!(result, Some(4.0));
    }

    #[test]
    fn complex_expression() {
        let result = evaluate("(3 + 5) * 2");
        assert_eq!(result, Some(16.0));
    }

    #[test]
    fn non_expression_returns_none() {
        assert!(evaluate("firefox").is_none());
    }

    #[test]
    fn empty_returns_none() {
        assert!(evaluate("").is_none());
    }

    #[test]
    fn plain_number_no_operator_returns_none() {
        assert!(evaluate("42").is_none());
    }

    #[test]
    fn division() {
        let r = evaluate("10 / 4");
        assert_eq!(r, Some(2.5));
    }
}
