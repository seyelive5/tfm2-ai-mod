//! Minimal zero-dependency JSON reader for the stable API's record documents
//! (`record_get_json` returns whole-struct JSON views; field names match game-core).

#[derive(Clone, Debug)]
pub(crate) enum JsonVal {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<JsonVal>),
    Obj(Vec<(String, JsonVal)>),
}

impl JsonVal {
    pub(crate) fn parse(s: &str) -> Option<JsonVal> {
        let b = s.as_bytes();
        let mut i = 0;
        let v = Self::parse_val(b, &mut i)?;
        Some(v)
    }

    fn skip_ws(b: &[u8], i: &mut usize) {
        while *i < b.len() && matches!(b[*i], b' ' | b'\t' | b'\r' | b'\n') {
            *i += 1;
        }
    }

    fn parse_val(b: &[u8], i: &mut usize) -> Option<JsonVal> {
        Self::skip_ws(b, i);
        if *i >= b.len() {
            return None;
        }
        match b[*i] {
            b'"' => Self::parse_str(b, i).map(JsonVal::Str),
            b'{' => Self::parse_obj(b, i),
            b'[' => Self::parse_arr(b, i),
            b't' => {
                if b.get(*i..*i + 4) == Some(b"true") {
                    *i += 4;
                    Some(JsonVal::Bool(true))
                } else {
                    None
                }
            }
            b'f' => {
                if b.get(*i..*i + 5) == Some(b"false") {
                    *i += 5;
                    Some(JsonVal::Bool(false))
                } else {
                    None
                }
            }
            b'n' => {
                if b.get(*i..*i + 4) == Some(b"null") {
                    *i += 4;
                    Some(JsonVal::Null)
                } else {
                    None
                }
            }
            c if c == b'-' || c.is_ascii_digit() => Self::parse_num(b, i),
            _ => None,
        }
    }

    fn parse_str(b: &[u8], i: &mut usize) -> Option<String> {
        if b.get(*i) != Some(&b'"') {
            return None;
        }
        *i += 1;
        let start = *i;
        // Fast path: scan for closing quote without escapes
        let mut has_esc = false;
        while *i < b.len() {
            let c = b[*i];
            if c == b'"' {
                let end = *i;
                *i += 1;
                if !has_esc {
                    return std::str::from_utf8(&b[start..end]).ok().map(|s| s.to_string());
                }
                break;
            } else if c == b'\\' {
                has_esc = true;
                *i += 2;
            } else {
                *i += 1;
            }
        }
        if !has_esc {
            return None;
        }
        // Slow path with escape decoding
        let mut j = start;
        let mut out = String::new();
        while j < b.len() {
            let c = b[j];
            if c == b'"' {
                return Some(out);
            } else if c == b'\\' {
                j += 1;
                if j >= b.len() {
                    return None;
                }
                match b[j] {
                    b'"' => out.push('"'),
                    b'\\' => out.push('\\'),
                    b'/' => out.push('/'),
                    b'b' => out.push('\u{0008}'),
                    b'f' => out.push('\u{000c}'),
                    b'n' => out.push('\n'),
                    b'r' => out.push('\r'),
                    b't' => out.push('\t'),
                    b'u' => {
                        if j + 4 < b.len() {
                            if let Ok(hex) = std::str::from_utf8(&b[j + 1..j + 5]) {
                                if let Ok(code) = u32::from_str_radix(hex, 16) {
                                    if let Some(ch) = char::from_u32(code) {
                                        out.push(ch);
                                    }
                                }
                            }
                            j += 4;
                        }
                    }
                    other => out.push(other as char),
                }
                j += 1;
            } else {
                let ch_start = j;
                j += 1;
                while j < b.len() && (b[j] & 0b1100_0000) == 0b1000_0000 {
                    j += 1;
                }
                if let Ok(s) = std::str::from_utf8(&b[ch_start..j]) {
                    out.push_str(s);
                }
            }
        }
        None
    }

    fn parse_num(b: &[u8], i: &mut usize) -> Option<JsonVal> {
        let start = *i;
        while *i < b.len()
            && matches!(b[*i], b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E')
        {
            *i += 1;
        }
        let s = std::str::from_utf8(&b[start..*i]).ok()?;
        s.parse::<f64>().ok().map(JsonVal::Num)
    }

    fn parse_arr(b: &[u8], i: &mut usize) -> Option<JsonVal> {
        *i += 1; // skip '['
        let mut arr = Vec::new();
        loop {
            Self::skip_ws(b, i);
            if *i >= b.len() {
                return None;
            }
            if b[*i] == b']' {
                *i += 1;
                return Some(JsonVal::Arr(arr));
            }
            let v = Self::parse_val(b, i)?;
            arr.push(v);
            Self::skip_ws(b, i);
            if *i < b.len() && b[*i] == b',' {
                *i += 1;
            }
        }
    }

    fn parse_obj(b: &[u8], i: &mut usize) -> Option<JsonVal> {
        *i += 1; // skip '{'
        let mut obj = Vec::new();
        loop {
            Self::skip_ws(b, i);
            if *i >= b.len() {
                return None;
            }
            if b[*i] == b'}' {
                *i += 1;
                return Some(JsonVal::Obj(obj));
            }
            let k = Self::parse_str(b, i)?;
            Self::skip_ws(b, i);
            if b.get(*i) != Some(&b':') {
                return None;
            }
            *i += 1;
            let v = Self::parse_val(b, i)?;
            obj.push((k, v));
            Self::skip_ws(b, i);
            if *i < b.len() && b[*i] == b',' {
                *i += 1;
            }
        }
    }

    pub(crate) fn get(&self, key: &str) -> Option<&JsonVal> {
        match self {
            JsonVal::Obj(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub(crate) fn as_str(&self) -> Option<&str> {
        match self {
            JsonVal::Str(s) => Some(s.as_str()),
            _ => None,
        }
    }

    pub(crate) fn as_f64(&self) -> Option<f64> {
        match self {
            JsonVal::Num(n) => Some(*n),
            _ => None,
        }
    }

    pub(crate) fn as_u64(&self) -> Option<u64> {
        self.as_f64().map(|n| n.max(0.0) as u64)
    }

    pub(crate) fn as_usize(&self) -> Option<usize> {
        self.as_f64().map(|n| n.max(0.0) as usize)
    }

    pub(crate) fn as_bool(&self) -> Option<bool> {
        match self {
            JsonVal::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub(crate) fn as_arr(&self) -> Option<&[JsonVal]> {
        match self {
            JsonVal::Arr(a) => Some(a.as_slice()),
            _ => None,
        }
    }

    pub(crate) fn as_obj(&self) -> Option<&[(String, JsonVal)]> {
        match self {
            JsonVal::Obj(o) => Some(o.as_slice()),
            _ => None,
        }
    }

    /// Map-shaped containers serialize either as an object or as an array of
    /// `[key, value]` pairs (non-string map keys); `entries` reads both.
    pub(crate) fn entries(&self) -> Vec<(String, &JsonVal)> {
        match self {
            JsonVal::Obj(o) => o.iter().map(|(k, v)| (k.clone(), v)).collect(),
            JsonVal::Arr(a) => a
                .iter()
                .filter_map(|item| match item.as_arr() {
                    Some([k, v]) => {
                        let key = match k {
                            JsonVal::Str(s) => s.clone(),
                            JsonVal::Num(n) => format!("{}", n),
                            _ => return None,
                        };
                        Some((key, v))
                    }
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        }
    }

    /// Values of an object, or the items of an array (sequence or pair-shaped map).
    pub(crate) fn values(&self) -> Vec<&JsonVal> {
        match self {
            JsonVal::Obj(o) => o.iter().map(|(_, v)| v).collect(),
            JsonVal::Arr(a) => a
                .iter()
                .map(|item| match item.as_arr() {
                    Some([JsonVal::Str(_) | JsonVal::Num(_), v]) => v,
                    _ => item,
                })
                .collect(),
            _ => Vec::new(),
        }
    }

    /// Top-level keys, comma-joined (diagnostics: learn a record's real shape).
    pub(crate) fn keys(&self) -> String {
        self.as_obj()
            .map(|o| o.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>().join(","))
            .unwrap_or_default()
    }
}
