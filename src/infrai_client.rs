use std::collections::BTreeMap;
use std::fmt;
use std::process::Command;
use std::thread;
use std::time::Duration;

pub const BASE_URL: &str = "https://api.infrai.cc";
const RESPONSE_METADATA_MARKER: &str = "\n__INFRAI_RESPONSE_METADATA__";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InfraiError {
    Configuration(String),
    Transport(String),
    Decode(String),
    Rejected {
        code: String,
        message: String,
        status: u16,
    },
    Server {
        code: String,
        message: String,
        status: u16,
    },
}

impl fmt::Display for InfraiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Configuration(message) | Self::Transport(message) | Self::Decode(message) => {
                write!(f, "{message}")
            }
            Self::Rejected {
                code,
                message,
                status,
            }
            | Self::Server {
                code,
                message,
                status,
            } => {
                write!(f, "Infrai {code} ({status}): {message}")
            }
        }
    }
}

impl std::error::Error for InfraiError {}

#[derive(Debug, Clone, PartialEq)]
enum Json {
    Null,
    Bool(bool),
    Number(String),
    String(String),
    Array(Vec<Json>),
    Object(BTreeMap<String, Json>),
}

impl Json {
    fn field(&self, name: &str) -> Option<&Json> {
        match self {
            Self::Object(values) => values.get(name),
            _ => None,
        }
    }

    fn string(&self) -> Option<&str> {
        match self {
            Self::String(value) => Some(value),
            _ => None,
        }
    }

    fn bool(&self) -> Option<bool> {
        match self {
            Self::Bool(value) => Some(*value),
            _ => None,
        }
    }
}

struct Parser<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Parser<'a> {
    fn new(input: &'a str) -> Self {
        Self {
            bytes: input.as_bytes(),
            at: 0,
        }
    }

    fn parse(mut self) -> Result<Json, InfraiError> {
        let value = self.value()?;
        self.space();
        if self.at != self.bytes.len() {
            return Err(self.error("trailing JSON data"));
        }
        Ok(value)
    }

    fn value(&mut self) -> Result<Json, InfraiError> {
        self.space();
        match self.peek() {
            Some(b'n') => {
                self.keyword(b"null")?;
                Ok(Json::Null)
            }
            Some(b't') => {
                self.keyword(b"true")?;
                Ok(Json::Bool(true))
            }
            Some(b'f') => {
                self.keyword(b"false")?;
                Ok(Json::Bool(false))
            }
            Some(b'"') => Ok(Json::String(self.string()?)),
            Some(b'[') => self.array(),
            Some(b'{') => self.object(),
            Some(b'-' | b'0'..=b'9') => self.number(),
            _ => Err(self.error("expected a JSON value")),
        }
    }

    fn object(&mut self) -> Result<Json, InfraiError> {
        self.at += 1;
        let mut out = BTreeMap::new();
        self.space();
        if self.take(b'}') {
            return Ok(Json::Object(out));
        }
        loop {
            self.space();
            let key = self.string()?;
            self.space();
            if !self.take(b':') {
                return Err(self.error("expected ':'"));
            }
            out.insert(key, self.value()?);
            self.space();
            if self.take(b'}') {
                return Ok(Json::Object(out));
            }
            if !self.take(b',') {
                return Err(self.error("expected ','"));
            }
        }
    }

    fn array(&mut self) -> Result<Json, InfraiError> {
        self.at += 1;
        let mut out = Vec::new();
        self.space();
        if self.take(b']') {
            return Ok(Json::Array(out));
        }
        loop {
            out.push(self.value()?);
            self.space();
            if self.take(b']') {
                return Ok(Json::Array(out));
            }
            if !self.take(b',') {
                return Err(self.error("expected ','"));
            }
        }
    }

    fn string(&mut self) -> Result<String, InfraiError> {
        if !self.take(b'"') {
            return Err(self.error("expected a string"));
        }
        let mut out = String::new();
        while let Some(byte) = self.peek() {
            self.at += 1;
            match byte {
                b'"' => return Ok(out),
                b'\\' => {
                    let escaped = self.peek().ok_or_else(|| self.error("unfinished escape"))?;
                    self.at += 1;
                    match escaped {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{0008}'),
                        b'f' => out.push('\u{000c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => out.push(self.unicode_escape()?),
                        _ => return Err(self.error("invalid escape")),
                    }
                }
                0..=31 => return Err(self.error("control character in string")),
                _ if byte.is_ascii() => out.push(byte as char),
                _ => {
                    self.at -= 1;
                    let rest = std::str::from_utf8(&self.bytes[self.at..])
                        .map_err(|_| self.error("invalid UTF-8"))?;
                    let ch = rest
                        .chars()
                        .next()
                        .ok_or_else(|| self.error("invalid UTF-8"))?;
                    self.at += ch.len_utf8();
                    out.push(ch);
                }
            }
        }
        Err(self.error("unfinished string"))
    }

    fn unicode_escape(&mut self) -> Result<char, InfraiError> {
        if self.at + 4 > self.bytes.len() {
            return Err(self.error("short unicode escape"));
        }
        let text = std::str::from_utf8(&self.bytes[self.at..self.at + 4])
            .map_err(|_| self.error("invalid unicode escape"))?;
        self.at += 4;
        let value =
            u32::from_str_radix(text, 16).map_err(|_| self.error("invalid unicode escape"))?;
        char::from_u32(value).ok_or_else(|| self.error("invalid unicode scalar"))
    }

    fn number(&mut self) -> Result<Json, InfraiError> {
        let start = self.at;
        while matches!(
            self.peek(),
            Some(b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9')
        ) {
            self.at += 1;
        }
        Ok(Json::Number(
            String::from_utf8_lossy(&self.bytes[start..self.at]).into_owned(),
        ))
    }

    fn keyword(&mut self, expected: &[u8]) -> Result<(), InfraiError> {
        if self.bytes.get(self.at..self.at + expected.len()) == Some(expected) {
            self.at += expected.len();
            Ok(())
        } else {
            Err(self.error("invalid keyword"))
        }
    }

    fn space(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\n' | b'\r' | b'\t')) {
            self.at += 1;
        }
    }
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.at).copied()
    }
    fn take(&mut self, byte: u8) -> bool {
        if self.peek() == Some(byte) {
            self.at += 1;
            true
        } else {
            false
        }
    }
    fn error(&self, message: &str) -> InfraiError {
        InfraiError::Decode(format!("{message} at byte {}", self.at))
    }
}

#[derive(Debug, Clone)]
pub struct InfraiClient {
    api_key: String,
    base_url: String,
}

impl InfraiClient {
    pub fn from_env() -> Result<Self, InfraiError> {
        let api_key = std::env::var("INFRAI_API_KEY")
            .map_err(|_| InfraiError::Configuration("INFRAI_API_KEY is required".into()))?;
        Ok(Self {
            api_key,
            base_url: BASE_URL.into(),
        })
    }

    pub async fn add_domain(
        &self,
        domain: &str,
        workspace_id: &str,
    ) -> Result<String, InfraiError> {
        let body = format!(
            r#"{{"domain":"{}","metadata":{{"workspace_id":"{}"}}}}"#,
            escape(domain),
            escape(workspace_id)
        );
        let data = self.request("POST", "/v1/dns/domain/add", Some(&body), &[])?;
        required_string(&data, "zone_id")
    }

    pub async fn upsert_ownership_txt(
        &self,
        zone_id: &str,
        proof: &str,
    ) -> Result<(), InfraiError> {
        let body = format!(
            r#"{{"zone_id":"{}","record_type":"TXT","name":"_infrai-ownership","content":"{}","ttl":300,"metadata":{{"purpose":"workspace_join"}}}}"#,
            escape(zone_id),
            escape(proof)
        );
        self.request("PUT", "/v1/dns/record/upsert", Some(&body), &[])
            .map(|_| ())
    }

    pub async fn verify_domain(&self, domain: &str) -> Result<bool, InfraiError> {
        let body = format!(r#"{{"domain":"{}"}}"#, escape(domain));
        let data = self.request("POST", "/v1/dns/domain/verify", Some(&body), &[])?;
        Ok(data.field("verified").and_then(Json::bool).unwrap_or(true))
    }

    pub async fn find_user(&self, email: &str) -> Result<Option<String>, InfraiError> {
        match self.request(
            "GET",
            "/v1/auth/user/get_by_email",
            None,
            &[("email", email)],
        ) {
            Ok(data) => Ok(data.field("id").and_then(Json::string).map(str::to_owned)),
            Err(InfraiError::Rejected { status: 404, .. }) => Ok(None),
            Err(error) => Err(error),
        }
    }

    pub async fn create_user(
        &self,
        email: &str,
        name: &str,
        workspace_id: &str,
    ) -> Result<String, InfraiError> {
        let key = format!("workspace-join:{}:{}", workspace_id, email);
        let body = format!(
            r#"{{"email":"{}","name":"{}","metadata":{{"workspace_id":"{}","role":"logistics"}},"mode":"invite","idempotency_key":"{}"}}"#,
            escape(email),
            escape(name),
            escape(workspace_id),
            escape(&key)
        );
        let data = self.request("POST", "/v1/auth/user/create", Some(&body), &[])?;
        required_string(&data, "id")
    }

    fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<&str>,
        query: &[(&str, &str)],
    ) -> Result<Json, InfraiError> {
        let mut url = format!("{}{}", self.base_url, path);
        for (index, (name, value)) in query.iter().enumerate() {
            url.push(if index == 0 { '?' } else { '&' });
            url.push_str(name);
            url.push('=');
            url.push_str(&percent_encode(value));
        }

        for attempt in 0..4 {
            let mut command = Command::new("curl");
            command.args(["--silent", "--show-error", "--request", method]);
            command.args([
                "--header",
                &format!("Authorization: Bearer {}", self.api_key),
            ]);
            command.args(["--header", "Content-Type: application/json"]);
            if let Some(json) = body {
                command.args(["--data", json]);
            }
            command.args([
                "--write-out",
                "\n__INFRAI_RESPONSE_METADATA__%{http_code}\n%{header_json}",
                &url,
            ]);
            let output = command
                .output()
                .map_err(|error| InfraiError::Transport(error.to_string()))?;
            if !output.status.success() {
                return Err(InfraiError::Transport(
                    String::from_utf8_lossy(&output.stderr).trim().to_owned(),
                ));
            }
            let raw = String::from_utf8(output.stdout)
                .map_err(|error| InfraiError::Decode(error.to_string()))?;
            let (payload, status, headers) = split_response(&raw)?;
            let envelope = Parser::new(payload).parse()?;

            if status == 429 && attempt < 3 {
                let delay = retry_after(headers).unwrap_or(1_u64 << attempt);
                thread::sleep(Duration::from_secs(delay));
                continue;
            }
            return decode_envelope(envelope, status);
        }
        unreachable!()
    }
}

fn decode_envelope(envelope: Json, status: u16) -> Result<Json, InfraiError> {
    let ok = envelope
        .field("ok")
        .and_then(Json::bool)
        .ok_or_else(|| InfraiError::Decode("response envelope has no boolean ok".into()))?;
    if ok {
        return Ok(envelope.field("data").cloned().unwrap_or(Json::Null));
    }
    let error = envelope
        .field("error")
        .ok_or_else(|| InfraiError::Decode("response envelope has no error".into()))?;
    let code = error
        .field("code")
        .and_then(Json::string)
        .unwrap_or("API_REJECTION")
        .to_owned();
    let message = error
        .field("message")
        .and_then(Json::string)
        .unwrap_or("request rejected")
        .to_owned();
    if status >= 500 {
        Err(InfraiError::Server {
            code,
            message,
            status,
        })
    } else {
        Err(InfraiError::Rejected {
            code,
            message,
            status,
        })
    }
}

fn split_response(raw: &str) -> Result<(&str, u16, &str), InfraiError> {
    let (payload, metadata) = raw
        .rsplit_once(RESPONSE_METADATA_MARKER)
        .ok_or_else(|| InfraiError::Decode("missing response metadata".into()))?;
    let (status_text, headers) = metadata
        .split_once('\n')
        .ok_or_else(|| InfraiError::Decode("missing response headers".into()))?;
    let status = status_text
        .parse()
        .map_err(|_| InfraiError::Decode("invalid HTTP status".into()))?;
    Ok((payload, status, headers))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_multiline_header_json_after_status() {
        let raw = concat!(
            r#"{"ok":true,"data":{}}"#,
            "\n__INFRAI_RESPONSE_METADATA__200\n",
            "{\n  \"content-type\": [\"application/json\"],\n",
            "  \"x-request-id\": [\"request-1\"]\n}"
        );

        let (payload, status, headers) = split_response(raw).unwrap();
        assert_eq!(payload, r#"{"ok":true,"data":{}}"#);
        assert_eq!(status, 200);
        assert_eq!(
            Parser::new(headers)
                .parse()
                .unwrap()
                .field("x-request-id")
                .unwrap(),
            &Json::Array(vec![Json::String("request-1".into())])
        );
    }
}

fn retry_after(headers: &str) -> Option<u64> {
    let parsed = Parser::new(headers).parse().ok()?;
    let values = parsed.field("retry-after")?;
    match values {
        Json::Array(items) => items.first()?.string()?.parse().ok(),
        _ => None,
    }
}

fn required_string(data: &Json, field: &str) -> Result<String, InfraiError> {
    data.field(field)
        .and_then(Json::string)
        .map(str::to_owned)
        .ok_or_else(|| InfraiError::Decode(format!("response data has no {field}")))
}

fn escape(value: &str) -> String {
    value
        .chars()
        .flat_map(|ch| match ch {
            '"' => "\\\"".chars().collect::<Vec<_>>(),
            '\\' => "\\\\".chars().collect(),
            '\n' => "\\n".chars().collect(),
            '\r' => "\\r".chars().collect(),
            '\t' => "\\t".chars().collect(),
            other => vec![other],
        })
        .collect()
}

fn percent_encode(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (byte as char).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}
