//! The publisher's input: CSV (RFC 4180: a header line of column names,
//! commas, `"`-quoted fields with `""` for a quote) or JSON Lines (one object
//! per line). Each record is checked against the schema here; nothing is
//! rounded or guessed (a value that does not fit is an error with its line).

use crate::row::{parse_field, Cell};
use crate::schema::{ColumnKind, Schema};
use serde_json::Value;
use std::io::BufRead;

/// Longest input record (bytes); far above any row that fits a shard.
pub const MAX_RECORD_BYTES: usize = 1 << 20;

/// An input record that does not fit.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[error("line {line}: {why}")]
pub struct InputError {
    /// 1-based line where the record starts.
    pub line: u64,
    /// What is wrong.
    pub why: String,
}

/// Input format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    /// CSV with a header line.
    Csv,
    /// JSON Lines.
    JsonLines,
}

impl Format {
    /// By file name: `.csv`, `.jsonl` or `.ndjson`.
    pub fn from_path(p: &str) -> Option<Format> {
        let l = p.to_ascii_lowercase();
        if l.ends_with(".csv") {
            Some(Format::Csv)
        } else if l.ends_with(".jsonl") || l.ends_with(".ndjson") {
            Some(Format::JsonLines)
        } else {
            None
        }
    }
}

/// Reads records as cells in schema order.
pub struct Records<R: BufRead> {
    r: R,
    schema: Schema,
    format: Format,
    /// For CSV: input column position of each schema column.
    order: Vec<usize>,
    line: u64,
}

fn err(line: u64, why: impl Into<String>) -> InputError {
    InputError {
        line,
        why: why.into(),
    }
}

/// Split one CSV record (already joined across quoted newlines) into fields;
/// `None` is an empty unquoted field.
fn csv_fields(rec: &str) -> Result<Vec<Option<String>>, String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut was_quoted = false;
    let mut chars = rec.chars().peekable();
    let mut at_start = true;
    while let Some(c) = chars.next() {
        if quoted {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    cur.push('"');
                } else {
                    quoted = false;
                    if !matches!(chars.peek(), None | Some(',')) {
                        return Err("text after a closing quote".into());
                    }
                }
            } else {
                cur.push(c);
            }
            continue;
        }
        match c {
            '"' if at_start => {
                quoted = true;
                was_quoted = true;
            }
            '"' => return Err("a quote inside an unquoted field".into()),
            ',' => {
                out.push(if cur.is_empty() && !was_quoted {
                    None
                } else {
                    Some(std::mem::take(&mut cur))
                });
                was_quoted = false;
                at_start = true;
                continue;
            }
            _ => cur.push(c),
        }
        at_start = false;
    }
    if quoted {
        return Err("an unclosed quote".into());
    }
    out.push(if cur.is_empty() && !was_quoted {
        None
    } else {
        Some(cur)
    });
    Ok(out)
}

impl<R: BufRead> Records<R> {
    /// A reader over `r`. For CSV the header line is read now and must name
    /// every schema column exactly once and nothing else (any order).
    pub fn new(mut r: R, schema: Schema, format: Format) -> Result<Self, InputError> {
        let mut order = Vec::new();
        let mut line = 0;
        if format == Format::Csv {
            let mut head = String::new();
            r.read_line(&mut head).map_err(|e| err(1, e.to_string()))?;
            line = 1;
            let head = head
                .trim_end_matches(['\r', '\n'])
                .trim_start_matches('\u{feff}');
            let names = csv_fields(head).map_err(|e| err(1, e))?;
            let names: Vec<String> = names.into_iter().map(Option::unwrap_or_default).collect();
            if names.len() != schema.columns.len() {
                return Err(err(
                    1,
                    format!(
                        "header has {} columns, the schema {}",
                        names.len(),
                        schema.columns.len()
                    ),
                ));
            }
            for c in &schema.columns {
                let pos: Vec<usize> = names
                    .iter()
                    .enumerate()
                    .filter(|(_, n)| **n == c.name)
                    .map(|(i, _)| i)
                    .collect();
                match pos[..] {
                    [p] => order.push(p),
                    _ => {
                        return Err(err(
                            1,
                            format!("header must name column {} exactly once", c.name),
                        ))
                    }
                }
            }
        }
        Ok(Records {
            r,
            schema,
            format,
            order,
            line,
        })
    }

    /// Read one physical line into `buf` (without its end); false at the end.
    fn read_line(&mut self, buf: &mut String) -> Result<bool, InputError> {
        let start = self.line + 1;
        let mut raw = Vec::new();
        let n = std::io::Read::take(&mut self.r, MAX_RECORD_BYTES as u64 + 2)
            .read_until(b'\n', &mut raw)
            .map_err(|e| err(start, e.to_string()))?;
        if n == 0 {
            return Ok(false);
        }
        self.line += 1;
        if raw.len() > MAX_RECORD_BYTES + 1 {
            return Err(err(start, format!("line over {MAX_RECORD_BYTES} bytes")));
        }
        let s = String::from_utf8(raw).map_err(|_| err(start, "not UTF-8"))?;
        buf.push_str(s.trim_end_matches(['\r', '\n']));
        Ok(true)
    }

    /// The next record's cells in schema order, `None` at the end. Blank
    /// lines are skipped.
    pub fn next_record(&mut self) -> Result<Option<(u64, Vec<Cell>)>, InputError> {
        loop {
            let mut rec = String::new();
            if !self.read_line(&mut rec)? {
                return Ok(None);
            }
            let start = self.line;
            if rec.trim().is_empty() {
                continue;
            }
            let cells = match self.format {
                Format::Csv => {
                    // A quoted field may hold newlines: join lines while the
                    // quotes are unbalanced.
                    while rec.matches('"').count() % 2 == 1 {
                        rec.push('\n');
                        if rec.len() > MAX_RECORD_BYTES || !self.read_line(&mut rec)? {
                            return Err(err(start, "an unclosed quote"));
                        }
                    }
                    self.csv_cells(start, &rec)?
                }
                Format::JsonLines => self.json_cells(start, &rec)?,
            };
            return Ok(Some((start, cells)));
        }
    }

    fn csv_cells(&self, line: u64, rec: &str) -> Result<Vec<Cell>, InputError> {
        let fields = csv_fields(rec).map_err(|e| err(line, e))?;
        if fields.len() != self.order.len() {
            return Err(err(
                line,
                format!("{} fields, the header {}", fields.len(), self.order.len()),
            ));
        }
        self.schema
            .columns
            .iter()
            .zip(&self.order)
            .map(|(c, &p)| {
                parse_field(c.kind, c.nullable, fields[p].as_deref())
                    .map_err(|e| err(line, format!("column {}: {e}", c.name)))
            })
            .collect()
    }

    fn json_cells(&self, line: u64, rec: &str) -> Result<Vec<Cell>, InputError> {
        let v: Value =
            serde_json::from_str(rec).map_err(|e| err(line, format!("not JSON: {e}")))?;
        let obj = v
            .as_object()
            .ok_or_else(|| err(line, "not a JSON object"))?;
        if let Some(k) = obj.keys().find(|k| self.schema.index_of(k).is_none()) {
            return Err(err(line, format!("unknown column {k}")));
        }
        self.schema
            .columns
            .iter()
            .map(|c| {
                let e = |why: String| err(line, format!("column {}: {why}", c.name));
                match obj.get(&c.name) {
                    None | Some(Value::Null) => parse_field(c.kind, c.nullable, None).map_err(e),
                    Some(Value::String(s)) => parse_field(c.kind, c.nullable, Some(s)).map_err(e),
                    // Integers may be JSON numbers (exact integers only);
                    // decimals must be strings (no float ever, ADR-0009).
                    Some(Value::Number(n)) if c.kind == ColumnKind::Int => {
                        let text = n.to_string();
                        if n.is_f64() {
                            return Err(e(format!("{text} is not a whole number")));
                        }
                        parse_field(c.kind, c.nullable, Some(&text)).map_err(e)
                    }
                    Some(Value::Number(n)) => Err(e(format!(
                        "{n} as a JSON number: write decimals as strings (\"1.25\")"
                    ))),
                    Some(_) => Err(e("not a string or number".into())),
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn schema() -> Schema {
        Schema::from_json(&json!({"columns":[
            {"name":"t","kind":"int"},
            {"name":"name","kind":"text"},
            {"name":"v","kind":"decimal","scale":2,"nullable":true}
        ],"key":"t"}))
        .expect("ok")
    }

    fn all(input: &str, f: Format) -> Result<Vec<Vec<Cell>>, InputError> {
        let mut r = Records::new(input.as_bytes(), schema(), f)?;
        let mut out = Vec::new();
        while let Some((_, c)) = r.next_record()? {
            out.push(c);
        }
        Ok(out)
    }

    #[test]
    fn csv_with_quotes_and_reordered_header() {
        let rows = all(
            "v,t,name\r\n1.5,1,\"a, \"\"b\"\"\"\n,2,\"multi\nline\"\n\n",
            Format::Csv,
        )
        .expect("ok");
        assert_eq!(
            rows,
            vec![
                vec![
                    Cell::Int(1),
                    Cell::Text("a, \"b\"".into()),
                    Cell::Decimal("1.50".into())
                ],
                vec![Cell::Int(2), Cell::Text("multi\nline".into()), Cell::Null],
            ]
        );
    }

    #[test]
    fn csv_refusals_name_the_line() {
        assert_eq!(all("t,name\n", Format::Csv).expect_err("refused").line, 1);
        assert_eq!(
            all("t,name,name\n", Format::Csv).expect_err("refused").line,
            1
        );
        let e = all("t,name,v\n1,a,1.5\n2,b,1.234\n", Format::Csv).expect_err("refused");
        assert_eq!(e.line, 3);
        assert!(e.why.contains("decimal places"), "{e}");
        assert!(all("t,name,v\n1,a\n", Format::Csv).is_err());
        assert!(all("t,name,v\nx,a,1\n", Format::Csv).is_err());
        assert!(all("t,name,v\n1,a\"b,1\n", Format::Csv).is_err());
        assert!(all("t,name,v\n1,\"open,1\n", Format::Csv).is_err());
    }

    #[test]
    fn json_lines() {
        let rows = all(
            "{\"t\":1,\"name\":\"a\",\"v\":\"2.5\"}\n{\"t\":\"170141183460469231731687303715884105727\",\"name\":\"b\"}\n",
            Format::JsonLines,
        )
        .expect("ok");
        assert_eq!(
            rows[0],
            vec![
                Cell::Int(1),
                Cell::Text("a".into()),
                Cell::Decimal("2.50".into())
            ]
        );
        assert_eq!(rows[1][0], Cell::Int(i128::MAX));
        assert_eq!(rows[1][2], Cell::Null);
        for bad in [
            "{\"t\":1.5,\"name\":\"a\"}",
            "{\"t\":1,\"name\":\"a\",\"v\":2.5}",
            "{\"t\":1,\"name\":\"a\",\"x\":1}",
            "[1,2]",
            "{\"name\":\"a\"}",
        ] {
            assert!(all(bad, Format::JsonLines).is_err(), "{bad}");
        }
    }

    #[test]
    fn long_lines_are_refused() {
        let long = format!("t,name,v\n1,{},1\n", "x".repeat(MAX_RECORD_BYTES + 10));
        assert!(all(&long, Format::Csv).is_err());
    }
}
