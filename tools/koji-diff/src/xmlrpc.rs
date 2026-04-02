// SPDX-License-Identifier: MPL-2.0

//! Minimal XML-RPC client using reqwest + quick-xml.

use std::collections::HashMap;
use std::fmt;

use quick_xml::Reader;
use quick_xml::events::Event;

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
            Error::Http(e) => write!(f, "HTTP error: {e}"),
            Error::Xml(e) => write!(f, "XML error: {e}"),
            Error::Fault { code, message } => {
                write!(f, "XML-RPC fault {code}: {message}")
            }
            Error::Parse(e) => write!(f, "parse error: {e}"),
        }
    }
}

impl std::error::Error for Error {}

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

/// XML-RPC client that communicates with a hub endpoint.
pub struct Client {
    http: reqwest::blocking::Client,
    hub_url: String,
}

impl Client {
    pub fn new(hub_url: &str) -> Self {
        Self {
            http: reqwest::blocking::Client::new(),
            hub_url: hub_url.to_string(),
        }
    }

    /// Call an XML-RPC method with the given parameters.
    pub fn call(&self, method: &str, params: &[Value]) -> Result<Value, Error> {
        let body = build_request(method, params);
        let resp = self
            .http
            .post(&self.hub_url)
            .header("Content-Type", "text/xml")
            .body(body)
            .send()?
            .text()?;
        parse_response(&resp)
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
            for ch in s.chars() {
                match ch {
                    '<' => xml.push_str("&lt;"),
                    '>' => xml.push_str("&gt;"),
                    '&' => xml.push_str("&amp;"),
                    _ => xml.push(ch),
                }
            }
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
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => match e.name().as_ref() {
                b"params" => return parse_params(&mut reader),
                b"fault" => return parse_fault(&mut reader),
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
            Ok(Event::Start(e)) if e.name().as_ref() == b"value" => {
                return parse_value_content(reader);
            }
            Ok(Event::End(e)) if e.name().as_ref() == b"params" => {
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
            Ok(Event::Start(e)) if e.name().as_ref() == b"value" => {
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
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                let tag = e.name().as_ref().to_vec();
                match tag.as_slice() {
                    b"int" | b"i4" | b"i8" => {
                        let text = read_text_content(reader)?;
                        skip_to_end(reader, b"value")?;
                        return Ok(Value::Int(text.parse::<i64>().map_err(|e| {
                            Error::Parse(format!("invalid int '{text}': {e}"))
                        })?));
                    }
                    b"string" => {
                        let text = read_text_content(reader)?;
                        skip_to_end(reader, b"value")?;
                        return Ok(Value::String(text));
                    }
                    b"boolean" => {
                        let text = read_text_content(reader)?;
                        skip_to_end(reader, b"value")?;
                        return Ok(Value::Boolean(text == "1" || text == "true"));
                    }
                    b"double" => {
                        let text = read_text_content(reader)?;
                        skip_to_end(reader, b"value")?;
                        return Ok(Value::Double(text.parse::<f64>().map_err(|e| {
                            Error::Parse(format!("invalid double '{text}': {e}"))
                        })?));
                    }
                    b"struct" => {
                        let s = parse_struct_content(reader)?;
                        skip_to_end(reader, b"value")?;
                        return Ok(s);
                    }
                    b"array" => {
                        let a = parse_array_content(reader)?;
                        skip_to_end(reader, b"value")?;
                        return Ok(a);
                    }
                    _ => {
                        // Unknown type tag — treat content as string.
                        let text = read_text_content(reader)?;
                        skip_to_end(reader, b"value")?;
                        return Ok(Value::String(text));
                    }
                }
            }
            Ok(Event::Empty(e)) if e.name().as_ref() == b"nil" => {
                skip_to_end(reader, b"value")?;
                return Ok(Value::Nil);
            }
            Ok(Event::Text(t)) => {
                let text = t
                    .unescape()
                    .map_err(|e| Error::Xml(e.to_string()))?
                    .into_owned();
                if !text.trim().is_empty() {
                    // Bare text in <value> is implicitly a string.
                    skip_to_end(reader, b"value")?;
                    return Ok(Value::String(text));
                }
            }
            Ok(Event::End(e)) if e.name().as_ref() == b"value" => {
                return Ok(Value::String(String::new()));
            }
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
            Ok(Event::Start(e)) if e.name().as_ref() == b"member" => {
                let (name, value) = parse_member(reader)?;
                map.insert(name, value);
            }
            Ok(Event::End(e)) if e.name().as_ref() == b"struct" => {
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
                b"name" => {
                    name = read_text_content(reader)?;
                }
                b"value" => {
                    value = parse_value_content(reader)?;
                }
                _ => {}
            },
            Ok(Event::End(e)) if e.name().as_ref() == b"member" => {
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
                if e.name().as_ref() == b"data" {
                    loop {
                        match reader.read_event() {
                            Ok(Event::Start(e2)) if e2.name().as_ref() == b"value" => {
                                items.push(parse_value_content(reader)?);
                            }
                            Ok(Event::End(e2)) if e2.name().as_ref() == b"data" => break,
                            Ok(Event::Eof) => {
                                return Err(Error::Parse("unexpected EOF in array data".into()));
                            }
                            Err(e) => return Err(e.into()),
                            _ => {}
                        }
                    }
                }
            }
            Ok(Event::End(e)) if e.name().as_ref() == b"array" => {
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

/// Read text content until the matching end tag.
fn read_text_content(reader: &mut Reader<&[u8]>) -> Result<String, Error> {
    let mut text = String::new();
    loop {
        match reader.read_event() {
            Ok(Event::Text(t)) => {
                text.push_str(&t.unescape().map_err(|e| Error::Xml(e.to_string()))?);
            }
            Ok(Event::End(_)) => return Ok(text),
            Ok(Event::Eof) => {
                return Err(Error::Parse("unexpected EOF reading text".into()));
            }
            Err(e) => return Err(e.into()),
            _ => {}
        }
    }
}

/// Skip events until we find an end tag matching the given name at depth 0.
fn skip_to_end(reader: &mut Reader<&[u8]>, end_tag: &[u8]) -> Result<(), Error> {
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

    #[test]
    fn test_parse_bare_text_as_string() {
        let xml = r#"<?xml version="1.0"?>
            <methodResponse><params><param>
            <value>bare text</value>
            </param></params></methodResponse>"#;
        let v = parse_response(xml).unwrap();
        assert_eq!(v.as_str(), Some("bare text"));
    }
}
