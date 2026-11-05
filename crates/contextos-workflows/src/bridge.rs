//! Marshals values across the one boundary a job script crosses: a Rhai
//! `call_mcp(...)` invocation's arguments going out as JSON, and the MCP
//! tool result coming back in as a Rhai value (`script.rs`).
//!
//! `rhai::Dynamic` and `serde_json::Value` are both foreign types, so a
//! direct `From`/`TryFrom` impl between them is blocked by the orphan
//! rule; these are named for the marshalling behaviour they perform
//! instead, not as a `to_*`/`from_*` pair standing in for one.

use rhai::{Dynamic, EvalAltResult};
use serde_json::{Map, Number, Value};

/// Converts a Rhai object map (a `call_mcp` argument) to JSON.
///
/// # Errors
///
/// Returns an error message when a value inside `map` has no JSON
/// representation (Rhai's own function pointers, closures, and shared
/// mutable references cannot appear in an MCP call's arguments).
pub fn arguments_to_json(map: rhai::Map) -> Result<Map<String, Value>, String> {
    map.into_iter()
        .map(|(key, value)| Ok((key.to_string(), encode_json(&value)?)))
        .collect()
}

/// Converts an MCP tool result to a Rhai value a script can inspect.
pub fn result_to_dynamic(value: Value) -> Dynamic {
    decode_json(value)
}

/// Converts any Rhai value a script hands to the host (an output) to JSON.
///
/// # Errors
///
/// Returns an error message when the value, or one inside it, has no JSON representation.
pub fn dynamic_to_json(value: &Dynamic) -> Result<Value, String> {
    encode_json(value)
}

fn encode_json(value: &Dynamic) -> Result<Value, String> {
    if value.is_unit() {
        return Ok(Value::Null);
    }
    if value.is_bool() {
        return Ok(Value::Bool(value.as_bool().unwrap_or_default()));
    }
    if value.is_int() {
        return Ok(Value::Number(Number::from(value.as_int().unwrap_or_default())));
    }
    if value.is_float() {
        let float = value.as_float().unwrap_or_default();
        return Number::from_f64(float)
            .map(Value::Number)
            .ok_or_else(|| format!("{float} has no JSON representation (NaN or infinite)"));
    }
    if value.is_string() {
        return Ok(Value::String(value.clone().into_string().unwrap_or_default()));
    }
    if value.is_array() {
        let array = value.clone().cast::<rhai::Array>();
        return array
            .iter()
            .map(encode_json)
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array);
    }
    if value.is_map() {
        let map = value.clone().cast::<rhai::Map>();
        return arguments_to_json(map).map(Value::Object);
    }
    Err(format!("{} has no JSON representation", value.type_name()))
}

fn decode_json(value: Value) -> Dynamic {
    match value {
        Value::Null => Dynamic::UNIT,
        Value::Bool(flag) => Dynamic::from(flag),
        Value::Number(number) => number
            .as_i64()
            .map(Dynamic::from)
            .or_else(|| number.as_f64().map(Dynamic::from))
            .unwrap_or(Dynamic::UNIT),
        Value::String(text) => Dynamic::from(text),
        Value::Array(items) => Dynamic::from_array(items.into_iter().map(decode_json).collect()),
        Value::Object(fields) => {
            let mut map = rhai::Map::new();
            for (key, field_value) in fields {
                map.insert(key.into(), decode_json(field_value));
            }
            Dynamic::from_map(map)
        }
    }
}

/// A `call_mcp` argument that could not be marshalled to JSON, raised as a
/// catchable Rhai runtime error.
pub fn argument_error(message: String) -> Box<EvalAltResult> {
    Box::new(EvalAltResult::ErrorRuntime(message.into(), rhai::Position::NONE))
}

#[cfg(test)]
#[path = "bridge_test.rs"]
mod tests;
