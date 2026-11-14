//! Rows: one [`Cell`] per column, Borsh encoded (ADR-0002). Every cell has
//! exactly one accepted encoding, so a row's bytes (and its Merkle leaf) are
//! fixed by its values.

use crate::schema::{ColumnKind, Schema};
use borsh::{BorshDeserialize, BorshSerialize};
use serde_json::{Map, Value};

/// Largest encoded row (bytes).
pub const MAX_ROW_BYTES: usize = 64 << 10;
/// Largest text cell (UTF-8 bytes).
pub const MAX_TEXT_BYTES: usize = 16 << 10;
/// Longest decimal string.
pub const MAX_DECIMAL_CHARS: usize = 80;

/// One value of a row.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum Cell {
    /// Empty (only in a nullable column).
    Null,
    /// A whole number.
    Int(i128),
    /// A canonical decimal string (see [`is_canonical_decimal`]).
    Decimal(String),
    /// Text.
    Text(String),
}

/// A row that does not fit its schema.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum RowError {
    /// Wrong number of cells.
    #[error("row has {got} cells, the schema {want}")]
    Width {
        /// Cells in the row.
        got: usize,
        /// Columns in the schema.
        want: usize,
    },
    /// A cell of the wrong kind, or a bad value.
    #[error("column {column}: {why}")]
    Cell {
        /// Column name.
        column: String,
        /// What is wrong.
        why: String,
    },
    /// The bytes are not one Borsh row.
    #[error("row bytes do not decode")]
    Encoding,
    /// Over [`MAX_ROW_BYTES`].
    #[error("row of {0} bytes is over {MAX_ROW_BYTES}")]
    TooLarge(usize),
}

/// Parse a whole number written in decimal (`-?[0-9]+`, no `+`, no spaces).
pub fn parse_int(s: &str) -> Result<i128, String> {
    let digits = s.strip_prefix('-').unwrap_or(s);
    if digits.is_empty() || !digits.bytes().all(|c| c.is_ascii_digit()) {
        return Err(format!("{s:?} is not a whole number"));
    }
    s.parse::<i128>()
        .map_err(|_| format!("{s:?} is out of range"))
}

/// The canonical form of decimal `s` at `scale`: `-?(0|[1-9][0-9]*)\.[0-9]{scale}`,
/// no negative zero. Leading zeros are dropped and the fraction padded with
/// zeros; extra fraction digits are accepted only if they are zeros (never
/// rounded). No exponent, no `+`, no spaces.
pub fn canonical_decimal(s: &str, scale: u8) -> Result<String, String> {
    let scale = usize::from(scale);
    if s.len() > MAX_DECIMAL_CHARS {
        return Err(format!(
            "decimal longer than {MAX_DECIMAL_CHARS} characters"
        ));
    }
    let (neg, body) = match s.strip_prefix('-') {
        Some(b) => (true, b),
        None => (false, s),
    };
    let (int, frac) = match body.split_once('.') {
        Some((i, f)) => (i, f),
        None => (body, ""),
    };
    let digits = |x: &str| x.bytes().all(|c| c.is_ascii_digit());
    if int.is_empty() || !digits(int) || !digits(frac) || (body.contains('.') && frac.is_empty()) {
        return Err(format!(
            "{s:?} is not a decimal (digits, one optional point)"
        ));
    }
    if frac.len() > scale && frac[scale..].bytes().any(|c| c != b'0') {
        let needs = frac.trim_end_matches('0').len();
        return Err(format!(
            "{s:?} needs {needs} decimal places, more than {scale} (values are never rounded)"
        ));
    }
    let int = int.trim_start_matches('0');
    let int = if int.is_empty() { "0" } else { int };
    let mut frac = frac[..frac.len().min(scale)].to_string();
    while frac.len() < scale {
        frac.push('0');
    }
    let zero = int == "0" && frac.bytes().all(|c| c == b'0');
    let out = format!("{}{int}.{frac}", if neg && !zero { "-" } else { "" });
    if out.len() > MAX_DECIMAL_CHARS {
        return Err(format!(
            "decimal longer than {MAX_DECIMAL_CHARS} characters"
        ));
    }
    Ok(out)
}

/// True if `s` is already in the one form [`canonical_decimal`] gives.
pub fn is_canonical_decimal(s: &str, scale: u8) -> bool {
    canonical_decimal(s, scale).is_ok_and(|c| c == s)
}

/// Text rule: at most [`MAX_TEXT_BYTES`], no control characters (C0, DEL,
/// C1) other than tab and line feed.
pub fn check_text(s: &str) -> Result<(), String> {
    if s.len() > MAX_TEXT_BYTES {
        return Err(format!("text over {MAX_TEXT_BYTES} bytes"));
    }
    if s.chars().any(|c| c.is_control() && c != '\n' && c != '\t') {
        return Err("text holds a control character".into());
    }
    Ok(())
}

fn cell_err(schema: &Schema, i: usize, why: impl Into<String>) -> RowError {
    RowError::Cell {
        column: schema.columns[i].name.clone(),
        why: why.into(),
    }
}

/// Check `cells` against `schema`: width, kinds, nulls, canonical decimals, text.
pub fn check_cells(schema: &Schema, cells: &[Cell]) -> Result<(), RowError> {
    if cells.len() != schema.columns.len() {
        return Err(RowError::Width {
            got: cells.len(),
            want: schema.columns.len(),
        });
    }
    for (i, (c, col)) in cells.iter().zip(&schema.columns).enumerate() {
        match (c, col.kind) {
            (Cell::Null, _) if col.nullable => {}
            (Cell::Null, _) => {
                return Err(cell_err(
                    schema,
                    i,
                    "empty in a column that is not nullable",
                ))
            }
            (Cell::Int(_), ColumnKind::Int) => {}
            (Cell::Decimal(d), ColumnKind::Decimal { scale }) => {
                if !is_canonical_decimal(d, scale) {
                    return Err(cell_err(
                        schema,
                        i,
                        format!("{d:?} is not canonical at scale {scale}"),
                    ));
                }
            }
            (Cell::Text(t), ColumnKind::Text) => {
                check_text(t).map_err(|e| cell_err(schema, i, e))?
            }
            _ => return Err(cell_err(schema, i, "wrong kind")),
        }
    }
    Ok(())
}

/// Encode a checked row.
pub fn encode_row(schema: &Schema, cells: &[Cell]) -> Result<Vec<u8>, RowError> {
    check_cells(schema, cells)?;
    let b = borsh::to_vec(cells).map_err(|_| RowError::Encoding)?;
    if b.len() > MAX_ROW_BYTES {
        return Err(RowError::TooLarge(b.len()));
    }
    Ok(b)
}

/// Decode and check row bytes (exactly one row, nothing after it).
pub fn decode_row(schema: &Schema, bytes: &[u8]) -> Result<Vec<Cell>, RowError> {
    if bytes.len() > MAX_ROW_BYTES {
        return Err(RowError::TooLarge(bytes.len()));
    }
    let cells = Vec::<Cell>::try_from_slice(bytes).map_err(|_| RowError::Encoding)?;
    check_cells(schema, &cells)?;
    Ok(cells)
}

/// The key of a checked row.
pub fn key_of(schema: &Schema, cells: &[Cell]) -> Option<i128> {
    match cells.get(usize::from(schema.key)) {
        Some(Cell::Int(k)) => Some(*k),
        _ => None,
    }
}

/// The row as JSON: `{column: value}`, integers as strings (they may exceed
/// 2^53), decimals as their canonical string, null as null.
pub fn cells_json(schema: &Schema, cells: &[Cell]) -> Value {
    let mut m = Map::new();
    for (c, col) in cells.iter().zip(&schema.columns) {
        let v = match c {
            Cell::Null => Value::Null,
            Cell::Int(i) => Value::String(i.to_string()),
            Cell::Decimal(d) | Cell::Text(d) => Value::String(d.clone()),
        };
        m.insert(col.name.clone(), v);
    }
    Value::Object(m)
}

/// Show the cells of a row stored under `from` as a row of `to`, a schema
/// `from` widens to, directly or through others (ADR-0059 amendment 5 item
/// 4): a decimal gets fraction zeros up to `to`'s scale, an int in a column
/// that became decimal is written with zero fraction digits (`7` as
/// `7.000`), and every column `from` lacks is null. Nothing is rounded and
/// nothing fails; a cell `to` would not widen is left as it is. The shown
/// cells are for display and checks only, never input: a padded decimal may
/// pass [`MAX_DECIMAL_CHARS`], which bounds stored cells.
pub fn present(from: &Schema, to: &Schema, cells: &[Cell]) -> Vec<Cell> {
    to.columns
        .iter()
        .enumerate()
        .map(|(i, col)| {
            let cell = match (i < from.columns.len()).then(|| cells.get(i)).flatten() {
                Some(c) => c,
                None => return Cell::Null,
            };
            match (cell, col.kind) {
                (Cell::Int(v), ColumnKind::Decimal { scale }) => {
                    Cell::Decimal(pad_fraction(&format!("{v}."), scale))
                }
                (Cell::Decimal(d), ColumnKind::Decimal { scale }) => {
                    Cell::Decimal(pad_fraction(d, scale))
                }
                (c, _) => c.clone(),
            }
        })
        .collect()
}

/// `d` (digits and one point) with zeros after its fraction up to `scale`
/// places; one with as many or more places is returned unchanged.
fn pad_fraction(d: &str, scale: u8) -> String {
    let have = d.split_once('.').map_or(0, |(_, f)| f.len());
    let mut out = d.to_string();
    if !d.contains('.') {
        out.push('.');
    }
    out.extend(std::iter::repeat_n(
        '0',
        usize::from(scale).saturating_sub(have),
    ));
    out
}

/// Parse one input field of a column from text (CSV, or a JSON string).
/// `None` is an absent or empty field.
pub fn parse_field(kind: ColumnKind, nullable: bool, field: Option<&str>) -> Result<Cell, String> {
    match (field, kind) {
        (None, _) if nullable => Ok(Cell::Null),
        (None, ColumnKind::Text) => Ok(Cell::Text(String::new())),
        (None, _) => Err("empty, and the column is not nullable".into()),
        (Some(s), ColumnKind::Int) => parse_int(s).map(Cell::Int),
        (Some(s), ColumnKind::Decimal { scale }) => canonical_decimal(s, scale).map(Cell::Decimal),
        (Some(s), ColumnKind::Text) => check_text(s).map(|()| Cell::Text(s.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn schema() -> Schema {
        Schema::from_json(&json!({"columns":[
            {"name":"t","kind":"int"},
            {"name":"series","kind":"text"},
            {"name":"value","kind":"decimal","scale":2,"nullable":true}
        ],"key":"t"}))
        .expect("ok")
    }

    #[test]
    fn decimals_have_one_form() {
        for (inp, out) in [
            ("1", "1.00"),
            ("1.5", "1.50"),
            ("001.50", "1.50"),
            ("-0.00", "0.00"),
            ("-0", "0.00"),
            ("-12.34", "-12.34"),
            ("3.1400", "3.14"),
        ] {
            assert_eq!(canonical_decimal(inp, 2).as_deref(), Ok(out), "{inp}");
            assert!(is_canonical_decimal(out, 2));
        }
        for bad in [
            "", "-", ".5", "1.", "1.234", "1e3", "+1", " 1", "1,5", "0x10", "NaN", "inf",
        ] {
            assert!(canonical_decimal(bad, 2).is_err(), "{bad}");
        }
        for not_canon in ["1.5", "01.50", "-0.00", "1.500"] {
            assert!(!is_canonical_decimal(not_canon, 2), "{not_canon}");
        }
        assert!(canonical_decimal(&"9".repeat(MAX_DECIMAL_CHARS + 1), 2).is_err());
    }

    #[test]
    fn rows_round_trip_and_refuse_misfits() {
        let s = schema();
        let cells = vec![Cell::Int(7), Cell::Text("a".into()), Cell::Null];
        let b = encode_row(&s, &cells).expect("ok");
        assert_eq!(decode_row(&s, &b).expect("ok"), cells);
        assert_eq!(key_of(&s, &cells), Some(7));
        assert_eq!(
            cells_json(&s, &cells),
            json!({"t":"7","series":"a","value":null})
        );
        // Trailing bytes, wrong width, wrong kind, non-canonical decimal, null key.
        let mut extra = b.clone();
        extra.push(0);
        assert_eq!(decode_row(&s, &extra), Err(RowError::Encoding));
        assert!(encode_row(&s, &cells[..2]).is_err());
        assert!(encode_row(
            &s,
            &[Cell::Text("x".into()), Cell::Text("a".into()), Cell::Null]
        )
        .is_err());
        assert!(encode_row(
            &s,
            &[
                Cell::Int(1),
                Cell::Text("a".into()),
                Cell::Decimal("1.5".into())
            ]
        )
        .is_err());
        assert!(encode_row(&s, &[Cell::Null, Cell::Text("a".into()), Cell::Null]).is_err());
        assert!(encode_row(&s, &[Cell::Int(1), Cell::Text("a\u{0}".into()), Cell::Null]).is_err());
        let forged = borsh::to_vec(&vec![
            Cell::Int(1),
            Cell::Text("a".into()),
            Cell::Decimal("1.5".into()),
        ])
        .expect("ok");
        assert!(decode_row(&s, &forged).is_err());
    }

    #[test]
    fn row_and_text_limits() {
        let s = schema();
        let at = "x".repeat(MAX_TEXT_BYTES);
        assert!(encode_row(&s, &[Cell::Int(1), Cell::Text(at.clone()), Cell::Null]).is_ok());
        let over = format!("{at}x");
        assert!(encode_row(&s, &[Cell::Int(1), Cell::Text(over), Cell::Null]).is_err());
        assert_eq!(
            decode_row(&s, &vec![0u8; MAX_ROW_BYTES + 1]),
            Err(RowError::TooLarge(MAX_ROW_BYTES + 1))
        );
        // A row of many text cells crosses the row cap before any text cap.
        let wide = Schema::from_json(&json!({"columns": (0..5).map(|i| if i == 0 {json!({"name":"k","kind":"int"})} else {json!({"name": format!("c{i}"), "kind":"text"})}).collect::<Vec<_>>(), "key":"k"})).expect("ok");
        let mut cells = vec![Cell::Int(1)];
        cells.extend((1..5).map(|_| Cell::Text(at.clone())));
        assert!(matches!(
            encode_row(&wide, &cells),
            Err(RowError::TooLarge(_))
        ));
    }

    fn widen(json: Value) -> Schema {
        Schema::from_json(&json).expect("ok")
    }

    /// The three schemas of a dataset that widened twice (ADR-0059
    /// amendment 5): `value` from 2 to 7 to 38 places, `n` from int to
    /// decimal, and a column added at each step.
    fn three() -> [Schema; 3] {
        [
            widen(json!({"columns":[
                {"name":"t","kind":"int"},
                {"name":"value","kind":"decimal","scale":2,"nullable":true},
                {"name":"n","kind":"int"}
            ],"key":"t"})),
            widen(json!({"columns":[
                {"name":"t","kind":"int"},
                {"name":"value","kind":"decimal","scale":7,"nullable":true},
                {"name":"n","kind":"decimal","scale":3},
                {"name":"a","kind":"text","nullable":true}
            ],"key":"t"})),
            widen(json!({"columns":[
                {"name":"t","kind":"int"},
                {"name":"value","kind":"decimal","scale":38,"nullable":true},
                {"name":"n","kind":"decimal","scale":5},
                {"name":"a","kind":"text","nullable":true},
                {"name":"b","kind":"int","nullable":true}
            ],"key":"t"})),
        ]
    }

    /// `present` pads with zeros only: the shown value is the stored one with
    /// fraction zeros added (its number never changes), an int gets zero
    /// fraction digits, added columns read null, and a value that already
    /// has the places is left alone. A stored decimal at the 80-character cap
    /// shows at 80 plus the added places (display only, never cut).
    #[test]
    fn present_pads_exactly_and_never_rounds() {
        let [s0, s1, s2] = three();
        assert_eq!(Schema::widens(&s0, &s1), Ok(()));
        assert_eq!(Schema::widens(&s1, &s2), Ok(()));
        let stored = vec![Cell::Int(5), Cell::Decimal("-711.54".into()), Cell::Int(-3)];
        assert!(check_cells(&s0, &stored).is_ok());
        let shown = present(&s0, &s1, &stored);
        assert_eq!(
            shown,
            vec![
                Cell::Int(5),
                Cell::Decimal("-711.5400000".into()),
                Cell::Decimal("-3.000".into()),
                Cell::Null
            ]
        );
        // The shown row fits the newer schema and is the same number.
        assert!(check_cells(&s1, &shown).is_ok());
        assert_eq!(
            canonical_decimal("-711.54", 7).as_deref(),
            Ok("-711.5400000")
        );
        // Zero and null stay what they are.
        let zero = vec![Cell::Int(0), Cell::Null, Cell::Int(0)];
        assert_eq!(
            present(&s0, &s2, &zero),
            vec![
                Cell::Int(0),
                Cell::Null,
                Cell::Decimal("0.00000".into()),
                Cell::Null,
                Cell::Null
            ]
        );
        // Under its own schema a row shows as stored.
        assert_eq!(present(&s0, &s0, &stored), stored);
        // A long stored value: 80 characters at scale 2 shows as 116 at 38.
        let long = format!("{}.25", "9".repeat(77));
        assert_eq!(long.len(), MAX_DECIMAL_CHARS);
        let row = vec![Cell::Int(1), Cell::Decimal(long.clone()), Cell::Int(1)];
        assert!(check_cells(&s0, &row).is_ok());
        let Cell::Decimal(d) = &present(&s0, &s2, &row)[1] else {
            panic!("decimal");
        };
        assert_eq!(d.len(), MAX_DECIMAL_CHARS + 36);
        assert!(d.starts_with(&long) && d[long.len()..].bytes().all(|c| c == b'0'));
        // Never rounds: a value with more places than the target keeps them.
        let many = vec![
            Cell::Int(1),
            Cell::Decimal("1.2345678".into()),
            Cell::Int(1),
        ];
        assert_eq!(
            present(&s1, &s0, &many)[1],
            Cell::Decimal("1.2345678".into())
        );
        // A row shorter than its schema: the missing cells read null.
        assert_eq!(present(&s0, &s1, &[Cell::Int(1)])[1], Cell::Null);
        // The JSON keys are the newest schema's, in its names.
        assert_eq!(
            cells_json(&s1, &shown),
            json!({"t":"5","value":"-711.5400000","n":"-3.000","a":null})
        );
    }

    /// `present` composes (`validate` checks adjacent pairs only): showing a
    /// row of schema 0 at schema 2 equals showing it at 1 and then at 2.
    #[test]
    fn present_composes_across_widenings() {
        let [s0, s1, s2] = three();
        for stored in [
            vec![Cell::Int(1), Cell::Decimal("0.01".into()), Cell::Int(7)],
            vec![Cell::Int(-9), Cell::Null, Cell::Int(i128::MIN)],
            vec![
                Cell::Int(2),
                Cell::Decimal("-123456.78".into()),
                Cell::Int(0),
            ],
        ] {
            assert!(check_cells(&s0, &stored).is_ok());
            let direct = present(&s0, &s2, &stored);
            let stepped = present(&s1, &s2, &present(&s0, &s1, &stored));
            assert_eq!(direct, stepped, "{stored:?}");
        }
        let at1 = vec![
            Cell::Int(3),
            Cell::Decimal("1.2345678".into()),
            Cell::Decimal("4.125".into()),
            Cell::Text("x".into()),
        ];
        assert!(check_cells(&s1, &at1).is_ok());
        let shown = present(&s1, &s2, &at1);
        assert!(check_cells(&s2, &shown).is_ok());
        assert_eq!(present(&s2, &s2, &shown), shown);
    }

    #[test]
    fn fields() {
        let k = ColumnKind::Decimal { scale: 2 };
        assert_eq!(parse_field(k, true, None), Ok(Cell::Null));
        assert!(parse_field(k, false, None).is_err());
        assert_eq!(
            parse_field(ColumnKind::Text, false, None),
            Ok(Cell::Text(String::new()))
        );
        assert_eq!(
            parse_field(ColumnKind::Int, false, Some("-42")),
            Ok(Cell::Int(-42))
        );
        assert!(parse_field(ColumnKind::Int, false, Some("4.2")).is_err());
        assert!(parse_field(ColumnKind::Int, false, Some("1e3")).is_err());
    }
}
