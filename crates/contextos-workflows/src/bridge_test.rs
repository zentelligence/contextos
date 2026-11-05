use rhai::Dynamic;
use serde_json::json;

use super::*;

#[test]
fn arguments_to_json_converts_every_scalar_and_nested_shape() -> Result<(), String> {
    let mut map = rhai::Map::new();
    map.insert("name".into(), Dynamic::from("sync-crm-contacts".to_owned()));
    map.insert("count".into(), Dynamic::from(3_i64));
    map.insert("dry_run".into(), Dynamic::from(true));
    let mut nested = rhai::Map::new();
    nested.insert(
        "tags".into(),
        Dynamic::from_array(vec![Dynamic::from("a".to_owned()), Dynamic::from("b".to_owned())]),
    );
    map.insert("options".into(), Dynamic::from_map(nested));

    let json = arguments_to_json(map)?;

    assert_eq!(json["name"], Value::String("sync-crm-contacts".to_owned()));
    assert_eq!(json["count"], json!(3));
    assert_eq!(json["dry_run"], Value::Bool(true));
    assert_eq!(json["options"]["tags"], json!(["a", "b"]));
    Ok(())
}

#[test]
fn arguments_to_json_rejects_a_value_with_no_json_representation() -> Result<(), String> {
    let mut map = rhai::Map::new();
    let function_pointer = rhai::FnPtr::new("noop").map_err(|error| error.to_string())?;
    map.insert("callback".into(), Dynamic::from(function_pointer));

    let Err(error) = arguments_to_json(map) else {
        return Err("a function pointer has no JSON representation".to_owned());
    };
    assert!(error.contains("no JSON representation"), "message was {error:?}");
    Ok(())
}

#[test]
fn result_to_dynamic_round_trips_a_json_object_into_a_rhai_map() {
    let value = json!({"status": "ok", "attempts": 2, "warnings": []});

    let dynamic = result_to_dynamic(value);

    let map = dynamic.cast::<rhai::Map>();
    assert_eq!(map["status"].clone().cast::<String>(), "ok");
    assert_eq!(map["attempts"].clone().cast::<rhai::INT>(), 2);
    assert!(map["warnings"].clone().cast::<rhai::Array>().is_empty());
}

#[test]
fn result_to_dynamic_converts_null_to_unit() {
    let dynamic = result_to_dynamic(Value::Null);
    assert!(dynamic.is_unit());
}
