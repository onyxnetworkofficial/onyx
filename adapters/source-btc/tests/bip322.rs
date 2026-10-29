//! BIP-322 proof rule against the official vectors (made by btcd, an
//! independent implementation), the Onyx policy layer, and a full
//! first-party attestation end to end (ADR-0015).

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use bip322::{LockParams, Verification};
use bitcoin::key::{CompressedPublicKey, PrivateKey};
use bitcoin::secp256k1::{Message, Secp256k1, SecretKey};
use bitcoin::{Address, Network};
use onyx_core::{
    keccak256, AddressCanon, AddressProof, EntityKind, ProofOutcome, Registry, StaticRegistry,
};
use onyx_labels::attest::{eip191_digest, AttestContext, AttestError, Attestation, Statement};
use onyx_source_btc::{verify_checksum, Bip322Proof, BtcAddressCanon};
use serde_json::Value;
use std::path::PathBuf;

fn fixtures() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures"))
}

fn vectors(name: &str) -> Value {
    let rel = format!("bip322/{name}");
    verify_checksum(&fixtures(), &rel).expect("checksum");
    serde_json::from_str(&std::fs::read_to_string(fixtures().join(rel)).expect("read"))
        .expect("json")
}

fn ours(address: &str, message: &str, sig: &str) -> ProofOutcome {
    Bip322Proof.verify(address, message.as_bytes(), sig)
}

fn signatures(v: &Value) -> Vec<String> {
    v["bip322_signatures"]
        .as_array()
        .expect("sigs")
        .iter()
        .map(|s| s.as_str().expect("sig").to_string())
        .collect()
}

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v[k].as_str().expect(k)
}

/// Every valid simple vector passes, unless it relies on the unprefixed fallback.
#[test]
fn simple_vectors_valid() {
    let mut checked = 0;
    for file in ["basic-test-vectors.json", "generated-test-vectors.json"] {
        for v in vectors(file)["simple"].as_array().expect("simple") {
            for sig in signatures(v) {
                let want = if sig.starts_with("smp") {
                    ProofOutcome::Valid
                } else {
                    // The crate accepts the legacy unprefixed form; Onyx does not.
                    assert!(matches!(
                        bip322::verify_simple_encoded(s(v, "address"), s(v, "message"), &sig),
                        Ok(Verification::Valid { .. })
                    ));
                    ProofOutcome::Invalid
                };
                assert_eq!(ours(s(v, "address"), s(v, "message"), &sig), want, "{v}");
                checked += 1;
            }
        }
    }
    assert!(checked >= 10, "{checked}");
}

/// The generated full vectors all use time locks (T = S = 2016). The crate
/// reports "valid at T, S" for the 8 key and multisig types; Onyx refuses
/// time-locked proofs.
#[test]
fn time_locked_full_vectors_refused() {
    let g = vectors("generated-test-vectors.json");
    let full = g["full"].as_array().expect("full");
    assert_eq!(full.len(), 10);
    for v in full {
        for sig in signatures(v) {
            let r = bip322::verify_full_encoded(s(v, "address"), s(v, "message"), &sig);
            if s(v, "type").ends_with("time-lock") {
                // The crate cannot run time-lock scripts and reports them invalid
                // (not inconclusive). Onyx refuses them either way.
                assert!(
                    matches!(r, Err(bip322::Error::InvalidWitness)),
                    "{v}: {r:?}"
                );
            } else {
                assert!(
                    matches!(r, Ok(Verification::Valid { time, .. }) if time.to_consensus_u32() == 2016),
                    "{v}: {r:?}"
                );
            }
            assert_eq!(
                ours(s(v, "address"), s(v, "message"), &sig),
                ProofOutcome::Invalid
            );
        }
    }
}

#[test]
fn proof_of_funds_refused() {
    let g = vectors("generated-test-vectors.json");
    for v in g["proof_of_funds"].as_array().expect("pof") {
        for sig in signatures(v) {
            assert!(sig.starts_with("pof"));
            assert_eq!(
                ours(s(v, "address"), s(v, "message"), &sig),
                ProofOutcome::Invalid
            );
        }
    }
}

#[test]
fn error_vectors_never_valid() {
    let mut n = 0;
    for file in ["basic-test-vectors.json", "generated-test-vectors.json"] {
        for v in vectors(file)["error"].as_array().expect("error") {
            assert_eq!(
                ours(s(v, "address"), s(v, "message"), s(v, "signature")),
                ProofOutcome::Invalid,
                "{}",
                s(v, "description")
            );
            n += 1;
        }
    }
    assert_eq!(n, 36);
}

/// Test keys derived from fixed labels. Not real keys; they hold nothing.
fn test_key(label: &str) -> SecretKey {
    SecretKey::from_slice(&keccak256(label.as_bytes()).0).expect("scalar")
}

fn wif(sk: &SecretKey) -> String {
    PrivateKey::new(*sk, Network::Bitcoin).to_wif()
}

fn addresses(sk: &SecretKey, network: Network) -> [(&'static str, String); 3] {
    let secp = Secp256k1::new();
    let pk = CompressedPublicKey(sk.public_key(&secp));
    let (xonly, _) = sk.public_key(&secp).x_only_public_key();
    [
        ("p2wpkh", Address::p2wpkh(&pk, network).to_string()),
        (
            "p2tr",
            Address::p2tr(&secp, xonly, None, network).to_string(),
        ),
        ("p2pkh", Address::p2pkh(pk, network).to_string()),
    ]
}

/// Full proofs without time locks are accepted for every supported type;
/// simple proofs only for native segwit.
#[test]
fn self_signed_full_and_simple() {
    let sk = test_key("onyx test btc key 1");
    for (kind, addr) in addresses(&sk, Network::Bitcoin) {
        assert!(BtcAddressCanon.is_canonical(&addr), "{addr}");
        let full = bip322::sign_full_encoded(&addr, "m", &[wif(&sk)], None, LockParams::default())
            .expect("sign full");
        assert_eq!(ours(&addr, "m", &full), ProofOutcome::Valid, "{kind} full");
        assert_eq!(
            ours(&addr, "n", &full),
            ProofOutcome::Invalid,
            "{kind} full, other msg"
        );
        if kind != "p2pkh" {
            let simple = bip322::sign_simple_encoded(&addr, "m", &[wif(&sk)], None).expect("sign");
            assert_eq!(
                ours(&addr, "m", &simple),
                ProofOutcome::Valid,
                "{kind} simple"
            );
        }
    }
}

/// One proof, one accepted encoding: extra base64 padding, non-zero trailing
/// bits and trailing bytes after the witness are refused (the crate ignores
/// trailing witness bytes).
#[test]
fn non_canonical_encodings_refused() {
    let sk = test_key("onyx test btc key 1");
    let [(_, addr), ..] = addresses(&sk, Network::Bitcoin);
    let sig = bip322::sign_simple_encoded(&addr, "m", &[wif(&sk)], None).expect("sign");
    assert_eq!(ours(&addr, "m", &sig), ProofOutcome::Valid);

    let mut raw = STANDARD.decode(&sig[3..]).expect("b64");
    raw.push(0);
    let trailing = format!("smp{}", STANDARD.encode(&raw));
    assert!(matches!(
        bip322::verify_simple_encoded(&addr, "m", &trailing),
        Ok(Verification::Valid { .. })
    ));
    assert_eq!(ours(&addr, "m", &trailing), ProofOutcome::Invalid);

    assert_eq!(ours(&addr, "m", &format!("{sig}=")), ProofOutcome::Invalid);
    assert_eq!(ours(&addr, "m", &format!(" {sig}")), ProofOutcome::Invalid);
    assert_eq!(
        ours(&addr, "m", &sig.replace("smp", "SMP")),
        ProofOutcome::Invalid
    );
    // Flip the low bits of the last base64 digit before the padding: decoders
    // that ignore trailing bits would accept a second spelling.
    let body = sig.trim_end_matches('=');
    let pad = &sig[body.len()..];
    if !pad.is_empty() {
        let last = body.chars().last().expect("char");
        const ALPHA: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let i = ALPHA.find(last).expect("b64 char");
        let alt = ALPHA.as_bytes()[i ^ 1] as char;
        let respelled = format!("{}{alt}{pad}", &body[..body.len() - 1]);
        assert_eq!(ours(&addr, "m", &respelled), ProofOutcome::Invalid);
    }
}

#[test]
fn non_mainnet_and_wrong_variant_refused() {
    let sk = test_key("onyx test btc key 1");
    for (kind, addr) in addresses(&sk, Network::Testnet) {
        let full = bip322::sign_full_encoded(&addr, "m", &[wif(&sk)], None, LockParams::default())
            .expect("sign");
        assert_eq!(ours(&addr, "m", &full), ProofOutcome::Invalid, "{kind}");
    }
    let [(_, wpkh), _, (_, pkh)] = addresses(&sk, Network::Bitcoin);
    let simple = bip322::sign_simple_encoded(&wpkh, "m", &[wif(&sk)], None).expect("sign");
    // A simple proof cannot be relabelled as full, nor a legacy proof used.
    assert_eq!(
        ours(&wpkh, "m", &simple.replacen("smp", "ful", 1)),
        ProofOutcome::Invalid
    );
    let legacy = bip322::sign_legacy_encoded(&pkh, "m", &wif(&sk)).expect("legacy");
    assert_eq!(ours(&pkh, "m", &legacy), ProofOutcome::Invalid);
}

fn eip191_sign(sk: &SecretKey, msg: &[u8]) -> [u8; 65] {
    let (recid, rs) = Secp256k1::new()
        .sign_ecdsa_recoverable(&Message::from_digest(eip191_digest(msg).0), sk)
        .serialize_compact();
    let mut out = [0u8; 65];
    out[..64].copy_from_slice(&rs);
    out[64] = 27 + u8::try_from(recid.to_i32()).expect("recid");
    out
}

fn evm_address(sk: &SecretKey) -> [u8; 20] {
    let pk = sk.public_key(&Secp256k1::new()).serialize_uncompressed();
    let mut out = [0u8; 20];
    out.copy_from_slice(&keccak256(&pk[1..]).0[12..]);
    out
}

/// End to end: a real BIP-322 address signature plus an EIP-191 entity
/// signature, checked against a registry with the BTC canon and proof rule.
#[test]
fn attestation_end_to_end() {
    let btc = test_key("onyx test btc key 1");
    let entity = test_key("onyx test entity key 1");
    let reg = test_registry(&entity);
    assert!(reg.address_proof("btc").is_some());

    for (kind, addr) in addresses(&btc, Network::Bitcoin) {
        let st = Statement {
            domain: "onyx.devnet".into(),
            entity: "entity:example".into(),
            address: format!("btc:{addr}"),
            valid_from: 921_000,
            expires: 921_000 + 52_560,
            nonce: [9; 16],
        };
        let text = st.render();
        let address_sig =
            bip322::sign_full_encoded(&addr, &text, &[wif(&btc)], None, LockParams::default())
                .expect("sign");
        let a = Attestation {
            address_sig,
            entity_key: evm_address(&entity),
            entity_sig: eip191_sign(&entity, text.as_bytes()),
            statement: text,
        };
        let ctx = AttestContext {
            ranges: &[("btc", 921_080, 921_082)],
        };
        assert_eq!(a.verify(&ctx, &reg), Ok(st.clone()), "{kind}");

        // A real proof of the same statement by another key, for that key's own
        // address of the same type, presented for this address (L12-F8a).
        let other = test_key("onyx test btc key 2");
        let other_addr = addresses(&other, Network::Bitcoin)
            .into_iter()
            .find(|(k, _)| *k == kind)
            .expect("same kind")
            .1;
        let other_sig = bip322::sign_full_encoded(
            &other_addr,
            &a.statement,
            &[wif(&other)],
            None,
            LockParams::default(),
        )
        .expect("other key signs its own address");
        assert_eq!(
            Bip322Proof.verify(&other_addr, a.statement.as_bytes(), &other_sig),
            ProofOutcome::Valid
        );
        let mut b = a.clone();
        b.address_sig = other_sig;
        assert_eq!(
            b.verify(&ctx, &reg),
            Err(AttestError::AddressSigInvalid),
            "{kind}"
        );

        // Window one block longer than a year.
        let mut long = st.clone();
        long.expires += 1;
        let mut c = a.clone();
        c.statement = long.render();
        assert_eq!(c.verify(&ctx, &reg), Err(AttestError::BadWindow), "{kind}");
    }
}

/// L12-F2: the registry vectors cover every accepted type and variant and each
/// refusal; a rule that differs on any of them cannot be registered.
#[test]
fn registry_vectors_cover_types_and_refusals() {
    let vs = Bip322Proof.vectors();
    assert_eq!(vs.len(), 29);
    let count = |o| vs.iter().filter(|v| v.expect == o).count();
    assert_eq!(count(ProofOutcome::Valid), 13);
    assert_eq!(count(ProofOutcome::Invalid), 13);
    assert_eq!(count(ProofOutcome::Inconclusive), 3);
    for v in vs {
        assert_eq!(
            Bip322Proof.verify(v.body, v.message.as_bytes(), v.signature),
            v.expect,
            "{} {}",
            v.body,
            &v.signature[..12]
        );
    }
    // Policy refusals the crate itself accepts: trailing witness byte, empty
    // P2SH scriptSig, taproot 0x00 hash type, 547-byte redeem, 3 time locks,
    // simple proof for wrapped P2WPKH.
    let crate_accepts = vs
        .iter()
        .filter(|v| v.expect == ProofOutcome::Invalid)
        .filter(|v| {
            let r = if v.signature.starts_with("smp") {
                bip322::verify_simple_encoded(v.body, v.message, v.signature)
            } else if v.signature.starts_with("ful") {
                bip322::verify_full_encoded(v.body, v.message, v.signature)
            } else {
                return false;
            };
            matches!(r, Ok(Verification::Valid { .. }))
        })
        .count();
    assert_eq!(crate_accepts, 8);

    /// The reviewer's drift: same id, but every `ful` proof is valid.
    struct Drift;
    impl AddressProof for Drift {
        fn namespace(&self) -> &str {
            "btc"
        }
        fn chain(&self) -> &str {
            Bip322Proof.chain()
        }
        fn proof_id(&self) -> &str {
            Bip322Proof.proof_id()
        }
        fn max_window(&self) -> u64 {
            Bip322Proof.max_window()
        }
        fn vectors(&self) -> &'static [onyx_core::ProofVector] {
            Bip322Proof.vectors()
        }
        fn verify(&self, body: &str, msg: &[u8], sig: &str) -> ProofOutcome {
            if sig.starts_with("ful") {
                ProofOutcome::Valid
            } else {
                Bip322Proof.verify(body, msg, sig)
            }
        }
    }
    let mut r = StaticRegistry::new();
    r.add_canon(Box::new(BtcAddressCanon)).expect("canon");
    assert!(r.add_proof(Box::new(Drift)).is_err());
    assert!(r.add_proof(Box::new(Bip322Proof)).is_ok());
}

fn test_registry(entity: &SecretKey) -> StaticRegistry {
    let mut reg = StaticRegistry::new();
    reg.set_domain("onyx.devnet")
        .and_then(|r| r.add_entity("entity:example", EntityKind::Exchange))
        .and_then(|r| r.add_canon(Box::new(BtcAddressCanon)))
        .and_then(|r| r.add_proof(Box::new(Bip322Proof)))
        .and_then(|r| r.add_entity_key("entity:example", evm_address(entity)))
        .and_then(|r| {
            r.add_source(onyx_core::DataRights::public_chain(onyx_core::SourceId(
                onyx_source_btc::SOURCE_ID.into(),
            )))
        })
        .expect("registry");
    reg
}

/// L13-F3: the judging range comes from the shard's snapshot. An attestation
/// valid from 921,081 covers a shard over 921,081-921,082 but not one that
/// starts at 921,080; there the label is dropped, not the set.
#[test]
fn shard_range_decides_attestation() {
    use onyx_core::{
        ChainId, Confidence, EvidenceRef, LabelSetRef, ShardSpec, Snapshot, SourceId, SourceRef,
    };
    use onyx_labels::{DropReason, Label, LabelSet, ATTESTATION_EVIDENCE};
    use onyx_recipe_host::assemble_input;

    let btc = test_key("onyx test btc key 1");
    let entity = test_key("onyx test entity key 1");
    let reg = test_registry(&entity);
    let [(_, addr), ..] = addresses(&btc, Network::Bitcoin);
    let text = Statement {
        domain: "onyx.devnet".into(),
        entity: "entity:example".into(),
        address: format!("btc:{addr}"),
        valid_from: 921_081,
        expires: 930_000,
        nonce: [4; 16],
    }
    .render();
    let att = Attestation {
        address_sig: bip322::sign_simple_encoded(&addr, &text, &[wif(&btc)], None).expect("sign"),
        entity_key: evm_address(&entity),
        entity_sig: eip191_sign(&entity, text.as_bytes()),
        statement: text,
    };
    let set = LabelSet {
        name: "onyx.test".into(),
        version: 1,
        labels: vec![Label {
            address: format!("btc:{addr}"),
            entity: "entity:example".into(),
            kind: EntityKind::Exchange,
            confidence: Confidence(Confidence::ONE),
            evidence: vec![EvidenceRef {
                kind: ATTESTATION_EVIDENCE.into(),
                content_hash: att.content_hash(),
            }],
        }],
    };
    let source = SourceId(onyx_source_btc::SOURCE_ID.into());
    let spec = |from_height: u64| ShardSpec {
        recipe: None,
        runtime: None,
        label_set: Some(LabelSetRef {
            name: set.name.clone(),
            version: set.version,
            content_hash: set.content_hash(),
        }),
        rights_hash: reg.source_rights(&source).expect("rights").hash(),
        source: SourceRef {
            source: source.clone(),
            snapshot: Snapshot::BlockRange {
                chain: ChainId(onyx_source_btc::CHAIN_ID.into()),
                from_height,
                to_height: 921_082,
                tip_hash: keccak256(b"tip"),
                content_hash: keccak256(&[]),
            },
        },
        schema: keccak256(b"schema"),
        params: vec![],
        registry_root: reg.root(),
    };
    // Without the cited attestation the party abstains (L14-F1).
    assert!(matches!(
        assemble_input(&spec(921_081), vec![], Some(&set), &[], &reg),
        Err(onyx_recipe_host::HostError::Abstain(_))
    ));
    let inside = assemble_input(
        &spec(921_081),
        vec![],
        Some(&set),
        std::slice::from_ref(&att),
        &reg,
    )
    .expect("assemble");
    assert!(inside.dropped.is_empty());
    let early = assemble_input(
        &spec(921_080),
        vec![],
        Some(&set),
        std::slice::from_ref(&att),
        &reg,
    )
    .expect("assemble");
    assert_eq!(
        early.dropped,
        vec![(
            format!("btc:{addr}"),
            DropReason::Invalid(AttestError::NotYetValid)
        )]
    );
}
