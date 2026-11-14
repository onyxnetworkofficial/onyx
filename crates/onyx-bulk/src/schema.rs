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

/// A kind as the schema file writes it: `int`, `decimal(8)`, `text`.
pub fn kind_text(k: ColumnKind) -> String {
    match k {
        ColumnKind::Int => "int".into(),
        ColumnKind::Decimal { scale } => format!("decimal({scale})"),
        ColumnKind::Text => "text".into(),
    }
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

    /// Whether `new` widens `old` (ADR-0059 amendment 5 item 2), so a later
    /// version may build its new shards under `new` while every earlier
    /// shard keeps `old`. Allowed at an old column's position: a decimal's
    /// scale grows, or a non-key `int` becomes `decimal`; after the old
    /// columns, new nullable ones. Names, order, nullability, text columns
    /// and the key never change, and at least one change is required. Both
    /// schemas must be valid.
    pub fn widens(old: &Schema, new: &Schema) -> Result<(), SchemaError> {
        old.validate()?;
        new.validate()?;
        if new.key != old.key {
            return Err(bad("widening: the key column stays the same"));
        }
        if new.columns.len() < old.columns.len() {
            return Err(bad(
                "widening: no column is dropped (new columns go after the old ones)",
            ));
        }
        for (i, (o, n)) in old.columns.iter().zip(&new.columns).enumerate() {
            if o.name != n.name {
                return Err(bad(format!(
                    "widening: column {i} is {} and stays {} (no renaming or reordering)",
                    n.name, o.name
                )));
            }
            if o.nullable != n.nullable {
                return Err(bad(format!(
                    "widening: column {}: nullability never changes",
                    o.name
                )));
            }
            match (o.kind, n.kind) {
                (a, b) if a == b => {}
                (ColumnKind::Decimal { scale: a }, ColumnKind::Decimal { scale: b }) if b > a => {}
                (ColumnKind::Int, ColumnKind::Decimal { .. }) if i != usize::from(old.key) => {}
                (a, b) => {
                    return Err(bad(format!(
                        "widening: column {}: {} cannot become {} (only a decimal's scale grows, or a non-key int becomes decimal)",
                        o.name,
                        kind_text(a),
                        kind_text(b)
                    )))
                }
            }
        }
        if let Some(c) = new.columns[old.columns.len()..]
            .iter()
            .find(|c| !c.nullable)
        {
            return Err(bad(format!(
                "widening: the added column {} must be nullable",
                c.name
            )));
        }
        if old == new {
            return Err(bad("widening: nothing changes"));
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

    fn widened(f: impl FnOnce(&mut Schema)) -> Schema {
        let mut s = sample();
        f(&mut s);
        s
    }

    fn col(name: &str, kind: ColumnKind, nullable: bool) -> Column {
        Column {
            name: name.into(),
            kind,
            nullable,
        }
    }

    /// ADR-0059 amendment 5 item 2: a decimal's scale grows (up to 38), a
    /// non-key int becomes decimal, and nullable columns are added after the
    /// old ones, alone or together.
    #[test]
    fn widen_2_scale_int_to_decimal_nullable_and_appended_columns_widen() {
        let old = sample();
        let scale = widened(|s| s.columns[2].kind = ColumnKind::Decimal { scale: 16 });
        let max = widened(|s| {
            s.columns[2].kind = ColumnKind::Decimal {
                scale: MAX_DECIMAL_SCALE,
            }
        });
        let int = widened(|s| s.columns[3].kind = ColumnKind::Decimal { scale: 1 });
        let added = widened(|s| {
            s.columns.push(col("extra", ColumnKind::Text, true));
            s.columns
                .push(col("more", ColumnKind::Decimal { scale: 4 }, true));
        });
        let all = widened(|s| {
            s.columns[2].kind = ColumnKind::Decimal { scale: 9 };
            s.columns[3].kind = ColumnKind::Decimal { scale: 38 };
            s.columns.push(col("extra", ColumnKind::Int, true));
        });
        for (name, new) in [
            ("scale", &scale),
            ("scale 38", &max),
            ("int to decimal", &int),
            ("added", &added),
            ("all", &all),
        ] {
            assert_eq!(Schema::widens(&old, new), Ok(()), "{name}");
        }
        // Widening twice: each step on its own.
        assert_eq!(Schema::widens(&scale, &max), Ok(()));
        // Up to 64 columns.
        let full = widened(|s| {
            for i in s.columns.len()..MAX_COLUMNS {
                s.columns.push(col(&format!("c{i}"), ColumnKind::Int, true));
            }
        });
        assert_eq!(Schema::widens(&old, &full), Ok(()));
    }

    /// Everything else is refused: renaming, reordering or dropping a column,
    /// a smaller or equal scale, decimal to int, any change to or from text,
    /// any change of nullability (review BULK-WIDEN-R1 finding 3), a
    /// required added column, a change to the key, no change at all, more
    /// than 64 columns.
    #[test]
    fn widen_3_every_other_change_is_refused() {
        let old = sample();
        let mut cases: Vec<(&str, Schema)> = vec![
            ("rename", widened(|s| s.columns[1].name = "label".into())),
            ("reorder", widened(|s| s.columns.swap(1, 3))),
            (
                "drop",
                widened(|s| {
                    s.columns.pop();
                }),
            ),
            (
                "smaller scale",
                widened(|s| s.columns[2].kind = ColumnKind::Decimal { scale: 7 }),
            ),
            (
                "decimal to int",
                widened(|s| s.columns[2].kind = ColumnKind::Int),
            ),
            (
                "decimal to text",
                widened(|s| s.columns[2].kind = ColumnKind::Text),
            ),
            (
                "int to text",
                widened(|s| s.columns[3].kind = ColumnKind::Text),
            ),
            (
                "text to decimal",
                widened(|s| s.columns[1].kind = ColumnKind::Decimal { scale: 2 }),
            ),
            (
                "text to int",
                widened(|s| s.columns[1].kind = ColumnKind::Int),
            ),
            ("to nullable", widened(|s| s.columns[1].nullable = true)),
            ("to not null", widened(|s| s.columns[2].nullable = false)),
            (
                "required added column",
                widened(|s| s.columns.push(col("extra", ColumnKind::Int, false))),
            ),
            (
                "key to decimal",
                widened(|s| s.columns[0].kind = ColumnKind::Decimal { scale: 2 }),
            ),
            ("no change", sample()),
        ];
        // Another key column (the key moves to `count`, an int, not null).
        cases.push(("other key", widened(|s| s.key = 3)));
        // An added column first, the old ones after it.
        cases.push((
            "added first",
            widened(|s| s.columns.insert(0, col("extra", ColumnKind::Int, true))),
        ));
        for (name, new) in &cases {
            assert!(Schema::widens(&old, new).is_err(), "{name}");
        }
        // An invalid schema never widens anything (65 columns, scale 39).
        let over = widened(|s| {
            for i in s.columns.len()..=MAX_COLUMNS {
                s.columns.push(col(&format!("c{i}"), ColumnKind::Int, true));
            }
        });
        assert!(Schema::widens(&old, &over).is_err());
        let scale_39 = widened(|s| s.columns[2].kind = ColumnKind::Decimal { scale: 39 });
        assert!(Schema::widens(&old, &scale_39).is_err());
        // The reverse of every widening is refused too.
        let wide = widened(|s| s.columns[2].kind = ColumnKind::Decimal { scale: 9 });
        assert!(Schema::widens(&wide, &old).is_err());
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
