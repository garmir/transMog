//! just enough json to pull one object out of a websocket message.
//! numbers keep their text, so a value compares the same way the sender
//! wrote it

#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Number(String),
    String(String),
    Array(Vec<Json>),
    /// members in document order
    Object(Vec<(String, Json)>),
}

impl Json {
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Object(members) => members.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// a scalar as a control value: numbers as written, true and false
    /// as 1 and 0; none for null, arrays and objects
    pub fn as_value(&self) -> Option<String> {
        match self {
            Json::Bool(b) => Some(u8::from(*b).to_string()),
            Json::Number(n) => Some(n.clone()),
            Json::String(s) => Some(s.clone()),
            _ => None,
        }
    }
}

pub fn parse(text: &str) -> Result<Json, String> {
    let mut p = Parser {
        s: text.as_bytes(),
        i: 0,
        depth: 0,
    };
    let v = p.value()?;
    p.ws();
    if p.i != p.s.len() {
        return Err(format!("trailing data at {}", p.i));
    }
    Ok(v)
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
    depth: u32,
}

impl Parser<'_> {
    fn ws(&mut self) {
        while self.s.get(self.i).is_some_and(u8::is_ascii_whitespace) {
            self.i += 1;
        }
    }

    fn err<T>(&self, what: &str) -> Result<T, String> {
        Err(format!("{what} at {}", self.i))
    }

    fn eat(&mut self, word: &str) -> bool {
        if self.s[self.i..].starts_with(word.as_bytes()) {
            self.i += word.len();
            true
        } else {
            false
        }
    }

    fn value(&mut self) -> Result<Json, String> {
        self.ws();
        match self.s.get(self.i) {
            None => self.err("end of input"),
            Some(b'{') => self.nested(Parser::object),
            Some(b'[') => self.nested(Parser::array),
            Some(b'"') => self.string().map(Json::String),
            Some(b't') if self.eat("true") => Ok(Json::Bool(true)),
            Some(b'f') if self.eat("false") => Ok(Json::Bool(false)),
            Some(b'n') if self.eat("null") => Ok(Json::Null),
            Some(c) if *c == b'-' || c.is_ascii_digit() => self.number(),
            Some(_) => self.err("unexpected character"),
        }
    }

    fn nested(&mut self, f: fn(&mut Self) -> Result<Json, String>) -> Result<Json, String> {
        self.depth += 1;
        if self.depth > 64 {
            return self.err("nesting too deep");
        }
        self.i += 1;
        let v = f(self);
        self.depth -= 1;
        v
    }

    fn object(&mut self) -> Result<Json, String> {
        let mut members = Vec::new();
        self.ws();
        if self.eat("}") {
            return Ok(Json::Object(members));
        }
        loop {
            self.ws();
            if self.s.get(self.i) != Some(&b'"') {
                return self.err("expected a key");
            }
            let key = self.string()?;
            self.ws();
            if !self.eat(":") {
                return self.err("expected :");
            }
            members.push((key, self.value()?));
            self.ws();
            if self.eat(",") {
                continue;
            }
            if self.eat("}") {
                return Ok(Json::Object(members));
            }
            return self.err("expected , or }");
        }
    }

    fn array(&mut self) -> Result<Json, String> {
        let mut items = Vec::new();
        self.ws();
        if self.eat("]") {
            return Ok(Json::Array(items));
        }
        loop {
            items.push(self.value()?);
            self.ws();
            if self.eat(",") {
                continue;
            }
            if self.eat("]") {
                return Ok(Json::Array(items));
            }
            return self.err("expected , or ]");
        }
    }

    fn number(&mut self) -> Result<Json, String> {
        let start = self.i;
        while self
            .s
            .get(self.i)
            .is_some_and(|c| c.is_ascii_digit() || b"+-.eE".contains(c))
        {
            self.i += 1;
        }
        let text = std::str::from_utf8(&self.s[start..self.i]).expect("ascii");
        if text.parse::<f64>().is_err() {
            return self.err("bad number");
        }
        Ok(Json::Number(text.to_string()))
    }

    fn string(&mut self) -> Result<String, String> {
        self.i += 1;
        let mut out = Vec::new();
        loop {
            let Some(&c) = self.s.get(self.i) else {
                return self.err("unterminated string");
            };
            self.i += 1;
            match c {
                b'"' => break,
                b'\\' => {
                    let Some(&e) = self.s.get(self.i) else {
                        return self.err("unterminated escape");
                    };
                    self.i += 1;
                    let ch = match e {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => self.unicode()?,
                        _ => return self.err("bad escape"),
                    };
                    let mut buf = [0; 4];
                    out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                }
                _ => out.push(c),
            }
        }
        String::from_utf8(out).or_else(|_| self.err("invalid utf-8"))
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let digits = self.s.get(self.i..self.i + 4).unwrap_or(&[]);
        let v = std::str::from_utf8(digits)
            .ok()
            .and_then(|d| u32::from_str_radix(d, 16).ok());
        match v {
            Some(v) if digits.len() == 4 => {
                self.i += 4;
                Ok(v)
            }
            _ => self.err("bad \\u escape"),
        }
    }

    fn unicode(&mut self) -> Result<char, String> {
        let hi = self.hex4()?;
        if (0xd800..0xdc00).contains(&hi) && self.eat("\\u") {
            let lo = self.hex4()?;
            let c = 0x10000 + ((hi - 0xd800) << 10) + (lo.wrapping_sub(0xdc00) & 0x3ff);
            return Ok(char::from_u32(c).unwrap_or('\u{fffd}'));
        }
        Ok(char::from_u32(hi).unwrap_or('\u{fffd}'))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_viz_ctrl_message() {
        let text = r##"{"seq":7644,"frame":629707,"t":1793921606.898878,
            "audio":{"low":0.0,"kick":0.5},
            "assets":{"camera":{"enable":1,"tint":"#ffffff",
              "transmog":{"qa_lock":1,"zoom_absolute":150.0,"fisheye_enabled":true,
                          "fisheye_projection":2,"label":"a \"b\" é"}},
              "lattice":{"scale":1.0}}}"##;
        let v = parse(text).unwrap();
        assert_eq!(v.get("seq"), Some(&Json::Number("7644".into())));
        let tm = v
            .get("assets")
            .and_then(|a| a.get("camera"))
            .and_then(|c| c.get("transmog"));
        let Some(Json::Object(members)) = tm else {
            panic!("no transmog object in {v:?}");
        };
        let values: Vec<(&str, String)> = members
            .iter()
            .filter_map(|(k, v)| Some((k.as_str(), v.as_value()?)))
            .collect();
        assert_eq!(
            values,
            vec![
                ("qa_lock", "1".into()),
                ("zoom_absolute", "150.0".into()),
                ("fisheye_enabled", "1".into()),
                ("fisheye_projection", "2".into()),
                ("label", "a \"b\" é".into()),
            ]
        );
    }

    #[test]
    fn refuses_broken_text() {
        for bad in ["{", "{\"a\" 1}", "[1,]", "\"open", "{} x", "-", "nul"] {
            assert!(parse(bad).is_err(), "{bad}");
        }
        assert_eq!(parse(" [ ] ").unwrap(), Json::Array(Vec::new()));
    }
}
