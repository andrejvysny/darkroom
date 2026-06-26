//! Lightroom `.lrtemplate` importer. The file is a Lua assignment
//! `s = { … value = { settings = { … } } }`. We parse a strict subset (nested tables, strings,
//! numbers, booleans — no functions/metatables/expressions) into the `settings` table, then reuse the
//! exact same crs → IR mapping as the `.xmp` importer ([`super::lr_xmp::build_ir`]).

use std::collections::HashMap;
use std::path::Path;

use crate::error::PresetError;
use crate::formats::lr_xmp::{build_ir, parse_xy};
use crate::ir::{PresetIr, ToneCurveIr};
use crate::registry::PresetImporter;

pub struct LightroomTemplate;

fn ext_eq(path: Option<&Path>, ext: &str) -> bool {
    path.and_then(|p| p.extension())
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case(ext))
        .unwrap_or(false)
}

impl PresetImporter for LightroomTemplate {
    fn format_name(&self) -> &'static str {
        "lightroom-lrtemplate"
    }

    fn detect(&self, bytes: &[u8], path: Option<&Path>) -> bool {
        if ext_eq(path, "lrtemplate") {
            return true;
        }
        if ext_eq(path, "xmp") {
            return false;
        }
        let head = String::from_utf8_lossy(&bytes[..bytes.len().min(4096)]);
        let trimmed = head.trim_start();
        (trimmed.starts_with("s = {") || trimmed.starts_with("s={")) && head.contains("settings")
    }

    fn parse(&self, bytes: &[u8], _path: Option<&Path>) -> Result<PresetIr, PresetError> {
        let text = String::from_utf8_lossy(bytes);
        let mut p = Parser::new(&text);
        // Skip the leading `s =` (or any preamble) to the first table.
        while let Some(c) = p.peek() {
            if c == b'{' {
                break;
            }
            p.i += 1;
        }
        let root = match p.parse_value()? {
            Lua::Table(t) => t,
            _ => return Err(PresetError::Malformed("expected a Lua table".into())),
        };
        let settings = find_settings(&root)
            .ok_or_else(|| PresetError::Malformed("no settings table".into()))?;

        let mut attrs: HashMap<String, String> = HashMap::new();
        let mut tc = ToneCurveIr::default();
        for (k, v) in &settings.map {
            match v {
                Lua::Num(n) => {
                    attrs.insert(k.clone(), fmt_num(*n));
                }
                Lua::Str(s) => {
                    attrs.insert(k.clone(), s.clone());
                }
                Lua::Bool(b) => {
                    attrs.insert(k.clone(), if *b { "True" } else { "False" }.into());
                }
                Lua::Table(t) => match k.as_str() {
                    "ToneCurvePV2012" => tc.rgb = curve_from(t),
                    "ToneCurvePV2012Red" => tc.r = curve_from(t),
                    "ToneCurvePV2012Green" => tc.g = curve_from(t),
                    "ToneCurvePV2012Blue" => tc.b = curve_from(t),
                    _ => {}
                },
            }
        }

        if attrs.is_empty() && tc.rgb.is_empty() {
            return Err(PresetError::Malformed("empty settings".into()));
        }
        Ok(build_ir(&attrs, tc))
    }
}

fn fmt_num(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

fn curve_from(t: &LuaTable) -> Vec<(f32, f32)> {
    t.arr
        .iter()
        .filter_map(|v| {
            if let Lua::Str(s) = v {
                parse_xy(s)
            } else {
                None
            }
        })
        .collect()
}

fn find_settings(root: &LuaTable) -> Option<&LuaTable> {
    if let Some(Lua::Table(v)) = root.map.get("value") {
        if let Some(Lua::Table(s)) = v.map.get("settings") {
            return Some(s);
        }
    }
    if let Some(Lua::Table(s)) = root.map.get("settings") {
        return Some(s);
    }
    None
}

// ── Minimal Lua-table parser ─────────────────────────────────────────────────

enum Lua {
    Num(f64),
    Str(String),
    Bool(bool),
    Table(LuaTable),
}

#[derive(Default)]
struct LuaTable {
    map: HashMap<String, Lua>,
    arr: Vec<Lua>,
}

/// Max table-nesting depth. Real `.lrtemplate` files nest ~5 levels; this bounds recursion so a
/// hostile/garbage file of nested `{` can't overflow the stack.
const MAX_DEPTH: usize = 64;

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
    depth: usize,
}

impl<'a> Parser<'a> {
    fn new(s: &'a str) -> Self {
        Self {
            s: s.as_bytes(),
            i: 0,
            depth: 0,
        }
    }

    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }

    fn bump(&mut self) -> Option<u8> {
        let c = self.peek();
        if c.is_some() {
            self.i += 1;
        }
        c
    }

    fn skip_ws(&mut self) {
        loop {
            while let Some(c) = self.peek() {
                if c.is_ascii_whitespace() {
                    self.i += 1;
                } else {
                    break;
                }
            }
            // Lua line comment `-- …`.
            if self.peek() == Some(b'-') && self.s.get(self.i + 1) == Some(&b'-') {
                while let Some(c) = self.bump() {
                    if c == b'\n' {
                        break;
                    }
                }
            } else {
                break;
            }
        }
    }

    fn parse_value(&mut self) -> Result<Lua, PresetError> {
        self.skip_ws();
        match self.peek() {
            Some(b'{') => {
                self.depth += 1;
                if self.depth > MAX_DEPTH {
                    return Err(PresetError::Malformed("table nesting too deep".into()));
                }
                let r = self.parse_table();
                self.depth -= 1;
                r
            }
            Some(b'"') | Some(b'\'') => Ok(Lua::Str(self.parse_string()?)),
            Some(b'[') if self.is_long_bracket() => Ok(Lua::Str(self.parse_long_string()?)),
            Some(c) if c == b'-' || c == b'+' || c == b'.' || c.is_ascii_digit() => {
                self.parse_number()
            }
            Some(_) => self.parse_word(),
            None => Err(PresetError::Malformed("unexpected end of input".into())),
        }
    }

    fn parse_string(&mut self) -> Result<String, PresetError> {
        let q = self.bump().unwrap();
        let mut out: Vec<u8> = Vec::new();
        while let Some(c) = self.bump() {
            if c == q {
                return Ok(String::from_utf8_lossy(&out).into_owned());
            }
            if c == b'\\' {
                if let Some(n) = self.bump() {
                    out.push(match n {
                        b'n' => b'\n',
                        b't' => b'\t',
                        other => other,
                    });
                }
            } else {
                out.push(c);
            }
        }
        Err(PresetError::Malformed("unterminated string".into()))
    }

    /// True when the cursor is at a Lua long-bracket opener: `[`, zero+ `=`, then `[`.
    fn is_long_bracket(&self) -> bool {
        if self.peek() != Some(b'[') {
            return false;
        }
        let mut j = self.i + 1;
        while self.s.get(j) == Some(&b'=') {
            j += 1;
        }
        self.s.get(j) == Some(&b'[')
    }

    /// Parse a Lua long-bracket string `[[ … ]]` / `[==[ … ]==]` (level = number of `=`). A newline
    /// immediately after the opener is dropped, per Lua. The cursor is assumed at the opening `[`.
    fn parse_long_string(&mut self) -> Result<String, PresetError> {
        self.bump(); // first '['
        let mut level = 0usize;
        while self.peek() == Some(b'=') {
            self.bump();
            level += 1;
        }
        if self.bump() != Some(b'[') {
            return Err(PresetError::Malformed("bad long-bracket opener".into()));
        }
        if self.peek() == Some(b'\r') {
            self.bump();
        }
        if self.peek() == Some(b'\n') {
            self.bump();
        }
        let start = self.i;
        loop {
            match self.peek() {
                None => return Err(PresetError::Malformed("unterminated long string".into())),
                Some(b']') => {
                    let close = self.i;
                    self.bump(); // ']'
                    let mut l = 0usize;
                    while self.peek() == Some(b'=') {
                        self.bump();
                        l += 1;
                    }
                    if l == level && self.peek() == Some(b']') {
                        self.bump();
                        return Ok(String::from_utf8_lossy(&self.s[start..close]).into_owned());
                    }
                    // Not the matching close — resume scanning just past this ']'.
                    self.i = close + 1;
                }
                Some(_) => self.i += 1,
            }
        }
    }

    fn parse_number(&mut self) -> Result<Lua, PresetError> {
        let start = self.i;
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() || matches!(c, b'.' | b'-' | b'+' | b'e' | b'E') {
                self.i += 1;
            } else {
                break;
            }
        }
        let tok = std::str::from_utf8(&self.s[start..self.i]).unwrap_or("");
        tok.parse::<f64>()
            .map(Lua::Num)
            .map_err(|_| PresetError::Malformed(format!("bad number '{tok}'")))
    }

    fn parse_word(&mut self) -> Result<Lua, PresetError> {
        let start = self.i;
        while let Some(c) = self.peek() {
            if c.is_ascii_alphanumeric() || c == b'_' {
                self.i += 1;
            } else {
                break;
            }
        }
        if self.i == start {
            return Err(PresetError::Malformed("unexpected token".into()));
        }
        let w = std::str::from_utf8(&self.s[start..self.i]).unwrap_or("");
        Ok(match w {
            "true" => Lua::Bool(true),
            "false" => Lua::Bool(false),
            other => Lua::Str(other.to_string()),
        })
    }

    fn parse_table(&mut self) -> Result<Lua, PresetError> {
        self.bump(); // '{'
        let mut t = LuaTable::default();
        loop {
            self.skip_ws();
            match self.peek() {
                Some(b'}') => {
                    self.bump();
                    break;
                }
                Some(b',') | Some(b';') => {
                    self.bump();
                    continue;
                }
                None => return Err(PresetError::Malformed("unterminated table".into())),
                _ => {}
            }
            if self.peek() == Some(b'[') && !self.is_long_bracket() {
                // ["Key"] = value
                self.bump();
                self.skip_ws();
                let key = self.parse_string()?;
                self.skip_ws();
                if self.peek() == Some(b']') {
                    self.bump();
                }
                self.skip_ws();
                if self.peek() == Some(b'=') {
                    self.bump();
                }
                let v = self.parse_value()?;
                t.map.insert(key, v);
            } else if let Some(key) = self.try_key() {
                let v = self.parse_value()?;
                t.map.insert(key, v);
            } else {
                let v = self.parse_value()?;
                t.arr.push(v);
            }
        }
        Ok(Lua::Table(t))
    }

    /// If the next token is `identifier =`, consume it and return the identifier; otherwise leave the
    /// position untouched (the entry is a positional/array value).
    fn try_key(&mut self) -> Option<String> {
        let save = self.i;
        self.skip_ws();
        let start = self.i;
        while let Some(c) = self.peek() {
            if c.is_ascii_alphanumeric() || c == b'_' {
                self.i += 1;
            } else {
                break;
            }
        }
        if self.i == start {
            self.i = save;
            return None;
        }
        let ident = std::str::from_utf8(&self.s[start..self.i])
            .unwrap_or("")
            .to_string();
        self.skip_ws();
        if self.peek() == Some(b'=') && self.s.get(self.i + 1) != Some(&b'=') {
            self.bump(); // '='
            Some(ident)
        } else {
            self.i = save;
            None
        }
    }
}
