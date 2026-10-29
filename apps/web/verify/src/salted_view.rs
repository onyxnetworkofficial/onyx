//! A bought salted label row checked in the browser (ROADMAP D30, pre-public loop PP3): the same
//! check a native client runs (`onyx_labels::salted::verify_salted_answer`), against the content
//! hash the page pins, never the one the answer carries.

use onyx_core::Hash32;
use onyx_labels::salted::{json, verify_salted_answer};
use serde_json::{json as j, Value};

/// The content hash of the shared synthetic salted set (fixtures/dated/salted-vectors.json),
/// compiled in: the page checks the demo row against this, never against the file it loads.
pub const SALTED_TEST_PIN: &str =
    "0x6f4f13485be020d73e44c5050f93d9f82abf648cf623cafca34d06f7f7645700";

fn pin_of(hex_pin: &str) -> Result<Hash32, String> {
    let h = hex_pin
        .strip_prefix("0x")
        .filter(|h| h.len() == 64 && h.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
        .ok_or("the pin must be 0x and 64 lowercase hex digits")?;
    let mut a = [0u8; 32];
    hex::decode_to_slice(h, &mut a).map_err(|e| e.to_string())?;
    Ok(Hash32(a))
}

/// `{ok, error, index, address, entity, from, to}` for one answer against `pinned`.
pub fn check(answer: &Value, pinned: &str) -> Value {
    let r = pin_of(pinned).and_then(|pin| {
        let a = json::from_json(answer)?;
        let (m, _) = verify_salted_answer(&a, &pin).map_err(|e| format!("{e:?}: {e}"))?;
        Ok((a.index, m))
    });
    match r {
        Ok((index, m)) => {
            j!({"ok": true, "error": null, "index": index, "address": m.label.address,
            "entity": m.label.entity, "from": m.from, "to": m.to})
        }
        Err(e) => {
            j!({"ok": false, "error": e, "index": null, "address": null, "entity": null, "from": null, "to": null})
        }
    }
}

/// Whether a guessed row, hashed as an unsalted (format-2) leaf, equals a sibling hash: the
/// attack format 3 stops (a partner's Q7). `true` would mean the guess is confirmed.
pub fn guess_confirms(guess_row_hex: &str, sibling: &str) -> Result<bool, String> {
    let row = hex::decode(guess_row_hex.strip_prefix("0x").ok_or("0x hex")?)
        .map_err(|e| e.to_string())?;
    Ok(onyx_merkle::leaf_hash(&row) == pin_of(sibling)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shared_salted_vectors_pass_and_fail_as_listed() {
        let v: Value = serde_json::from_str(include_str!(
            "../../../../fixtures/dated/salted-vectors.json"
        ))
        .expect("vectors");
        let pin = v["contentHash"].as_str().expect("pin");
        assert_eq!(
            pin, SALTED_TEST_PIN,
            "the compiled pin is the vectors' content hash"
        );
        let cases = v["cases"].as_array().expect("cases");
        assert!(cases.len() >= 9);
        for c in cases {
            let got = check(&c["answer"], pin);
            if c["expect"] == "ok" {
                assert_eq!(got["ok"], true, "{}: {got}", c["case"]);
                assert_eq!(got["address"], c["expectAddress"]);
                assert_eq!(got["entity"], c["expectEntity"]);
            } else {
                assert_eq!(got["ok"], false, "{}", c["case"]);
                let want = c["expect"].as_str().expect("expect");
                assert!(
                    got["error"].as_str().expect("error").starts_with(want),
                    "{}: {got}",
                    c["case"]
                );
            }
        }
        // A wrong pin refuses a good row.
        let other = format!("0x{}", "1".repeat(64));
        assert_eq!(check(&cases[0]["answer"], &other)["ok"], false);
        let g = &v["guess"];
        assert_eq!(
            guess_confirms(
                g["row"].as_str().expect("row"),
                g["sibling"].as_str().expect("s")
            ),
            Ok(false)
        );
        assert_eq!(
            guess_confirms(
                g["row"].as_str().expect("row"),
                g["leafIfUnsalted"].as_str().expect("l")
            ),
            Ok(true)
        );
    }
}
