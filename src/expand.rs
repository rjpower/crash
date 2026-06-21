//! Word expansion: quoting, parameter/variable expansion, command & arithmetic
//! substitution, tilde, word-splitting and pathname globbing.

use crate::interp::Interp;
use crate::vfs::resolve_against;

/// A piece of an expanding word, tracking whether it came from a quoted context
/// (quoted text is never word-split or glob-expanded).
struct Part {
    text: String,
    quoted: bool,
    has_glob: bool,
}

pub fn expand_word(interp: &mut Interp, word: &str, do_split_glob: bool) -> Vec<String> {
    let parts = expand_to_parts(interp, word);
    assemble(interp, parts, do_split_glob)
}

/// Expand a list of words (a command's argv) into the final field list.
pub fn expand_words(interp: &mut Interp, words: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for w in words {
        out.extend(expand_word(interp, w, true));
    }
    out
}

fn assemble(interp: &mut Interp, parts: Vec<Part>, do_split_glob: bool) -> Vec<String> {
    let ifs = interp.get_var("IFS").unwrap_or_else(|| " \t\n".to_string());
    let mut fields: Vec<(String, bool)> = Vec::new(); // (text, has_unquoted_glob)
    let mut cur = String::new();
    let mut cur_glob = false;
    let mut started = false;
    for p in parts {
        if p.quoted || !do_split_glob {
            cur.push_str(&p.text);
            started = true;
            cur_glob |= p.has_glob && !p.quoted;
        } else {
            // split unquoted text on IFS
            let mut chars = p.text.chars().peekable();
            while let Some(c) = chars.next() {
                if ifs.contains(c) {
                    if started {
                        fields.push((std::mem::take(&mut cur), cur_glob));
                        cur_glob = false;
                        started = false;
                    }
                } else {
                    cur.push(c);
                    started = true;
                }
            }
            cur_glob |= p.has_glob;
        }
    }
    if started {
        fields.push((cur, cur_glob));
    }
    // globbing
    let mut out = Vec::new();
    for (f, glob) in fields {
        if do_split_glob && glob {
            let matches = glob_vfs(interp, &f);
            if matches.is_empty() {
                out.push(f);
            } else {
                out.extend(matches);
            }
        } else {
            out.push(f);
        }
    }
    out
}

fn expand_to_parts(interp: &mut Interp, word: &str) -> Vec<Part> {
    let chars: Vec<char> = word.chars().collect();
    let mut i = 0;
    let mut parts: Vec<Part> = Vec::new();
    let mut buf = String::new();
    let mut buf_glob = false;
    macro_rules! flush_unquoted {
        () => {
            if !buf.is_empty() {
                parts.push(Part { text: std::mem::take(&mut buf), quoted: false, has_glob: buf_glob });
                buf_glob = false;
            }
        };
    }
    // tilde at start
    if chars.first() == Some(&'~') {
        let home = interp.get_var("HOME").unwrap_or_else(|| "/root".to_string());
        // ~ or ~/...
        if chars.get(1).map(|c| *c == '/').unwrap_or(true) {
            buf.push_str(&home);
            i = 1;
        }
    }
    while i < chars.len() {
        let c = chars[i];
        match c {
            '\\' => {
                if i + 1 < chars.len() {
                    buf.push(chars[i + 1]);
                    i += 2;
                } else {
                    i += 1;
                }
            }
            '\'' => {
                i += 1;
                let start = i;
                while i < chars.len() && chars[i] != '\'' {
                    i += 1;
                }
                let s: String = chars[start..i].iter().collect();
                i += 1; // closing
                flush_unquoted!();
                parts.push(Part { text: s, quoted: true, has_glob: false });
            }
            '"' => {
                i += 1;
                let (text, consumed) = expand_double(interp, &chars[i..]);
                i += consumed;
                flush_unquoted!();
                parts.push(Part { text, quoted: true, has_glob: false });
            }
            '$' => {
                let (val, consumed, _glob) = expand_dollar(interp, &chars[i..], false);
                i += consumed;
                buf.push_str(&val);
            }
            '`' => {
                i += 1;
                let start = i;
                while i < chars.len() && chars[i] != '`' {
                    i += 1;
                }
                let cmd: String = chars[start..i].iter().collect();
                i += 1;
                let out = run_capture(interp, &cmd);
                buf.push_str(&out);
            }
            '*' | '?' => {
                buf.push(c);
                buf_glob = true;
                i += 1;
            }
            '[' => {
                buf.push(c);
                buf_glob = true;
                i += 1;
            }
            _ => {
                buf.push(c);
                i += 1;
            }
        }
    }
    if !buf.is_empty() {
        parts.push(Part { text: buf, quoted: false, has_glob: buf_glob });
    }
    parts
}

/// Expand the inside of a double-quoted string until the closing quote.
/// Returns (expanded_text, chars_consumed_including_closing_quote).
fn expand_double(interp: &mut Interp, chars: &[char]) -> (String, usize) {
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '"' => {
                i += 1;
                break;
            }
            '\\' => {
                if i + 1 < chars.len() {
                    let n = chars[i + 1];
                    if matches!(n, '"' | '\\' | '$' | '`') {
                        out.push(n);
                        i += 2;
                    } else {
                        out.push('\\');
                        i += 1;
                    }
                } else {
                    i += 1;
                }
            }
            '$' => {
                let (val, consumed, _) = expand_dollar(interp, &chars[i..], true);
                out.push_str(&val);
                i += consumed;
            }
            '`' => {
                i += 1;
                let start = i;
                while i < chars.len() && chars[i] != '`' {
                    i += 1;
                }
                let cmd: String = chars[start..i].iter().collect();
                i += 1;
                out.push_str(&run_capture(interp, &cmd));
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    (out, i)
}

/// Expand a `$...` starting at chars[0] == '$'. Returns (value, consumed, was_glob).
fn expand_dollar(interp: &mut Interp, chars: &[char], _in_quotes: bool) -> (String, usize, bool) {
    if chars.len() < 2 {
        return ("$".to_string(), 1, false);
    }
    match chars[1] {
        '(' => {
            if chars.get(2) == Some(&'(') {
                // arithmetic $(( ))
                let (inner, consumed) = read_double_paren(&chars[1..]);
                let val = eval_arith(interp, &inner);
                (val.to_string(), 1 + consumed, false)
            } else {
                let (inner, consumed) = read_balanced(&chars[1..], '(', ')');
                let out = run_capture(interp, &inner);
                (out, 1 + consumed, false)
            }
        }
        '{' => {
            // ${...}
            let (inner, consumed) = read_balanced(&chars[1..], '{', '}');
            let val = expand_param(interp, &inner);
            (val, 1 + consumed, false)
        }
        c if c == '?' || c == '$' || c == '#' || c == '@' || c == '*' => {
            let val = interp.get_var(&c.to_string()).unwrap_or_default();
            (val, 2, false)
        }
        c if c.is_ascii_alphabetic() || c == '_' => {
            let mut name = String::new();
            let mut i = 1;
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                name.push(chars[i]);
                i += 1;
            }
            let val = interp.get_var(&name).unwrap_or_default();
            (val, i, false)
        }
        c if c.is_ascii_digit() => {
            let mut name = String::new();
            let mut i = 1;
            while i < chars.len() && chars[i].is_ascii_digit() {
                name.push(chars[i]);
                i += 1;
            }
            let val = interp.get_var(&name).unwrap_or_default();
            (val, i, false)
        }
        _ => ("$".to_string(), 1, false),
    }
}

/// Handle the inside of `${...}`: name plus optional operator.
fn expand_param(interp: &mut Interp, inner: &str) -> String {
    // ${#name}
    if let Some(rest) = inner.strip_prefix('#') {
        let v = interp.get_var(rest).unwrap_or_default();
        return v.chars().count().to_string();
    }
    // find operator
    let ops = [":-", ":=", ":+", ":?", "##", "#", "%%", "%", "//", "/", "^^", "^", ",,", ","];
    // name is leading [A-Za-z0-9_@*] run
    let name_end = inner
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '@' || c == '*'))
        .unwrap_or(inner.len());
    let name = &inner[..name_end];
    let rest = &inner[name_end..];
    let cur = interp.get_var(name);
    if rest.is_empty() {
        return cur.unwrap_or_default();
    }
    for op in ops {
        if let Some(arg) = rest.strip_prefix(op) {
            let arg_expanded = expand_word(interp, arg, false).join(" ");
            return apply_param_op(interp, name, op, &arg_expanded, cur);
        }
    }
    cur.unwrap_or_default()
}

fn apply_param_op(
    interp: &mut Interp,
    name: &str,
    op: &str,
    arg: &str,
    cur: Option<String>,
) -> String {
    let is_set = cur.as_ref().map(|s| !s.is_empty()).unwrap_or(false);
    let exists = cur.is_some();
    match op {
        ":-" => {
            if is_set {
                cur.unwrap()
            } else {
                arg.to_string()
            }
        }
        "-" => {
            if exists {
                cur.unwrap()
            } else {
                arg.to_string()
            }
        }
        ":=" => {
            if is_set {
                cur.unwrap()
            } else {
                interp.set_var(name, arg);
                arg.to_string()
            }
        }
        ":+" => {
            if is_set {
                arg.to_string()
            } else {
                String::new()
            }
        }
        "+" => {
            if exists {
                arg.to_string()
            } else {
                String::new()
            }
        }
        ":?" => cur.unwrap_or_default(),
        "#" => strip_prefix_glob(&cur.unwrap_or_default(), arg, false),
        "##" => strip_prefix_glob(&cur.unwrap_or_default(), arg, true),
        "%" => strip_suffix_glob(&cur.unwrap_or_default(), arg, false),
        "%%" => strip_suffix_glob(&cur.unwrap_or_default(), arg, true),
        "/" | "//" => {
            let v = cur.unwrap_or_default();
            let (pat, rep) = match arg.split_once('/') {
                Some((p, r)) => (p.to_string(), r.to_string()),
                None => (arg.to_string(), String::new()),
            };
            if pat.is_empty() {
                return v;
            }
            if op == "//" {
                v.replace(&pat, &rep)
            } else {
                v.replacen(&pat, &rep, 1)
            }
        }
        "^^" => cur.unwrap_or_default().to_uppercase(),
        "^" => uppercase_first(&cur.unwrap_or_default()),
        ",," => cur.unwrap_or_default().to_lowercase(),
        "," => lowercase_first(&cur.unwrap_or_default()),
        _ => cur.unwrap_or_default(),
    }
}

fn uppercase_first(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}
fn lowercase_first(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_lowercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

fn glob_to_regex(pat: &str) -> String {
    let mut re = String::from("^");
    for c in pat.chars() {
        match c {
            '*' => re.push_str(".*"),
            '?' => re.push('.'),
            '.' | '+' | '(' | ')' | '|' | '^' | '$' | '{' | '}' | '\\' => {
                re.push('\\');
                re.push(c);
            }
            _ => re.push(c),
        }
    }
    re.push('$');
    re
}

fn strip_prefix_glob(s: &str, pat: &str, greedy: bool) -> String {
    // try to match pat (glob) at the start, removing shortest/longest match
    let re = glob_to_regex(pat);
    let re = re.trim_start_matches('^').trim_end_matches('$');
    let full = if greedy {
        format!("^({re})")
    } else {
        // shortest: make * lazy
        format!("^({})", re.replace(".*", ".*?"))
    };
    if let Ok(r) = regex::Regex::new(&full) {
        if let Some(m) = r.find(s) {
            return s[m.end()..].to_string();
        }
    }
    s.to_string()
}

fn strip_suffix_glob(s: &str, pat: &str, greedy: bool) -> String {
    let re = glob_to_regex(pat);
    let re = re.trim_start_matches('^').trim_end_matches('$');
    let full = if greedy {
        format!("({re})$")
    } else {
        format!("({})$", re.replace(".*", ".*?"))
    };
    if let Ok(r) = regex::Regex::new(&full) {
        if let Some(m) = r.find(s) {
            return s[..m.start()].to_string();
        }
    }
    s.to_string()
}

fn read_balanced(chars: &[char], open: char, close: char) -> (String, usize) {
    // chars[0] == open
    let mut depth = 0;
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == open {
            depth += 1;
            if depth > 1 {
                out.push(c);
            }
        } else if c == close {
            depth -= 1;
            if depth == 0 {
                i += 1;
                break;
            }
            out.push(c);
        } else {
            out.push(c);
        }
        i += 1;
    }
    (out, i)
}

fn read_double_paren(chars: &[char]) -> (String, usize) {
    // chars[0..2] == "((" ; read until "))"
    let mut out = String::new();
    let mut i = 2;
    let mut depth = 1;
    while i < chars.len() {
        if chars[i] == '(' {
            depth += 1;
            out.push('(');
            i += 1;
        } else if chars[i] == ')' {
            depth -= 1;
            if depth == 0 {
                // expect another ')'
                i += 1;
                if chars.get(i) == Some(&')') {
                    i += 1;
                }
                break;
            }
            out.push(')');
            i += 1;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    (out, i)
}

fn run_capture(interp: &mut Interp, src: &str) -> String {
    let ast = crate::shell::parse(src);
    let mut out = Vec::new();
    let mut err = Vec::new();
    crate::exec::exec(interp, &ast, Vec::new(), &mut out, &mut err);
    let mut s = String::from_utf8_lossy(&out).into_owned();
    while s.ends_with('\n') {
        s.pop();
    }
    s
}

pub fn eval_arith(interp: &mut Interp, expr: &str) -> i64 {
    let mut p = ArithParser { interp, chars: expr.chars().collect(), i: 0 };
    let v = p.expr();
    v
}

struct ArithParser<'a> {
    interp: &'a mut Interp,
    chars: Vec<char>,
    i: usize,
}

impl ArithParser<'_> {
    fn skip_ws(&mut self) {
        while self.i < self.chars.len() && self.chars[self.i].is_whitespace() {
            self.i += 1;
        }
    }
    fn peek(&mut self) -> Option<char> {
        self.skip_ws();
        self.chars.get(self.i).copied()
    }
    fn expr(&mut self) -> i64 {
        self.ternary()
    }
    fn ternary(&mut self) -> i64 {
        let c = self.add_sub();
        if self.peek() == Some('?') {
            self.i += 1;
            let a = self.add_sub();
            self.skip_ws();
            if self.peek() == Some(':') {
                self.i += 1;
            }
            let b = self.add_sub();
            if c != 0 {
                a
            } else {
                b
            }
        } else {
            c
        }
    }
    fn add_sub(&mut self) -> i64 {
        let mut v = self.mul_div();
        loop {
            match self.peek() {
                Some('+') => {
                    self.i += 1;
                    v += self.mul_div();
                }
                Some('-') => {
                    self.i += 1;
                    v -= self.mul_div();
                }
                _ => break,
            }
        }
        v
    }
    fn mul_div(&mut self) -> i64 {
        let mut v = self.unary();
        loop {
            match self.peek() {
                Some('*') => {
                    self.i += 1;
                    v *= self.unary();
                }
                Some('/') => {
                    self.i += 1;
                    let d = self.unary();
                    if d != 0 {
                        v /= d;
                    }
                }
                Some('%') => {
                    self.i += 1;
                    let d = self.unary();
                    if d != 0 {
                        v %= d;
                    }
                }
                _ => break,
            }
        }
        v
    }
    fn unary(&mut self) -> i64 {
        match self.peek() {
            Some('-') => {
                self.i += 1;
                -self.unary()
            }
            Some('+') => {
                self.i += 1;
                self.unary()
            }
            Some('!') => {
                self.i += 1;
                if self.unary() == 0 {
                    1
                } else {
                    0
                }
            }
            _ => self.atom(),
        }
    }
    fn atom(&mut self) -> i64 {
        self.skip_ws();
        match self.peek() {
            Some('(') => {
                self.i += 1;
                let v = self.expr();
                self.skip_ws();
                if self.peek() == Some(')') {
                    self.i += 1;
                }
                v
            }
            Some(c) if c.is_ascii_digit() => {
                let mut n = String::new();
                while let Some(d) = self.chars.get(self.i) {
                    if d.is_ascii_digit() {
                        n.push(*d);
                        self.i += 1;
                    } else {
                        break;
                    }
                }
                n.parse().unwrap_or(0)
            }
            Some(c) if c.is_ascii_alphabetic() || c == '_' => {
                let mut name = String::new();
                while let Some(d) = self.chars.get(self.i) {
                    if d.is_ascii_alphanumeric() || *d == '_' {
                        name.push(*d);
                        self.i += 1;
                    } else {
                        break;
                    }
                }
                self.interp
                    .get_var(&name)
                    .and_then(|v| v.trim().parse().ok())
                    .unwrap_or(0)
            }
            Some('$') => {
                self.i += 1;
                self.atom()
            }
            _ => 0,
        }
    }
}

fn glob_vfs(interp: &Interp, pattern: &str) -> Vec<String> {
    // Only glob the basename components that contain metacharacters, against the VFS.
    // Split pattern into directory part and a per-component glob walk.
    let absolute = pattern.starts_with('/');
    let base = if absolute { "/".to_string() } else { interp.cwd.clone() };
    let comps: Vec<&str> = pattern.split('/').filter(|c| !c.is_empty()).collect();
    let mut current = vec![base];
    for comp in &comps {
        let mut next = Vec::new();
        let has_meta = comp.contains('*') || comp.contains('?') || comp.contains('[');
        for dir in &current {
            if has_meta {
                if let Ok(entries) = interp.vfs.list_dir("/", dir) {
                    let re = regex::Regex::new(&glob_to_regex(comp)).ok();
                    for e in entries {
                        // skip hidden unless pattern starts with .
                        if e.starts_with('.') && !comp.starts_with('.') {
                            continue;
                        }
                        let matched = re.as_ref().map(|r| r.is_match(&e)).unwrap_or(false);
                        if matched {
                            next.push(format!("{}/{}", dir.trim_end_matches('/'), e));
                        }
                    }
                }
            } else {
                let cand = format!("{}/{}", dir.trim_end_matches('/'), comp);
                if interp.vfs.lexists("/", &cand) {
                    next.push(cand);
                }
            }
        }
        current = next;
        if current.is_empty() {
            return Vec::new();
        }
    }
    // produce results relative to cwd if pattern was relative
    let mut out: Vec<String> = current
        .into_iter()
        .map(|p| {
            if absolute {
                p
            } else {
                let prefix = format!("{}/", interp.cwd.trim_end_matches('/'));
                p.strip_prefix(&prefix).map(|s| s.to_string()).unwrap_or(p)
            }
        })
        .collect();
    out.sort();
    let _ = resolve_against; // keep import used
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ex(src: &str) -> Vec<String> {
        let mut i = Interp::new();
        i.set_var("FOO", "bar");
        i.set_var("EMPTY", "");
        expand_word(&mut i, src, true)
    }

    #[test]
    fn vars_and_quotes() {
        assert_eq!(ex("$FOO"), vec!["bar"]);
        assert_eq!(ex("\"$FOO\""), vec!["bar"]);
        assert_eq!(ex("'$FOO'"), vec!["$FOO"]);
        assert_eq!(ex("a${FOO}b"), vec!["abarb"]);
        assert_eq!(ex("${EMPTY:-def}"), vec!["def"]);
        assert_eq!(ex("${MISSING:-x}"), vec!["x"]);
        assert_eq!(ex("${#FOO}"), vec!["3"]);
    }

    #[test]
    fn arithmetic() {
        assert_eq!(ex("$((1+2*3))"), vec!["7"]);
        assert_eq!(ex("$(( (1+2)*3 ))"), vec!["9"]);
    }

    #[test]
    fn suffix_prefix() {
        let mut i = Interp::new();
        i.set_var("F", "file.tar.gz");
        assert_eq!(expand_word(&mut i, "${F%.gz}", true), vec!["file.tar"]);
        assert_eq!(expand_word(&mut i, "${F%%.*}", true), vec!["file"]);
        assert_eq!(expand_word(&mut i, "${F#*.}", true), vec!["tar.gz"]);
        assert_eq!(expand_word(&mut i, "${F##*.}", true), vec!["gz"]);
    }
}
