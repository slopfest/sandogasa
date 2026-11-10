// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Minimal XML-RPC client using reqwest + quick-xml.

use std::collections::HashMap;
use std::fmt;

use quick_xml::Reader;
use quick_xml::events::{BytesRef, Event};

/// An XML-RPC value.
#[derive(Debug, Clone)]
pub enum Value {
    Int(i64),
    String(String),
    Boolean(bool),
    Double(f64),
    Array(Vec<Value>),
    Struct(HashMap<String, Value>),
    Nil,
}

impl Value {
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Value::Int(i) => Some(*i),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Value]> {
        match self {
            Value::Array(a) => Some(a),
            _ => None,
        }
    }

    pub fn as_struct(&self) -> Option<&HashMap<String, Value>> {
        match self {
            Value::Struct(s) => Some(s),
            _ => None,
        }
    }

    /// Look up a field in a struct value.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.as_struct()?.get(key)
    }
}

#[derive(Debug)]
pub enum Error {
    Http(reqwest::Error),
    Xml(String),
    Fault { code: i64, message: String },
    Parse(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Http(e) => {
                // reqwest's Display hides the cause ("error
                // sending request"); walk the chain so transport
                // failures are actually diagnosable.
                write!(f, "HTTP error: {e}")?;
                let mut source = std::error::Error::source(e);
                while let Some(cause) = source {
                    write!(f, ": {cause}")?;
                    source = cause.source();
                }
                Ok(())
            }
            Error::Xml(e) => write!(f, "XML error: {e}"),
            Error::Fault { code, message } => {
                write!(f, "XML-RPC fault {code}: {message}")
            }
            Error::Parse(e) => write!(f, "parse error: {e}"),
        }
    }
}

impl std::error::Error for Error {}

impl Error {
    /// Return `true` if this error is likely transient and the
    /// operation should be retried (e.g. HTTP 502/503/504).
    pub fn is_retriable(&self) -> bool {
        match self {
            // Connection or transport failures are often transient.
            Error::Http(_) => true,
            // XML parse failures after a successful HTTP request
            // usually mean we got an HTML error page instead of XML
            // (e.g. a 502 gateway error page).
            Error::Xml(_) => true,
            // Koji CLI errors mentioning server errors.
            Error::Parse(msg) => {
                msg.contains("502")
                    || msg.contains("503")
                    || msg.contains("504")
                    || msg.contains("Bad Gateway")
                    || msg.contains("Service Unavailable")
                    || msg.contains("Gateway Timeout")
            }
            // XML-RPC faults are legitimate API errors.
            Error::Fault { .. } => false,
        }
    }
}

impl From<reqwest::Error> for Error {
    fn from(e: reqwest::Error) -> Self {
        Error::Http(e)
    }
}

impl From<quick_xml::Error> for Error {
    fn from(e: quick_xml::Error) -> Self {
        Error::Xml(e.to_string())
    }
}

/// Default bound on a single hub request.
///
/// **Sized for the first page into a cold region, not for the steady
/// state.** Those differ by most of an order of magnitude and the steady
/// state is what a casual measurement finds: listing January 2025 at
/// twenty months' depth settled at 27-34s a 4000-row page across nine
/// consecutive pages, while the first page into that stretch exceeded
/// 180s and was abandoned there.
///
/// The two failure modes are not symmetric, which is why this errs high.
/// Too low discards work that was nearly done and charges the hub for it
/// again — three times, with retries — and a deep sweep can stop making
/// progress rather than merely slow down. Too high only delays noticing a
/// genuine hang, which the duty-cycle pacing and the retry budget already
/// tolerate.
pub const DEFAULT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);

/// Environment variable overriding [`DEFAULT_TIMEOUT`], in seconds.
///
/// The same variable `sandogasa-koji` uses for the `koji` CLI,
/// deliberately: it is one knob for "how long may a Koji request take",
/// and whoever has had to raise it for one path usually means both. A
/// value of `0` waits forever, for a caller willing to block.
///
/// A setting and its override belong together — a crate that takes the
/// first without the second leaves the caller with a wall and no door.
pub const TIMEOUT_ENV: &str = "SANDOGASA_KOJI_TIMEOUT";

/// The configured request bound: the environment if it says so, else
/// [`DEFAULT_TIMEOUT`]. `None` means no bound at all.
pub fn configured_timeout() -> Option<std::time::Duration> {
    match std::env::var(TIMEOUT_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
    {
        Some(0) => None,
        Some(secs) => Some(std::time::Duration::from_secs(secs)),
        None => Some(DEFAULT_TIMEOUT),
    }
}

/// XML-RPC client that communicates with a hub endpoint.
pub struct Client {
    http: reqwest::blocking::Client,
    hub_url: String,
}

impl Client {
    /// A client bound by [`configured_timeout`].
    pub fn new(hub_url: &str) -> Self {
        Self::with_timeout(hub_url, configured_timeout())
    }

    /// A client bound by `timeout`, or unbounded when it is `None`.
    ///
    /// For a caller that knows better than the environment does — a deep
    /// sweep that has measured what its own pages cost, say.
    pub fn with_timeout(hub_url: &str, timeout: Option<std::time::Duration>) -> Self {
        sandogasa_cli::install_crypto_provider();
        let builder = reqwest::blocking::Client::builder()
            // Fedora's infrastructure TARPITS requests without
            // a User-Agent: the same heavy listTasks query
            // completes with one and times out without one
            // (measured live, 2026-07). reqwest sends none by
            // default — always identify ourselves.
            .user_agent(concat!("sandogasa-kojihub/", env!("CARGO_PKG_VERSION")))
            // Match the koji CLI's HTTP/1.1; h2 to the hub
            // showed additional hangs during the same testing.
            .http1_only()
            // Fedora's proxy stack also stalls heavy queries
            // sent over a REUSED keep-alive connection
            // (reproduced with curl --next: fresh connection
            // ~3-90s, reused connection times out), so don't
            // pool connections at all — the handshake cost is
            // noise next to the queries and the polite pacing.
            .pool_max_idle_per_host(0);
        // Heavy queries (multi-MB decoded task pages) have been observed
        // taking 90+ seconds on a loaded hub and about 300s at twenty
        // months' depth. Past the bound the request is abandoned and the
        // retry pays the hub cost again, so too low a value stops a deep
        // sweep progressing rather than merely slowing it.
        //
        // Unbounded is the *absence* of a bound, not a very large one:
        // reqwest adds the duration to an `Instant`, and a sentinel near
        // `u64::MAX` overflows when it does.
        let builder = match timeout {
            Some(bound) => builder.timeout(bound),
            None => builder,
        };
        Self {
            http: builder.build().expect("build reqwest client"),
            // Normalised, so a hub given as ".../kojihub/" is the same
            // client as one given as ".../kojihu" — the difference is a
            // typo, not a different hub.
            hub_url: hub_url.trim_end_matches('/').to_string(),
        }
    }

    /// Call an XML-RPC method with the given parameters.
    /// The hub this client talks to, for callers that report it. Any
    /// trailing slash has been trimmed.
    pub fn hub_url(&self) -> &str {
        &self.hub_url
    }

    /// Several calls in one request, through the hub's `multiCall`:
    /// one round trip instead of one each. The outer `Err` is the
    /// request failing; each inner result is one call's answer, or its
    /// fault, in the order the calls were given.
    pub fn multicall(
        &self,
        calls: &[(&str, Vec<Value>)],
    ) -> Result<Vec<Result<Value, Error>>, Error> {
        if calls.is_empty() {
            return Ok(Vec::new());
        }
        let list = calls
            .iter()
            .map(|(method, params)| {
                Value::Struct(HashMap::from([
                    ("methodName".to_string(), Value::String(method.to_string())),
                    ("params".to_string(), Value::Array(params.clone())),
                ]))
            })
            .collect();
        let answer = self.call("multiCall", &[Value::Array(list)])?;
        let items = answer
            .as_array()
            .ok_or_else(|| Error::Parse("multiCall did not answer a list".into()))?;
        if items.len() != calls.len() {
            return Err(Error::Parse(format!(
                "multiCall answered {} results for {} calls",
                items.len(),
                calls.len()
            )));
        }
        Ok(items.iter().map(multicall_result).collect())
    }

    pub fn call(&self, method: &str, params: &[Value]) -> Result<Value, Error> {
        let body = build_request(method, params);
        let resp = self
            .http
            .post(&self.hub_url)
            .header("Content-Type", "text/xml")
            .body(body)
            .send()?
            // Surface HTTP-level failures (proxy 502s and friends)
            // as retriable Http errors instead of depending on
            // whatever body shape the error page happens to have.
            .error_for_status()?
            .text()?;
        parse_response(&resp)
    }
}

/// One entry of a `multiCall` answer: the result wrapped in a
/// one-element list, or a fault struct.
fn multicall_result(item: &Value) -> Result<Value, Error> {
    if let Some(code) = item.get("faultCode").and_then(Value::as_int) {
        let message = item
            .get("faultString")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        return Err(Error::Fault { code, message });
    }
    match item.as_array() {
        Some([result]) => Ok(result.clone()),
        _ => Err(Error::Parse(
            "multiCall entry is neither a result nor a fault".into(),
        )),
    }
}

// --- Request building ---

fn build_request(method: &str, params: &[Value]) -> String {
    let mut xml = format!(
        "<?xml version=\"1.0\"?>\n\
         <methodCall>\n\
         <methodName>{method}</methodName>\n\
         <params>\n"
    );
    for param in params {
        xml.push_str("<param>");
        write_value(&mut xml, param);
        xml.push_str("</param>\n");
    }
    xml.push_str("</params>\n</methodCall>");
    xml
}

fn write_value(xml: &mut String, value: &Value) {
    xml.push_str("<value>");
    match value {
        Value::Int(i) => {
            xml.push_str(&format!("<int>{i}</int>"));
        }
        Value::String(s) => {
            xml.push_str("<string>");
            xml.push_str(&quick_xml::escape::escape(s));
            xml.push_str("</string>");
        }
        Value::Boolean(b) => {
            xml.push_str(&format!("<boolean>{}</boolean>", if *b { 1 } else { 0 }));
        }
        Value::Double(d) => {
            xml.push_str(&format!("<double>{d}</double>"));
        }
        Value::Array(items) => {
            xml.push_str("<array><data>");
            for item in items {
                write_value(xml, item);
            }
            xml.push_str("</data></array>");
        }
        Value::Struct(map) => {
            xml.push_str("<struct>");
            for (key, val) in map {
                xml.push_str(&format!("<member><name>{key}</name>"));
                write_value(xml, val);
                xml.push_str("</member>");
            }
            xml.push_str("</struct>");
        }
        Value::Nil => {
            xml.push_str("<nil/>");
        }
    }
    xml.push_str("</value>");
}

// --- Response parsing ---

pub fn parse_response(xml: &str) -> Result<Value, Error> {
    // Text is not trimmed: a string's own spaces are part of it, and
    // quick-xml splits text at each entity (`Crha &lt;mcrha@…&gt;`), so
    // trimming each piece ate the spaces beside one. The parsers skip
    // the whitespace between elements themselves.
    let mut reader = Reader::from_str(xml);

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => match e.name().as_ref() {
                "params" => return parse_params(&mut reader),
                "fault" => return parse_fault(&mut reader),
                _ => {}
            },
            Ok(Event::Eof) => {
                return Err(Error::Parse("unexpected EOF before params or fault".into()));
            }
            Err(e) => return Err(Error::Xml(e.to_string())),
            _ => {}
        }
    }
}

fn parse_params(reader: &mut Reader<&[u8]>) -> Result<Value, Error> {
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) if e.name().as_ref() == "value" => {
                return parse_value_content(reader);
            }
            Ok(Event::End(e)) if e.name().as_ref() == "params" => {
                return Ok(Value::Nil);
            }
            Ok(Event::Eof) => {
                return Err(Error::Parse("unexpected EOF in params".into()));
            }
            Err(e) => return Err(e.into()),
            _ => {}
        }
    }
}

fn parse_fault(reader: &mut Reader<&[u8]>) -> Result<Value, Error> {
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) if e.name().as_ref() == "value" => {
                let value = parse_value_content(reader)?;
                let code = value
                    .get("faultCode")
                    .and_then(|v| v.as_int())
                    .unwrap_or(-1);
                let message = value
                    .get("faultString")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown fault")
                    .to_string();
                return Err(Error::Fault { code, message });
            }
            Ok(Event::Eof) => {
                return Err(Error::Parse("unexpected EOF in fault".into()));
            }
            Err(e) => return Err(e.into()),
            _ => {}
        }
    }
}

/// Parse the content inside a `<value>` tag (opening `<value>` already consumed).
fn parse_value_content(reader: &mut Reader<&[u8]>) -> Result<Value, Error> {
    // Text outside a type tag: whitespace before one, or a bare string,
    // which may be split at entities like any other text.
    let mut bare = String::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                let tag = e.name().as_ref().to_string();
                match tag.as_str() {
                    "int" | "i4" | "i8" => {
                        let text = read_text_content(reader)?;
                        let text = text.trim();
                        skip_to_end(reader, "value")?;
                        return Ok(Value::Int(text.parse::<i64>().map_err(|e| {
                            Error::Parse(format!("invalid int '{text}': {e}"))
                        })?));
                    }
                    "string" => {
                        let text = read_text_content(reader)?;
                        skip_to_end(reader, "value")?;
                        return Ok(Value::String(text));
                    }
                    "boolean" => {
                        let text = read_text_content(reader)?;
                        let text = text.trim();
                        skip_to_end(reader, "value")?;
                        return Ok(Value::Boolean(text == "1" || text == "true"));
                    }
                    "double" => {
                        let text = read_text_content(reader)?;
                        let text = text.trim();
                        skip_to_end(reader, "value")?;
                        return Ok(Value::Double(text.parse::<f64>().map_err(|e| {
                            Error::Parse(format!("invalid double '{text}': {e}"))
                        })?));
                    }
                    "struct" => {
                        let s = parse_struct_content(reader)?;
                        skip_to_end(reader, "value")?;
                        return Ok(s);
                    }
                    "array" => {
                        let a = parse_array_content(reader)?;
                        skip_to_end(reader, "value")?;
                        return Ok(a);
                    }
                    _ => {
                        // Unknown type tag (`base64`, `dateTime.iso8601`)
                        // — treat content as string. Python writes base64
                        // across lines, so its whitespace is not data.
                        let text = read_text_content(reader)?;
                        skip_to_end(reader, "value")?;
                        return Ok(Value::String(text.trim().to_string()));
                    }
                }
            }
            Ok(Event::Empty(e)) if e.name().as_ref() == "nil" => {
                skip_to_end(reader, "value")?;
                return Ok(Value::Nil);
            }
            // Bare text in <value> is implicitly a string; whitespace
            // alone is the indentation of an empty one.
            Ok(Event::End(e)) if e.name().as_ref() == "value" => {
                if bare.trim().is_empty() {
                    bare.clear();
                }
                return Ok(Value::String(bare));
            }
            Ok(ev) if push_text(&mut bare, &ev)? => {}
            Ok(Event::Eof) => {
                return Err(Error::Parse("unexpected EOF in value".into()));
            }
            Err(e) => return Err(e.into()),
            _ => {}
        }
    }
}

fn parse_struct_content(reader: &mut Reader<&[u8]>) -> Result<Value, Error> {
    let mut map = HashMap::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) if e.name().as_ref() == "member" => {
                let (name, value) = parse_member(reader)?;
                map.insert(name, value);
            }
            Ok(Event::End(e)) if e.name().as_ref() == "struct" => {
                return Ok(Value::Struct(map));
            }
            Ok(Event::Eof) => {
                return Err(Error::Parse("unexpected EOF in struct".into()));
            }
            Err(e) => return Err(e.into()),
            _ => {}
        }
    }
}

fn parse_member(reader: &mut Reader<&[u8]>) -> Result<(String, Value), Error> {
    let mut name = String::new();
    let mut value = Value::Nil;
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => match e.name().as_ref() {
                "name" => {
                    name = read_text_content(reader)?.trim().to_string();
                }
                "value" => {
                    value = parse_value_content(reader)?;
                }
                _ => {}
            },
            Ok(Event::End(e)) if e.name().as_ref() == "member" => {
                return Ok((name, value));
            }
            Ok(Event::Eof) => {
                return Err(Error::Parse("unexpected EOF in member".into()));
            }
            Err(e) => return Err(e.into()),
            _ => {}
        }
    }
}

fn parse_array_content(reader: &mut Reader<&[u8]>) -> Result<Value, Error> {
    let mut items = Vec::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                if e.name().as_ref() == "data" {
                    loop {
                        match reader.read_event() {
                            Ok(Event::Start(e2)) if e2.name().as_ref() == "value" => {
                                items.push(parse_value_content(reader)?);
                            }
                            Ok(Event::End(e2)) if e2.name().as_ref() == "data" => break,
                            Ok(Event::Eof) => {
                                return Err(Error::Parse("unexpected EOF in array data".into()));
                            }
                            Err(e) => return Err(e.into()),
                            _ => {}
                        }
                    }
                }
            }
            Ok(Event::End(e)) if e.name().as_ref() == "array" => {
                return Ok(Value::Array(items));
            }
            Ok(Event::Eof) => {
                return Err(Error::Parse("unexpected EOF in array".into()));
            }
            Err(e) => return Err(e.into()),
            _ => {}
        }
    }
}

/// What an entity or character reference stands for: `&lt;` is `<`,
/// `&#233;` is `é`.
fn resolve_ref(r: &BytesRef) -> Result<String, Error> {
    if let Some(c) = r
        .resolve_char_ref()
        .map_err(|e| Error::Xml(e.to_string()))?
    {
        return Ok(c.to_string());
    }
    quick_xml::escape::resolve_predefined_entity(r)
        .map(str::to_string)
        .ok_or_else(|| Error::Parse(format!("unknown entity &{};", &**r)))
}

/// Append to `buf` the text `event` carries, if it carries any, and say
/// whether it did. quick-xml 0.42 splits text at every reference, which
/// arrives as an event of its own (`Crha &lt;mcrha@…&gt;` is a text, a
/// reference, a text, …), and CDATA is another; a reader collecting an
/// element's text has to take all three, and trim only the whole.
pub fn push_text(buf: &mut String, event: &Event) -> Result<bool, Error> {
    match event {
        Event::Text(t) => buf.push_str(t),
        Event::CData(c) => buf.push_str(c),
        Event::GeneralRef(r) => buf.push_str(&resolve_ref(r)?),
        _ => return Ok(false),
    }
    Ok(true)
}

/// Read text content until the matching end tag.
fn read_text_content(reader: &mut Reader<&[u8]>) -> Result<String, Error> {
    let mut text = String::new();
    loop {
        match reader.read_event() {
            Ok(Event::End(_)) => return Ok(text),
            Ok(Event::Eof) => {
                return Err(Error::Parse("unexpected EOF reading text".into()));
            }
            Ok(ev) => {
                push_text(&mut text, &ev)?;
            }
            Err(e) => return Err(e.into()),
        }
    }
}

/// Skip events until we find an end tag matching the given name at depth 0.
fn skip_to_end(reader: &mut Reader<&[u8]>, end_tag: &str) -> Result<(), Error> {
    let mut depth: u32 = 0;
    loop {
        match reader.read_event() {
            Ok(Event::Start(_)) => depth += 1,
            Ok(Event::End(e)) => {
                if depth == 0 && e.name().as_ref() == end_tag {
                    return Ok(());
                }
                depth = depth.saturating_sub(1);
            }
            Ok(Event::Eof) => return Ok(()),
            Err(e) => return Err(e.into()),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_request_int() {
        let xml = build_request("getBuild", &[Value::Int(42)]);
        assert!(xml.contains("<methodName>getBuild</methodName>"));
        assert!(xml.contains("<int>42</int>"));
    }

    #[test]
    fn test_build_request_string_escaping() {
        let xml = build_request("test", &[Value::String("a<b&c".into())]);
        assert!(xml.contains("a&lt;b&amp;c"));
    }

    #[test]
    fn test_parse_int_response() {
        let xml = r#"<?xml version="1.0"?>
            <methodResponse><params><param>
            <value><int>42</int></value>
            </param></params></methodResponse>"#;
        let v = parse_response(xml).unwrap();
        assert_eq!(v.as_int(), Some(42));
    }

    #[test]
    fn test_parse_string_response() {
        let xml = r#"<?xml version="1.0"?>
            <methodResponse><params><param>
            <value><string>hello world</string></value>
            </param></params></methodResponse>"#;
        let v = parse_response(xml).unwrap();
        assert_eq!(v.as_str(), Some("hello world"));
    }

    #[test]
    fn test_parse_struct_response() {
        let xml = r#"<?xml version="1.0"?>
            <methodResponse><params><param>
            <value><struct>
                <member><name>task_id</name><value><int>12345</int></value></member>
                <member><name>nvr</name><value><string>foo-1.0-1.fc42</string></value></member>
            </struct></value>
            </param></params></methodResponse>"#;
        let v = parse_response(xml).unwrap();
        assert_eq!(v.get("task_id").unwrap().as_int(), Some(12345));
        assert_eq!(v.get("nvr").unwrap().as_str(), Some("foo-1.0-1.fc42"));
    }

    #[test]
    fn test_parse_array_response() {
        let xml = r#"<?xml version="1.0"?>
            <methodResponse><params><param>
            <value><array><data>
                <value><int>1</int></value>
                <value><int>2</int></value>
                <value><int>3</int></value>
            </data></array></value>
            </param></params></methodResponse>"#;
        let v = parse_response(xml).unwrap();
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 3);
        assert_eq!(arr[0].as_int(), Some(1));
        assert_eq!(arr[2].as_int(), Some(3));
    }

    #[test]
    fn test_parse_nested_struct_array() {
        let xml = r#"<?xml version="1.0"?>
            <methodResponse><params><param>
            <value><array><data>
                <value><struct>
                    <member><name>id</name><value><int>100</int></value></member>
                    <member><name>method</name><value><string>buildArch</string></value></member>
                    <member><name>arch</name><value><string>x86_64</string></value></member>
                </struct></value>
                <value><struct>
                    <member><name>id</name><value><int>101</int></value></member>
                    <member><name>method</name><value><string>buildArch</string></value></member>
                    <member><name>arch</name><value><string>aarch64</string></value></member>
                </struct></value>
            </data></array></value>
            </param></params></methodResponse>"#;
        let v = parse_response(xml).unwrap();
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0].get("id").unwrap().as_int(), Some(100));
        assert_eq!(arr[0].get("arch").unwrap().as_str(), Some("x86_64"));
        assert_eq!(arr[1].get("id").unwrap().as_int(), Some(101));
    }

    #[test]
    fn test_parse_boolean_response() {
        let xml = r#"<?xml version="1.0"?>
            <methodResponse><params><param>
            <value><boolean>1</boolean></value>
            </param></params></methodResponse>"#;
        let v = parse_response(xml).unwrap();
        assert_eq!(v.as_int(), None);
        match v {
            Value::Boolean(b) => assert!(b),
            _ => panic!("expected boolean"),
        }
    }

    #[test]
    fn test_parse_nil_response() {
        let xml = r#"<?xml version="1.0"?>
            <methodResponse><params><param>
            <value><nil/></value>
            </param></params></methodResponse>"#;
        let v = parse_response(xml).unwrap();
        assert!(matches!(v, Value::Nil));
    }

    #[test]
    fn test_parse_fault_response() {
        let xml = r#"<?xml version="1.0"?>
            <methodResponse><fault><value><struct>
                <member><name>faultCode</name><value><int>1000</int></value></member>
                <member><name>faultString</name><value><string>No such build</string></value></member>
            </struct></value></fault></methodResponse>"#;
        let err = parse_response(xml).unwrap_err();
        match err {
            Error::Fault { code, message } => {
                assert_eq!(code, 1000);
                assert_eq!(message, "No such build");
            }
            _ => panic!("expected Fault error, got: {err}"),
        }
    }

    /// Koji escapes `<`, `>` and `&` in every string, and quick-xml
    /// reports each reference as an event of its own: a changelog
    /// author came back as `Milan Crhamcrha@redhat.com- 4.0.7-1`,
    /// references dropped and the spaces beside them trimmed away.
    #[test]
    fn a_string_keeps_its_escaped_characters_and_the_spaces_beside_them() {
        let xml = r#"<?xml version="1.0"?>
            <methodResponse><params><param>
            <value><string>Milan Crha &lt;mcrha@redhat.com&gt; - 4.0.7-1</string></value>
            </param></params></methodResponse>"#;
        let v = parse_response(xml).unwrap();
        assert_eq!(v.as_str(), Some("Milan Crha <mcrha@redhat.com> - 4.0.7-1"));

        let xml = r#"<?xml version="1.0"?>
            <methodResponse><params><param>
            <value><string> a &amp; b &#233;&#x2014;&quot;c&apos; </string></value>
            </param></params></methodResponse>"#;
        let v = parse_response(xml).unwrap();
        assert_eq!(v.as_str(), Some(" a & b é—\"c' "));

        // Bare text is a string too, split at references the same way.
        let xml = r#"<?xml version="1.0"?>
            <methodResponse><params><param>
            <value>x &lt; y</value>
            </param></params></methodResponse>"#;
        assert_eq!(parse_response(xml).unwrap().as_str(), Some("x < y"));

        // Numbers and booleans read through surrounding whitespace.
        let xml = r#"<?xml version="1.0"?>
            <methodResponse><params><param>
            <value><array><data>
              <value><int> 42 </int></value>
              <value><boolean> 1 </boolean></value>
              <value><double> 2.5 </double></value>
            </data></array></value>
            </param></params></methodResponse>"#;
        let v = parse_response(xml).unwrap();
        let a = v.as_array().unwrap();
        assert_eq!(a[0].as_int(), Some(42));
        assert!(matches!(a[1], Value::Boolean(true)));
        assert!(matches!(a[2], Value::Double(d) if d == 2.5));

        // CDATA is text too; a member name and a base64 value are read
        // through whitespace; an empty value's indentation is not text.
        let xml = r#"<?xml version="1.0"?>
            <methodResponse><params><param>
            <value><struct>
              <member><name>
                a
              </name><value><string>x<![CDATA[<b>]]>y</string></value></member>
              <member><name>b</name><value><base64>
                QUJD
              </base64></value></member>
              <member><name>c</name><value>
              </value></member>
            </struct></value>
            </param></params></methodResponse>"#;
        let v = parse_response(xml).unwrap();
        assert_eq!(v.get("a").and_then(Value::as_str), Some("x<b>y"));
        assert_eq!(v.get("b").and_then(Value::as_str), Some("QUJD"));
        assert_eq!(v.get("c").and_then(Value::as_str), Some(""));

        // An entity XML does not define is an error, not dropped text.
        let xml = r#"<?xml version="1.0"?>
            <methodResponse><params><param>
            <value><string>&nbsp;</string></value>
            </param></params></methodResponse>"#;
        assert!(parse_response(xml).is_err());
    }

    #[test]
    fn test_parse_bare_text_as_string() {
        let xml = r#"<?xml version="1.0"?>
            <methodResponse><params><param>
            <value>bare text</value>
            </param></params></methodResponse>"#;
        let v = parse_response(xml).unwrap();
        assert_eq!(v.as_str(), Some("bare text"));
    }

    #[test]
    fn test_is_retriable_http_error() {
        // reqwest errors are retriable (connection failures, etc.)
        let err = Error::Xml("unexpected token".into());
        assert!(err.is_retriable());
    }

    #[test]
    fn test_is_retriable_parse_502() {
        let err = Error::Parse("koji download-logs failed: 502 Bad Gateway".into());
        assert!(err.is_retriable());
    }

    #[test]
    fn test_is_retriable_parse_503() {
        let err = Error::Parse("503 Service Unavailable".into());
        assert!(err.is_retriable());
    }

    #[test]
    fn test_is_not_retriable_fault() {
        let err = Error::Fault {
            code: 1000,
            message: "No such build".into(),
        };
        assert!(!err.is_retriable());
    }

    #[test]
    fn test_is_not_retriable_parse_not_found() {
        let err = Error::Parse("root.log not found for task 123".into());
        assert!(!err.is_retriable());
    }
}
