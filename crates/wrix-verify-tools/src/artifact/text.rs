//! Python-compatible Unicode strings, including surrogateescaped archive bytes.

use std::io;

/// Unicode code points, including unpaired surrogates retained by JSON and tarfile.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Text(Vec<u32>);

impl Text {
    pub fn from_utf8(value: &str) -> Self {
        Self(value.chars().map(u32::from).collect())
    }

    pub fn from_bytes(mut bytes: &[u8]) -> Self {
        let mut points = Vec::new();
        while !bytes.is_empty() {
            match std::str::from_utf8(bytes) {
                Ok(text) => {
                    points.extend(text.chars().map(u32::from));
                    break;
                }
                Err(error) => {
                    let (valid, rest) = bytes.split_at(error.valid_up_to());
                    points.extend(String::from_utf8_lossy(valid).chars().map(u32::from));
                    let count = error.error_len().unwrap_or(rest.len());
                    points.extend(rest[..count].iter().map(|byte| 0xdc00 + u32::from(*byte)));
                    bytes = &rest[count..];
                }
            }
        }
        Self(points)
    }

    pub fn bytes(&self) -> io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        for &point in &self.0 {
            if (0xdc80..=0xdcff).contains(&point) {
                bytes.push(u8::try_from(point - 0xdc00).map_err(io::Error::other)?);
            } else {
                let character = char::from_u32(point)
                    .ok_or_else(|| io::Error::other("unencodable surrogate in filesystem path"))?;
                bytes.extend_from_slice(character.encode_utf8(&mut [0; 4]).as_bytes());
            }
        }
        Ok(bytes)
    }

    pub fn unicode(&self) -> io::Result<String> {
        self.0
            .iter()
            .map(|&point| {
                char::from_u32(point)
                    .ok_or_else(|| io::Error::other("unpaired surrogate is not Unicode"))
            })
            .collect()
    }

    pub fn trim_directory_slashes(&mut self) {
        while self.0.last() == Some(&47) {
            self.0.pop();
        }
    }

    pub fn normalized(&self) -> io::Result<Self> {
        let mut output = Vec::new();
        for part in self.0.split(|point| *point == 47) {
            if part == [46, 46] {
                return Err(io::Error::other("unsafe archive member"));
            }
            if part.is_empty() || part == [46] {
                continue;
            }
            if !output.is_empty() {
                output.push(47);
            }
            output.extend_from_slice(part);
        }
        if output.is_empty() {
            output.push(46);
        }
        Ok(Self(output))
    }

    pub fn json(&self, output: &mut String) {
        output.push('"');
        for &point in &self.0 {
            match point {
                34 => output.push_str("\\\""),
                92 => output.push_str("\\\\"),
                8 => output.push_str("\\b"),
                9 => output.push_str("\\t"),
                10 => output.push_str("\\n"),
                12 => output.push_str("\\f"),
                13 => output.push_str("\\r"),
                32..=126 => {
                    if let Some(character) = char::from_u32(point) {
                        output.push(character);
                    }
                }
                _ => {
                    use std::fmt::Write;
                    if point <= 0xffff {
                        // Formatting into String cannot fail.
                        let _ = write!(output, "\\u{point:04x}");
                    } else {
                        let value = point - 0x10000;
                        // Formatting into String cannot fail.
                        let _ = write!(
                            output,
                            "\\u{:04x}\\u{:04x}",
                            0xd800 + (value >> 10),
                            0xdc00 + (value & 1023)
                        );
                    }
                }
            }
        }
        output.push('"');
    }
}

/// JSON values whose strings do not discard unpaired surrogates.
#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Number(serde_json::Number),
    String(Text),
    Array(Vec<Json>),
    Object(std::collections::BTreeMap<Text, Json>),
}

impl Json {
    pub fn parse(input: &[u8]) -> io::Result<Self> {
        let mut parser = Parser { input, position: 0 };
        let value = parser.value(0)?;
        parser.space();
        if parser.position != input.len() {
            return Err(io::Error::other("trailing JSON data"));
        }
        Ok(value)
    }

    pub fn encode(&self) -> String {
        let mut output = String::new();
        self.render(&mut output);
        output
    }

    fn render(&self, output: &mut String) {
        match self {
            Self::Null => output.push_str("null"),
            Self::Bool(value) => output.push_str(if *value { "true" } else { "false" }),
            Self::Number(value) => output.push_str(&value.to_string()),
            Self::String(value) => value.json(output),
            Self::Array(values) => {
                output.push('[');
                for (index, value) in values.iter().enumerate() {
                    if index != 0 {
                        output.push(',');
                    }
                    value.render(output);
                }
                output.push(']');
            }
            Self::Object(values) => {
                output.push('{');
                for (index, (key, value)) in values.iter().enumerate() {
                    if index != 0 {
                        output.push(',');
                    }
                    key.json(output);
                    output.push(':');
                    value.render(output);
                }
                output.push('}');
            }
        }
    }

    pub fn get(&self, key: &str) -> io::Result<&Self> {
        match self {
            Self::Object(values) => values
                .get(&Text::from_utf8(key))
                .ok_or_else(|| io::Error::other(format!("missing JSON field: {key}"))),
            _ => Err(io::Error::other("expected JSON object")),
        }
    }

    pub fn text(&self) -> io::Result<&Text> {
        match self {
            Self::String(value) => Ok(value),
            _ => Err(io::Error::other("expected JSON string")),
        }
    }

    pub fn array(&self) -> io::Result<&[Self]> {
        match self {
            Self::Array(value) => Ok(value),
            _ => Err(io::Error::other("expected JSON array")),
        }
    }

    pub fn integer(&self) -> io::Result<u64> {
        match self {
            Self::Number(value) => value
                .as_u64()
                .ok_or_else(|| io::Error::other("expected unsigned integer")),
            _ => Err(io::Error::other("expected JSON number")),
        }
    }
}

struct Parser<'a> {
    input: &'a [u8],
    position: usize,
}

impl Parser<'_> {
    fn space(&mut self) {
        while self
            .input
            .get(self.position)
            .is_some_and(|byte| b" \t\r\n".contains(byte))
        {
            self.position += 1;
        }
    }

    fn take(&mut self, byte: u8) -> io::Result<()> {
        self.space();
        if self.input.get(self.position) != Some(&byte) {
            return Err(io::Error::other("invalid JSON token"));
        }
        self.position += 1;
        Ok(())
    }

    fn hex(&mut self) -> io::Result<u32> {
        let end = self
            .position
            .checked_add(4)
            .ok_or_else(|| io::Error::other("JSON overflow"))?;
        let bytes = self
            .input
            .get(self.position..end)
            .ok_or_else(|| io::Error::other("short JSON escape"))?;
        let value = u32::from_str_radix(std::str::from_utf8(bytes).map_err(io::Error::other)?, 16)
            .map_err(io::Error::other)?;
        self.position = end;
        Ok(value)
    }

    fn string(&mut self) -> io::Result<Text> {
        self.take(b'"')?;
        let mut points = Vec::new();
        loop {
            let byte = *self
                .input
                .get(self.position)
                .ok_or_else(|| io::Error::other("unterminated JSON string"))?;
            self.position += 1;
            match byte {
                b'"' => return Ok(Text(points)),
                b'\\' => {
                    let escaped = *self
                        .input
                        .get(self.position)
                        .ok_or_else(|| io::Error::other("short JSON escape"))?;
                    self.position += 1;
                    let point = match escaped {
                        b'"' => 34,
                        b'\\' => 92,
                        b'/' => 47,
                        b'b' => 8,
                        b'f' => 12,
                        b'n' => 10,
                        b'r' => 13,
                        b't' => 9,
                        b'u' => {
                            let high = self.hex()?;
                            if (0xd800..=0xdbff).contains(&high)
                                && self.input.get(self.position..self.position + 2) == Some(b"\\u")
                            {
                                let saved = self.position;
                                self.position += 2;
                                let low = self.hex()?;
                                if (0xdc00..=0xdfff).contains(&low) {
                                    0x10000 + ((high - 0xd800) << 10) + low - 0xdc00
                                } else {
                                    self.position = saved;
                                    high
                                }
                            } else {
                                high
                            }
                        }
                        _ => return Err(io::Error::other("invalid JSON escape")),
                    };
                    points.push(point);
                }
                0..=31 => return Err(io::Error::other("control byte in JSON string")),
                32..=127 => points.push(u32::from(byte)),
                _ => {
                    self.position -= 1;
                    let tail = &self.input[self.position..];
                    let width = match byte {
                        0xc2..=0xdf => 2,
                        0xe0..=0xef => 3,
                        0xf0..=0xf4 => 4,
                        _ => return Err(io::Error::other("invalid JSON UTF-8")),
                    };
                    let text = std::str::from_utf8(
                        tail.get(..width)
                            .ok_or_else(|| io::Error::other("short UTF-8"))?,
                    )
                    .map_err(io::Error::other)?;
                    points.extend(text.chars().map(u32::from));
                    self.position += width;
                }
            }
        }
    }

    fn value(&mut self, depth: usize) -> io::Result<Json> {
        if depth > 128 {
            return Err(io::Error::other("JSON nesting limit"));
        }
        self.space();
        match self.input.get(self.position).copied() {
            Some(b'"') => Ok(Json::String(self.string()?)),
            Some(b'[') => {
                self.position += 1;
                self.space();
                let mut values = Vec::new();
                if self.input.get(self.position) != Some(&b']') {
                    loop {
                        values.push(self.value(depth + 1)?);
                        self.space();
                        if self.input.get(self.position) != Some(&b',') {
                            break;
                        }
                        self.position += 1;
                    }
                }
                self.take(b']')?;
                Ok(Json::Array(values))
            }
            Some(b'{') => {
                self.position += 1;
                self.space();
                let mut values = std::collections::BTreeMap::new();
                if self.input.get(self.position) != Some(&b'}') {
                    loop {
                        let key = self.string()?;
                        self.take(b':')?;
                        values.insert(key, self.value(depth + 1)?);
                        self.space();
                        if self.input.get(self.position) != Some(&b',') {
                            break;
                        }
                        self.position += 1;
                    }
                }
                self.take(b'}')?;
                Ok(Json::Object(values))
            }
            Some(_) => {
                let start = self.position;
                while self
                    .input
                    .get(self.position)
                    .is_some_and(|byte| !b" \r\n\t,]}".contains(byte))
                {
                    self.position += 1;
                }
                let value: serde_json::Value =
                    serde_json::from_slice(&self.input[start..self.position])
                        .map_err(io::Error::other)?;
                match value {
                    serde_json::Value::Null => Ok(Json::Null),
                    serde_json::Value::Bool(value) => Ok(Json::Bool(value)),
                    serde_json::Value::Number(value) => Ok(Json::Number(value)),
                    _ => Err(io::Error::other("invalid JSON atom")),
                }
            }
            None => Err(io::Error::other("missing JSON value")),
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn all_filename_bytes_roundtrip_through_surrogateescape_and_json() {
        let bytes: Vec<_> = (0..=255).collect();
        let text = Text::from_bytes(&bytes);
        assert_eq!(text.bytes().unwrap(), bytes);
        let encoded = Json::String(text.clone()).encode();
        assert_eq!(Json::parse(encoded.as_bytes()).unwrap(), Json::String(text));
    }

    #[test]
    fn json_preserves_unpaired_surrogates_without_conflating_private_unicode() {
        let input = br#"["\ud800","\udcff","\ud83d\ude00","\ue000","\ud800x","\ud800\u0041"]"#;
        let value = Json::parse(input).unwrap();
        assert_eq!(Json::parse(value.encode().as_bytes()).unwrap(), value);
        assert!(value.array().unwrap()[0].text().unwrap().bytes().is_err());
        assert_eq!(
            value.array().unwrap()[1].text().unwrap().bytes().unwrap(),
            [255]
        );
        assert_eq!(
            value.array().unwrap()[2].text().unwrap().unicode().unwrap(),
            "😀"
        );
    }

    #[test]
    fn unsafe_byte_paths_are_rejected_and_normalized_duplicates_agree() {
        assert!(Text::from_bytes(b"a/../\xff").normalized().is_err());
        assert_eq!(
            Text::from_bytes(b"//./a//\xff/").normalized().unwrap(),
            Text::from_bytes(b"a/\xff")
        );
    }

    #[test]
    fn malformed_json_is_not_accepted() {
        for input in [
            b"[1,]".as_slice(),
            b"{\"x\":0,}",
            b"\"\xff\"",
            b"\"\\u123z\"",
            b"01",
            b"true false",
        ] {
            assert!(Json::parse(input).is_err(), "{input:?}");
        }
    }
}
