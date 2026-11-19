//! Reading and editing one client's config text. Pure text in, text out.

use serde_json::{Map, Value};

use crate::clients::{Client, Format, SERVER_NAME};
use crate::jsonc;

/// The whole config as JSON-like data. An empty file is `{}`.
pub fn parse(client: &Client, text: &str) -> Result<Value, String> {
    match client.format {
        Format::Jsonc => {
            if text.trim().is_empty() {
                return Ok(Value::Object(Map::new()));
            }
            jsonc::parse(text, false).map(|d| d.to_value())
        }
        Format::Toml => {
            let doc: toml_edit::DocumentMut = text.parse().map_err(|e: toml_edit::TomlError| e.to_string())?;
            Ok(table_to_json(doc.as_table()))
        }
    }
}

fn table_to_json(t: &toml_edit::Table) -> Value {
    Value::Object(t.iter().map(|(k, v)| (k.to_owned(), item_to_json(v))).collect())
}

fn item_to_json(item: &toml_edit::Item) -> Value {
    match item {
        toml_edit::Item::Value(v) => value_to_json(v),
        toml_edit::Item::Table(t) => table_to_json(t),
        toml_edit::Item::ArrayOfTables(a) => Value::Array(a.iter().map(table_to_json).collect()),
        toml_edit::Item::None => Value::Null,
    }
}

fn value_to_json(v: &toml_edit::Value) -> Value {
    match v {
        toml_edit::Value::String(s) => Value::String(s.value().clone()),
        toml_edit::Value::Integer(i) => Value::from(*i.value()),
        toml_edit::Value::Float(f) => serde_json::Number::from_f64(*f.value()).map_or(Value::Null, Value::Number),
        toml_edit::Value::Boolean(b) => Value::Bool(*b.value()),
        toml_edit::Value::Datetime(d) => Value::String(d.value().to_string()),
        toml_edit::Value::Array(a) => Value::Array(a.iter().map(value_to_json).collect()),
        toml_edit::Value::InlineTable(t) => Value::Object(t.iter().map(|(k, v)| (k.to_owned(), value_to_json(v))).collect()),
    }
}

fn json_to_toml(v: &Value) -> Option<toml_edit::Value> {
    Some(match v {
        Value::String(s) => s.as_str().into(),
        Value::Bool(b) => (*b).into(),
        Value::Number(n) => match n.as_i64() {
            Some(i) => i.into(),
            None => n.as_f64()?.into(),
        },
        Value::Array(items) => {
            let mut a = toml_edit::Array::new();
            for i in items {
                a.push(json_to_toml(i)?);
            }
            a.into()
        }
        Value::Object(map) => {
            let mut t = toml_edit::InlineTable::new();
            for (k, v) in map {
                t.insert(k, json_to_toml(v)?);
            }
            t.into()
        }
        Value::Null => return None,
    })
}

/// Our entry, if present.
pub fn read_entry(client: &Client, text: &str) -> Result<Option<Value>, String> {
    let v = parse(client, text)?;
    let mut cur = &v;
    for k in client.entry_path() {
        match cur.get(k) {
            Some(n) => cur = n,
            None => return Ok(None),
        }
    }
    Ok(Some(cur.clone()))
}

/// Text with our entry set to `entry`; everything else, comments included, is untouched.
pub fn set_entry(client: &Client, text: &str, entry: &Value) -> Result<String, String> {
    parse(client, text)?;
    match client.format {
        Format::Jsonc => {
            let base = if text.trim().is_empty() { "{}" } else { text };
            jsonc::parse(base, false)?.set(&client.entry_path(), entry)
        }
        Format::Toml => {
            let mut doc: toml_edit::DocumentMut = text.parse().map_err(|e: toml_edit::TomlError| e.to_string())?;
            let table_key = client.table.first().copied().unwrap_or("mcp_servers");
            let servers = doc.entry(table_key).or_insert_with(|| {
                let mut t = toml_edit::Table::new();
                t.set_implicit(true);
                toml_edit::Item::Table(t)
            });
            let servers = servers.as_table_like_mut().ok_or_else(|| format!("`{table_key}` in this file is not a table"))?;
            let mut t = toml_edit::Table::new();
            for (k, v) in entry.as_object().into_iter().flatten() {
                t.insert(k, toml_edit::Item::Value(json_to_toml(v).ok_or("an entry value TOML cannot hold")?));
            }
            servers.insert(SERVER_NAME, toml_edit::Item::Table(t));
            Ok(with_line_endings_of(text, doc.to_string()))
        }
    }
}

/// Text with our entry removed, or `None` when it was not there.
pub fn remove_entry(client: &Client, text: &str) -> Result<Option<String>, String> {
    match client.format {
        Format::Jsonc => Ok(jsonc::parse(text, false)?.remove(&client.entry_path())),
        Format::Toml => {
            let mut doc: toml_edit::DocumentMut = text.parse().map_err(|e: toml_edit::TomlError| e.to_string())?;
            let table_key = client.table.first().copied().unwrap_or("mcp_servers");
            let removed = doc.get_mut(table_key).and_then(|t| t.as_table_like_mut()).and_then(|t| t.remove(SERVER_NAME)).is_some();
            Ok(removed.then(|| with_line_endings_of(text, doc.to_string())))
        }
    }
}

/// `edited` with CRLF line endings when every line of `original` had them (toml_edit writes `\n`).
fn with_line_endings_of(original: &str, edited: String) -> String {
    let crlf = original.matches("\r\n").count();
    if crlf == 0 || crlf != original.matches('\n').count() {
        return edited;
    }
    edited.replace("\r\n", "\n").replace('\n', "\r\n")
}
