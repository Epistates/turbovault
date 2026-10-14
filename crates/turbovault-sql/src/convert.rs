//! Conversion between GlueSQL values and serde_json values

use gluesql::prelude::{Payload, Value as GlueValue};
use serde_json::{Value, json};

/// Convert a GlueSQL `Payload` to a JSON value for MCP responses
pub(crate) fn payload_to_json(payload: Payload) -> Value {
    match payload {
        Payload::Select { labels, rows } => {
            let results: Vec<Value> = rows
                .into_iter()
                .map(|row| {
                    let obj: serde_json::Map<String, Value> = labels
                        .iter()
                        .zip(row)
                        .map(|(label, val)| (label.clone(), glue_to_json(val)))
                        .collect();
                    Value::Object(obj)
                })
                .collect();
            json!({
                "rows": results,
                "count": results.len()
            })
        }
        // A schemaless table, `files`, answers `SELECT *` with one map per
        // row rather than labels and values.
        Payload::SelectMap(rows) => {
            let results: Vec<Value> = rows
                .into_iter()
                .map(|row| {
                    Value::Object(
                        row.into_iter()
                            .map(|(label, val)| (label, glue_to_json(val)))
                            .collect(),
                    )
                })
                .collect();
            json!({
                "rows": results,
                "count": results.len()
            })
        }
        Payload::Insert(n) => json!({"type": "insert", "affected": n}),
        Payload::Update(n) => json!({"type": "update", "affected": n}),
        Payload::Delete(n) => json!({"type": "delete", "affected": n}),
        Payload::Create => json!({"type": "create", "status": "ok"}),
        _ => json!({"type": "other", "status": "ok"}),
    }
}

/// Convert a GlueSQL `Value` to a serde_json `Value`
pub(crate) fn glue_to_json(val: GlueValue) -> Value {
    match val {
        GlueValue::Bool(b) => json!(b),
        GlueValue::I8(n) => json!(n),
        GlueValue::I16(n) => json!(n),
        GlueValue::I32(n) => json!(n),
        GlueValue::I64(n) => json!(n),
        GlueValue::I128(n) => json!(n),
        GlueValue::U8(n) => json!(n),
        GlueValue::U16(n) => json!(n),
        GlueValue::U32(n) => json!(n),
        GlueValue::U64(n) => json!(n),
        GlueValue::U128(n) => json!(n),
        GlueValue::F32(n) => json!(n),
        GlueValue::F64(n) => json!(n),
        GlueValue::Str(s) => Value::String(s),
        GlueValue::Null => Value::Null,
        GlueValue::List(items) => Value::Array(items.into_iter().map(glue_to_json).collect()),
        GlueValue::Map(map) => {
            let obj: serde_json::Map<String, Value> = map
                .into_iter()
                .map(|(k, v)| (format!("{k:?}"), glue_to_json(v)))
                .collect();
            Value::Object(obj)
        }
        other => Value::String(format!("{other:?}")),
    }
}

/// Determine the JSON type name for schema inspection
pub(crate) fn json_type_name(value: &Value) -> &'static str {
    match value {
        Value::String(_) => "string",
        Value::Number(_) => "number",
        Value::Bool(_) => "boolean",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
        Value::Null => "null",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_json_type_name() {
        assert_eq!(json_type_name(&json!("hello")), "string");
        assert_eq!(json_type_name(&json!(42)), "number");
        assert_eq!(json_type_name(&json!(true)), "boolean");
        assert_eq!(json_type_name(&json!([1, 2])), "array");
        assert_eq!(json_type_name(&json!({"a": 1})), "object");
        assert_eq!(json_type_name(&json!(null)), "null");
    }

    #[test]
    fn test_glue_to_json_primitives() {
        assert_eq!(glue_to_json(GlueValue::Bool(true)), json!(true));
        assert_eq!(glue_to_json(GlueValue::I64(42)), json!(42));
        assert_eq!(glue_to_json(GlueValue::F64(2.72)), json!(2.72));
        assert_eq!(glue_to_json(GlueValue::Str("hi".into())), json!("hi"));
        assert_eq!(glue_to_json(GlueValue::Null), Value::Null);
    }

    #[test]
    fn test_glue_to_json_list() {
        let list = GlueValue::List(vec![GlueValue::Str("a".into()), GlueValue::Str("b".into())]);
        assert_eq!(glue_to_json(list), json!(["a", "b"]));
    }
}
