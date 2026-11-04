//! Lua source for values: what the macro recorder writes.

/// `s` as a Lua string literal ("…", escapes for quotes, backslashes and
/// control characters; other characters as they are — Lua strings are
/// bytes, and the file is UTF-8).
pub fn string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\{:03}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// `x` as a Lua number: the shortest text that reads back as `x`
/// ("3", "0.25", "-1e-07"); NaN and infinities as expressions.
pub fn number(x: f64) -> String {
    if x.is_nan() {
        "(0/0)".into()
    } else if x.is_infinite() {
        if x > 0.0 { "math.huge" } else { "-math.huge" }.into()
    } else if x == x.trunc() && x.abs() < 1e15 {
        // Whole numbers read as Lua integers (positions, ids, counts).
        format!("{}", x as i64)
    } else {
        format!("{x}")
    }
}

/// Whether `s` can be a table key without brackets (`{ name = … }`).
pub fn is_name(s: &str) -> bool {
    const RESERVED: [&str; 22] = [
        "and", "break", "do", "else", "elseif", "end", "false", "for", "function", "goto", "if",
        "in", "local", "nil", "not", "or", "repeat", "return", "then", "true", "until", "while",
    ];
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c == '_' || c.is_ascii_alphabetic())
        && chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
        && !RESERVED.contains(&s)
}

/// A table constructor of `(key, value source)` pairs:
/// `{ volume = -3, mute = true }`.
pub fn table(fields: &[(&str, String)]) -> String {
    if fields.is_empty() {
        return "{}".into();
    }
    let parts: Vec<String> = fields
        .iter()
        .map(|(k, v)| {
            if is_name(k) {
                format!("{k} = {v}")
            } else {
                format!("[{}] = {v}", string(k))
            }
        })
        .collect();
    format!("{{ {} }}", parts.join(", "))
}

/// A list constructor: `{ 1, 2, 3 }`.
pub fn list(items: &[String]) -> String {
    if items.is_empty() {
        "{}".into()
    } else {
        format!("{{ {} }}", items.join(", "))
    }
}
