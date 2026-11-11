use std::fmt;

use serde_json::{Map, Value, value::RawValue};

use super::json::repr_value;

/// An owned JSON value whose destruction does not recurse through nested data.
pub struct JsonValue(Option<Value>);

impl JsonValue {
    pub fn from_value(value: Value) -> Self {
        Self(Some(value))
    }

    pub fn as_value(&self) -> &Value {
        self.0.as_ref().expect("JSON value is present")
    }

    fn as_value_mut(&mut self) -> &mut Value {
        self.0.as_mut().expect("JSON value is present")
    }

    pub fn as_object(&self) -> Option<&Map<String, Value>> {
        self.as_value().as_object()
    }

    pub(crate) fn into_object(self) -> Result<JsonObject, Self> {
        if self.as_object().is_some() {
            Ok(JsonObject(self))
        } else {
            Err(self)
        }
    }
}

fn drop_values(mut values: Vec<Value>) {
    while let Some(value) = values.pop() {
        match value {
            Value::Array(children) => values.extend(children),
            Value::Object(children) => values.extend(children.into_iter().map(|(_, value)| value)),
            _ => {}
        }
    }
}

impl Drop for JsonValue {
    fn drop(&mut self) {
        if let Some(value) = self.0.take() {
            drop_values(vec![value]);
        }
    }
}

impl fmt::Debug for JsonValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&repr_value(self.as_value()))
    }
}

fn equal_values(left: &Value, right: &Value) -> bool {
    let mut pending = vec![(left, right)];
    while let Some((left, right)) = pending.pop() {
        match (left, right) {
            (Value::Null, Value::Null) => {}
            (Value::Bool(left), Value::Bool(right)) if left == right => {}
            (Value::Number(left), Value::Number(right)) if left == right => {}
            (Value::String(left), Value::String(right)) if left == right => {}
            (Value::Array(left), Value::Array(right)) if left.len() == right.len() => {
                pending.extend(left.iter().zip(right));
            }
            (Value::Object(left), Value::Object(right)) if left.len() == right.len() => {
                for (key, left) in left {
                    let Some(right) = right.get(key) else {
                        return false;
                    };
                    pending.push((left, right));
                }
            }
            _ => return false,
        }
    }
    true
}

impl PartialEq for JsonValue {
    fn eq(&self, other: &Self) -> bool {
        equal_values(self.as_value(), other.as_value())
    }
}

impl PartialEq<Value> for JsonValue {
    fn eq(&self, other: &Value) -> bool {
        equal_values(self.as_value(), other)
    }
}

/// An owned object returned by the wire decoder and notification queue.
#[derive(Debug, PartialEq)]
pub struct JsonObject(JsonValue);

impl JsonObject {
    pub fn from_map(map: Map<String, Value>) -> Self {
        Self(JsonValue::from_value(Value::Object(map)))
    }

    pub fn as_value(&self) -> &Value {
        self.0.as_value()
    }

    pub fn as_map(&self) -> &Map<String, Value> {
        self.0.as_object().expect("JSON object is present")
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.as_map().get(key)
    }

    pub fn contains_key(&self, key: &str) -> bool {
        self.as_map().contains_key(key)
    }

    pub(crate) fn remove(&mut self, key: &str) -> Option<JsonValue> {
        self.0
            .as_value_mut()
            .as_object_mut()
            .expect("JSON object is present")
            .remove(key)
            .map(JsonValue::from_value)
    }
}

enum Frame {
    Array(Vec<Value>),
    Object {
        values: Map<String, Value>,
        key: Option<String>,
    },
}

#[derive(Default)]
struct Frames(Vec<Frame>);

impl Drop for Frames {
    fn drop(&mut self) {
        let mut values = Vec::new();
        for frame in self.0.drain(..) {
            match frame {
                Frame::Array(children) => values.extend(children),
                Frame::Object {
                    values: children, ..
                } => values.extend(children.into_iter().map(|(_, value)| value)),
            }
        }
        drop_values(values);
    }
}

fn attach_value(frames: &mut Frames, root: &mut Option<JsonValue>, value: Value) {
    match frames.0.last_mut() {
        Some(Frame::Array(values)) => values.push(value),
        Some(Frame::Object { values, key }) => {
            let key = key.take().expect("validated JSON object has a key");
            if let Some(previous) = values.insert(key, value) {
                drop_values(vec![previous]);
            }
        }
        None => *root = Some(JsonValue::from_value(value)),
    }
}

/// Parses the complete JSON document without a nesting limit or recursive drop.
/// Non-Unicode scalar escapes are rejected by serde_json's string decoder.
pub fn parse_json(bytes: &[u8]) -> Result<JsonValue, JsonParseError> {
    let raw: &RawValue = serde_json::from_slice(bytes).map_err(|_| JsonParseError)?;
    let bytes = raw.get().as_bytes();
    let mut frames = Frames::default();
    let mut root = None;
    let mut at = 0;
    while at < bytes.len() {
        match bytes[at] {
            b' ' | b'\r' | b'\n' | b'\t' | b',' | b':' => at += 1,
            b'[' => {
                frames.0.push(Frame::Array(Vec::new()));
                at += 1;
            }
            b'{' => {
                frames.0.push(Frame::Object {
                    values: Map::new(),
                    key: None,
                });
                at += 1;
            }
            b']' => {
                let Some(Frame::Array(values)) = frames.0.pop() else {
                    return Err(JsonParseError);
                };
                attach_value(&mut frames, &mut root, Value::Array(values));
                at += 1;
            }
            b'}' => {
                let Some(Frame::Object { values, key: None }) = frames.0.pop() else {
                    return Err(JsonParseError);
                };
                attach_value(&mut frames, &mut root, Value::Object(values));
                at += 1;
            }
            b'"' => {
                let start = at;
                at += 1;
                while at < bytes.len() {
                    match bytes[at] {
                        b'\\' => at += 2,
                        b'"' => {
                            at += 1;
                            break;
                        }
                        _ => at += 1,
                    }
                }
                let text: String =
                    serde_json::from_slice(&bytes[start..at]).map_err(|_| JsonParseError)?;
                match frames.0.last_mut() {
                    Some(Frame::Object { key, .. }) if key.is_none() => *key = Some(text),
                    _ => attach_value(&mut frames, &mut root, Value::String(text)),
                }
            }
            _ => {
                let start = at;
                while at < bytes.len()
                    && !matches!(bytes[at], b' ' | b'\r' | b'\n' | b'\t' | b',' | b']' | b'}')
                {
                    at += 1;
                }
                let scalar: Value =
                    serde_json::from_slice(&bytes[start..at]).map_err(|_| JsonParseError)?;
                attach_value(&mut frames, &mut root, scalar);
            }
        }
    }
    root.ok_or(JsonParseError)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JsonParseError;

impl fmt::Display for JsonParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Invalid JSON")
    }
}

impl std::error::Error for JsonParseError {}
