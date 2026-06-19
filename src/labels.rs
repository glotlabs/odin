pub fn json_display(value: impl serde::Serialize) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(label)) => label,
        Ok(value) => value.to_string(),
        Err(_) => "-".to_string(),
    }
}
