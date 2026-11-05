//! Ordering resolved property values for `base_query` sorting.

use std::cmp::Ordering;

use serde_json::Value;

/// Orders two resolved property values for `base_query` sorting. Missing
/// values (`None`) sort before every present value; among present values,
/// `Null < Bool < Number < String`, then within-type ordering (`false <
/// true`; numeric; lexicographic). Arrays and objects have no defined order
/// here (arbitrary but stable, by discriminant only) since sorting on them
/// is out of this iteration's scope.
#[must_use]
pub fn compare_values(a: Option<&Value>, b: Option<&Value>) -> Ordering {
    match (a, b) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        (Some(a), Some(b)) => compare_present(a, b),
    }
}

fn type_rank(value: &Value) -> u8 {
    match value {
        Value::Null => 0,
        Value::Bool(_) => 1,
        Value::Number(_) => 2,
        Value::String(_) => 3,
        Value::Array(_) => 4,
        Value::Object(_) => 5,
    }
}

fn compare_present(a: &Value, b: &Value) -> Ordering {
    match (a, b) {
        (Value::Null, Value::Null) => Ordering::Equal,
        (Value::Bool(a), Value::Bool(b)) => a.cmp(b),
        (Value::Number(a), Value::Number(b)) => a
            .as_f64()
            .unwrap_or_default()
            .partial_cmp(&b.as_f64().unwrap_or_default())
            .unwrap_or(Ordering::Equal),
        (Value::String(a), Value::String(b)) => a.cmp(b),
        _ => type_rank(a).cmp(&type_rank(b)),
    }
}
