//! A dataset's columns: names and kinds, plus the key column rows are
//! sorted and queried by.

use borsh::{BorshDeserialize, BorshSerialize};
use serde_json::{json, Value};

/// Most columns a dataset may have.
pub const MAX_COLUMNS: usize = 64;
/// Longest column name (bytes).
pub const MAX_COLUMN_NAME: usize = 63;
/// Largest decimal scale (as `onyx_core::Value::Fixed`).
pub const MAX_DECIMAL_SCALE: u8 = 38;

/// What a column holds. No float kind exists (ADR-0009).
#[derive(Clone, Copy, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum ColumnKind {
    /// A whole number (`i128`).
    Int,
    /// A decimal, kept as its canonical string with exactly `scale`
    /// fraction digits (`"12.50"` for scale 2).
    Decimal {
        /// Fraction digits, 1..=[`MAX_DECIMAL_SCALE`].
        scale: u8,
    },
    /// UTF-8 text without control characters (tab and line feed allowed).
    Text,
}

/// One column.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Column {
    /// `[a-z][a-z0-9_]*`, at most [`MAX_COLUMN_NAME`] bytes, unique.
    pub name: String,
    /// Kind.
    pub kind: ColumnKind,
    /// Whether a row may leave it empty.
    pub nullable: bool,
}

/// A dataset's columns in row order and its key column.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Schema {
    /// Columns, in the order every row holds them.
    pub columns: Vec<Column>,
    /// Index of the key column: an `Int`, never null (a time or a block
    /// height). Rows are sorted by it and range queries use it.
    pub key: u16,
}

/// Malformed schema.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum SchemaError {
    /// Bad shape.
    #[error("schema: {0}")]
    Bad(String),
}

fn bad(s: impl Into<String>) -> SchemaError {
    SchemaError::Bad(s.into())
}

/// True for a column name: `[a-z][a-z0-9_]*`, 1..=63 bytes.
pub fn is_column_name(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b.len() <= MAX_COLUMN_NAME
        && b[0].is_ascii_lowercase()
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_')
}

impl Schema {
    /// Shape check: 1..=64 columns with valid unique names, valid scales,
    /// and an `Int`, non-null key column.
    pub fn validate(&self) -> Result<(), SchemaError> {
        if self.columns.is_empty() || self.columns.len() > MAX_COLUMNS {
            return Err(bad(format!("1 to {MAX_COLUMNS} columns")));
        }
        let mut names: Vec<&str> = Vec::with_capacity(self.columns.len());
        for c in &self.columns {
            if !is_column_name(&c.name) {
                return Err(bad(format!("column name {:?}", c.name)));
            }
            if names.contains(&c.name.as_str()) {
                return Err(bad(format!("column {} twice", c.name)));
            }
            names.push(&c.name);
            if let ColumnKind::Decimal { scale } = c.kind {
                if scale == 0 || scale > MAX_DECIMAL_SCALE {
                    return Err(bad(format!(
                        "column {}: decimal scale 1 to {MAX_DECIMAL_SCALE} (use int for 0)",
                        c.name
                    )));
                }
            }
        }
        let key = self
            .columns
            .get(usize::from(self.key))
            .ok_or_else(|| bad("key column out of range"))?;
        if key.kind != ColumnKind::Int || key.nullable {
            return Err(bad(format!(
                "key column {} must be int and not nullable",
                key.name
            )));
        }
        Ok(())
    }

    /// Index of the column named `name`.
    pub fn index_of(&self, name: &str) -> Option<usize> {
        self.columns.iter().position(|c| c.name == name)
    }

    /// The key column.
    pub fn key_column(&self) -> Option<&Column> {
        self.columns.get(usize::from(self.key))
    }

    /// Parse the publisher's schema file:
    /// `{"columns":[{"name":"t","kind":"int"},{"name":"v","kind":"decimal","scale":8,"nullable":true},{"name":"s","kind":"text"}],"key":"t"}`.
    pub fn from_json(v: &Value) -> Result<Schema, SchemaError> {
        let obj = v.as_object().ok_or_else(|| bad("not an object"))?;
        if let Some(k) = obj.keys().find(|k| *k != "columns" && *k != "key") {
            return Err(bad(format!("unknown field {k}")));
        }
        let cols = obj
            .get("columns")
            .and_then(Value::as_array)
            .ok_or_else(|| bad("columns: a list"))?;
        let mut columns = Vec::with_capacity(cols.len());
        for c in cols {
            let o = c.as_object().ok_or_else(|| bad("column: an object"))?;
            if let Some(k) = o
                .keys()
                .find(|k| !matches!(k.as_str(), "name" | "kind" | "scale" | "nullable"))
            {
                return Err(bad(format!("column: unknown field {k}")));
            }
            let name = o
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| bad("column: name"))?
                .to_string();
            let scale = o.get("scale").map(|s| {
                s.as_u64()
                    .and_then(|n| u8::try_from(n).ok())
                    .ok_or_else(|| bad(format!("column {name}: scale")))
            });
            let kind = match o.get("kind").and_then(Value::as_str) {
                Some("int") if scale.is_none() => ColumnKind::Int,
                Some("text") if scale.is_none() => ColumnKind::Text,
                Some("decimal") => ColumnKind::Decimal {
                    scale: scale
                        .ok_or_else(|| bad(format!("column {name}: decimal needs scale")))??,
                },
                _ => {
                    return Err(bad(format!(
                        "column {name}: kind is int, decimal (with scale) or text"
                    )))
                }
            };
            let nullable = match o.get("nullable") {
                None => false,
                Some(b) => b
                    .as_bool()
                    .ok_or_else(|| bad(format!("column {name}: nullable")))?,
            };
            columns.push(Column {
                name,
                kind,
                nullable,
            });
        }
        let key_name = obj
            .get("key")
            .and_then(Value::as_str)
            .ok_or_else(|| bad("key: a column name"))?;
        let key = columns
            .iter()
            .position(|c| c.name == key_name)
            .and_then(|i| u16::try_from(i).ok())
            .ok_or_else(|| bad(format!("key {key_name} is not a column")))?;
        let s = Schema { columns, key };
        s.validate()?;
        Ok(s)
    }

    /// The JSON form [`Schema::from_json`] reads.
    pub fn to_json(&self) -> Value {
        let cols: Vec<Value> = self
            .columns
            .iter()
            .map(|c| {
                let mut o = match c.kind {
                    ColumnKind::Int => json!({"name": c.name, "kind": "int"}),
                    ColumnKind::Text => json!({"name": c.name, "kind": "text"}),
                    ColumnKind::Decimal { scale } => {
                        json!({"name": c.name, "kind": "decimal", "scale": scale})
                    }
                };
                if c.nullable {
                    o["nullable"] = json!(true);
                }
                o
            })
            .collect();
        let key = self
            .key_column()
            .map(|c| c.name.clone())
            .unwrap_or_default();
        json!({"columns": cols, "key": key})
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn sample() -> Schema {
        Schema::from_json(&json!({"columns":[
            {"name":"t","kind":"int"},
            {"name":"series","kind":"text"},
            {"name":"value","kind":"decimal","scale":8,"nullable":true},
            {"name":"count","kind":"int"}
        ],"key":"t"}))
        .expect("ok")
    }

    #[test]
    fn json_round_trip() {
        let s = sample();
        assert_eq!(s.columns.len(), 4);
        assert_eq!(s.key, 0);
        assert_eq!(Schema::from_json(&s.to_json()).expect("ok"), s);
    }

    #[test]
    fn refuses_bad_schemas() {
        for bad in [
            json!({"columns":[],"key":"t"}),
            json!({"columns":[{"name":"t","kind":"float"}],"key":"t"}),
            json!({"columns":[{"name":"t","kind":"int"},{"name":"t","kind":"int"}],"key":"t"}),
            json!({"columns":[{"name":"T","kind":"int"}],"key":"T"}),
            json!({"columns":[{"name":"t","kind":"text"}],"key":"t"}),
            json!({"columns":[{"name":"t","kind":"int","nullable":true}],"key":"t"}),
            json!({"columns":[{"name":"t","kind":"int"},{"name":"v","kind":"decimal","scale":0}],"key":"t"}),
            json!({"columns":[{"name":"t","kind":"int"},{"name":"v","kind":"decimal","scale":39}],"key":"t"}),
            json!({"columns":[{"name":"t","kind":"int"},{"name":"v","kind":"decimal"}],"key":"t"}),
            json!({"columns":[{"name":"t","kind":"int","scale":2}],"key":"t"}),
            json!({"columns":[{"name":"t","kind":"int"}],"key":"x"}),
            json!({"columns":[{"name":"t","kind":"int"}],"key":"t","extra":1}),
        ] {
            assert!(Schema::from_json(&bad).is_err(), "{bad}");
        }
        let many: Vec<Value> = (0..=MAX_COLUMNS)
            .map(|i| json!({"name": format!("c{i}"), "kind": "int"}))
            .collect();
        assert!(Schema::from_json(&json!({"columns": many, "key": "c0"})).is_err());
        let ok: Vec<Value> = (0..MAX_COLUMNS)
            .map(|i| json!({"name": format!("c{i}"), "kind": "int"}))
            .collect();
        assert!(Schema::from_json(&json!({"columns": ok, "key": "c0"})).is_ok());
    }

    #[test]
    fn column_names() {
        assert!(is_column_name("block_height"));
        assert!(is_column_name(&"a".repeat(MAX_COLUMN_NAME)));
        assert!(!is_column_name(&"a".repeat(MAX_COLUMN_NAME + 1)));
        for bad in ["", "1a", "_a", "a-b", "a b", "Ab"] {
            assert!(!is_column_name(bad), "{bad}");
        }
    }
}
