//! Generate `src/proof_vectors.rs`: the BIP-322 rule's registry vectors, one
//! per accepted address type and variant, plus each policy refusal and an
//! inconclusive case (L12-F2). Their results go into the registry root, so a
//! verifier that behaves differently on any of them has a different root.
//!
//! Keys are derived from fixed labels (keccak256); they are test keys only and
//! hold nothing. Run: `cargo run -p onyx-source-btc --example gen-proof-vectors > adapters/source-btc/src/proof_vectors.rs`
//! then `cargo fmt`. Only with a new `proof_id` (ADR-0015).

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use bip322::LockParams;
use bitcoin::absolute::LockTime;
use bitcoin::consensus::{deserialize, serialize};
use bitcoin::key::{CompressedPublicKey, Keypair, PrivateKey, TapTweak};
use bitcoin::secp256k1::{Message, Secp256k1, SecretKey};
use bitcoin::sighash::{Prevouts, SighashCache, TapSighashType};
use bitcoin::{
    opcodes, Address, Amount, Network, ScriptBuf, Sequence, Transaction, TxOut, Witness,
};
use onyx_core::keccak256;

const MSG: &str = "Onyx proof vector v1";

fn key(label: &str) -> SecretKey {
    SecretKey::from_slice(&keccak256(label.as_bytes()).0).expect("scalar")
}

fn wif(sk: &SecretKey) -> String {
    PrivateKey::new(*sk, Network::Bitcoin).to_wif()
}

fn multisig(m: i64, keys: &[bitcoin::PublicKey]) -> ScriptBuf {
    let mut b = ScriptBuf::builder().push_int(m);
    for k in keys {
        b = b.push_key(k);
    }
    b.push_int(i64::try_from(keys.len()).expect("len"))
        .push_opcode(opcodes::all::OP_CHECKMULTISIG)
        .into_script()
}

fn ful(tx: &Transaction) -> String {
    format!("ful{}", STANDARD.encode(serialize(tx)))
}

fn smp(w: &Witness) -> String {
    format!("smp{}", STANDARD.encode(serialize(w)))
}

fn decode_ful(sig: &str) -> Transaction {
    deserialize(&STANDARD.decode(&sig[3..]).expect("b64")).expect("tx")
}

struct V {
    note: &'static str,
    body: String,
    message: &'static str,
    signature: String,
    expect: &'static str,
}

fn main() {
    let secp = Secp256k1::new();
    let sk = key("onyx proof vector key 1");
    let pk = CompressedPublicKey(sk.public_key(&secp));
    let (xonly, _) = sk.public_key(&secp).x_only_public_key();
    let wpkh = Address::p2wpkh(&pk, Network::Bitcoin).to_string();
    let tr = Address::p2tr(&secp, xonly, None, Network::Bitcoin);
    let pkh = Address::p2pkh(pk, Network::Bitcoin).to_string();
    let shwpkh = Address::p2shwpkh(&pk, Network::Bitcoin).to_string();

    let ms_keys: Vec<SecretKey> = (0..3)
        .map(|i| key(&format!("onyx proof vector ms {i}")))
        .collect();
    let ms_pubs: Vec<bitcoin::PublicKey> = ms_keys
        .iter()
        .map(|s| bitcoin::PublicKey::new(s.public_key(&secp)))
        .collect();
    let ms_privs: Vec<PrivateKey> = ms_keys[..2]
        .iter()
        .map(|s| PrivateKey::new(*s, Network::Bitcoin))
        .collect();
    let ws = multisig(2, &ms_pubs);
    let wsh = Address::p2wsh(&ws, Network::Bitcoin);
    let sh = Address::p2sh(&ws, Network::Bitcoin).expect("p2sh");
    let shwsh = Address::p2shwsh(&ws, Network::Bitcoin);

    let one = || [wif(&sk)];
    let full = |a: &str| {
        bip322::sign_full_encoded(a, MSG, &one(), None, LockParams::default()).expect("sign full")
    };
    let simple = |a: &str| bip322::sign_simple_encoded(a, MSG, &one(), None).expect("sign simple");
    let ms_full = |a: &Address| {
        ful(&bip322::sign_full(a, MSG, &ms_privs, Some(&ws), LockParams::default()).expect("ms"))
    };

    let mut v: Vec<V> = Vec::new();
    let mut push = |note, body: String, signature: String, expect| {
        v.push(V {
            note,
            body,
            message: MSG,
            signature,
            expect,
        })
    };
    let tr_s = tr.to_string();

    // Accepted: every supported type and variant.
    push("p2wpkh simple", wpkh.clone(), simple(&wpkh), "Valid");
    push("p2wpkh full", wpkh.clone(), full(&wpkh), "Valid");
    push("p2tr key path simple", tr_s.clone(), simple(&tr_s), "Valid");
    push("p2tr key path full", tr_s.clone(), full(&tr_s), "Valid");
    push("p2pkh full", pkh.clone(), full(&pkh), "Valid");
    push("p2sh-p2wpkh full", shwpkh.clone(), full(&shwpkh), "Valid");
    let wsh_full = ms_full(&wsh);
    let wsh_witness = decode_ful(&wsh_full).input[0].witness.clone();
    push(
        "p2wsh 2-of-3 simple",
        wsh.to_string(),
        smp(&wsh_witness),
        "Valid",
    );
    push("p2wsh 2-of-3 full", wsh.to_string(), wsh_full, "Valid");
    push("p2sh 2-of-3 full", sh.to_string(), ms_full(&sh), "Valid");
    push(
        "p2sh-p2wsh 2-of-3 full",
        shwsh.to_string(),
        ms_full(&shwsh),
        "Valid",
    );

    // Refused by Onyx policy.
    let s = simple(&wpkh);
    push(
        "unprefixed simple",
        wpkh.clone(),
        s[3..].to_string(),
        "Invalid",
    );
    push(
        "pof prefix",
        wpkh.clone(),
        format!("pof{}", &full(&wpkh)[3..]),
        "Invalid",
    );
    push(
        "legacy BIP-137",
        pkh.clone(),
        bip322::sign_legacy_encoded(&pkh, MSG, &wif(&sk)).expect("legacy"),
        "Invalid",
    );
    let locks = LockParams {
        lock_time: LockTime::from_consensus(2016),
        sequence: Sequence(2016),
    };
    push(
        "time-locked full",
        wpkh.clone(),
        bip322::sign_full_encoded(&wpkh, MSG, &one(), None, locks).expect("locked"),
        "Invalid",
    );
    // L14-F9: to_sign versions other than 0 and 2 are inconclusive (BIP-322 upgradeable rule).
    for (note, version) in [("to_sign version 1", 1), ("to_sign version 3", 3)] {
        let mut tx = decode_ful(&full(&wpkh));
        tx.version = bitcoin::transaction::Version(version);
        push(note, wpkh.clone(), ful(&tx), "Inconclusive");
    }
    // L13-F8: each time lock alone is refused too.
    for (note, lock_time, sequence) in [("lock time only", 1, 0), ("sequence only", 0, 1)] {
        let locks = LockParams {
            lock_time: LockTime::from_consensus(lock_time),
            sequence: Sequence(sequence),
        };
        push(
            note,
            wpkh.clone(),
            bip322::sign_full_encoded(&wpkh, MSG, &one(), None, locks).expect("locked"),
            "Invalid",
        );
    }
    let mut raw = STANDARD.decode(&s[3..]).expect("b64");
    raw.push(0);
    push(
        "trailing witness byte",
        wpkh.clone(),
        format!("smp{}", STANDARD.encode(&raw)),
        "Invalid",
    );
    let shwpkh_full = full(&shwpkh);
    let shwpkh_tx = decode_ful(&shwpkh_full);
    push(
        "p2sh-p2wpkh as simple",
        shwpkh.clone(),
        smp(&shwpkh_tx.input[0].witness),
        "Invalid",
    );
    let mut stripped = shwpkh_tx.clone();
    stripped.input[0].script_sig = ScriptBuf::new();
    push(
        "p2sh-p2wpkh full, empty scriptSig (L12-F3)",
        shwpkh.clone(),
        ful(&stripped),
        "Invalid",
    );

    // L12-F4: taproot 64-byte signature, and the same with an explicit 0x00 hash type.
    let to_spend = bip322::create_to_spend(&tr, MSG).expect("to_spend");
    let to_sign = bip322::create_to_sign(&to_spend, None, LockParams::default()).expect("to_sign");
    let prevout = TxOut {
        value: Amount::ZERO,
        script_pubkey: tr.script_pubkey(),
    };
    let sighash = SighashCache::new(&to_sign.unsigned_tx)
        .taproot_key_spend_signature_hash(0, &Prevouts::All(&[prevout]), TapSighashType::Default)
        .expect("sighash");
    let kp = Keypair::from_secret_key(&secp, &sk)
        .tap_tweak(&secp, None)
        .to_keypair();
    let sig = secp.sign_schnorr_no_aux_rand(
        &Message::from_digest_slice(sighash.as_ref()).expect("digest"),
        &kp,
    );
    let mut w64 = Witness::new();
    w64.push(sig.as_ref());
    push("p2tr 64-byte default", tr_s.clone(), smp(&w64), "Valid");
    let mut s65 = sig.as_ref().to_vec();
    s65.push(0x00);
    let mut w65 = Witness::new();
    w65.push(s65);
    push(
        "p2tr 65-byte with hash type 0x00 (L12-F4)",
        tr_s.clone(),
        smp(&w65),
        "Invalid",
    );
    // L13-F8: a 65-byte signature with explicit SIGHASH_ALL (0x01) is valid.
    let sighash_all = SighashCache::new(&to_sign.unsigned_tx)
        .taproot_key_spend_signature_hash(
            0,
            &Prevouts::All(&[TxOut {
                value: Amount::ZERO,
                script_pubkey: tr.script_pubkey(),
            }]),
            TapSighashType::All,
        )
        .expect("sighash");
    let sig_all = secp.sign_schnorr_no_aux_rand(
        &Message::from_digest_slice(sighash_all.as_ref()).expect("digest"),
        &kp,
    );
    let mut s65_all = sig_all.as_ref().to_vec();
    s65_all.push(0x01);
    let mut w65_all = Witness::new();
    w65_all.push(s65_all);
    push(
        "p2tr 65-byte with SIGHASH_ALL",
        tr_s.clone(),
        smp(&w65_all),
        "Valid",
    );

    // L12-F6: P2SH redeem script over 520 bytes (16-of-16, 547 bytes).
    let big_keys: Vec<SecretKey> = (0..16)
        .map(|i| key(&format!("onyx proof vector big {i}")))
        .collect();
    let big_pubs: Vec<bitcoin::PublicKey> = big_keys
        .iter()
        .map(|s| bitcoin::PublicKey::new(s.public_key(&secp)))
        .collect();
    let big = multisig(16, &big_pubs);
    let big_addr = Address::p2sh_from_hash(big.script_hash(), Network::Bitcoin);
    let big_privs: Vec<PrivateKey> = big_keys
        .iter()
        .map(|s| PrivateKey::new(*s, Network::Bitcoin))
        .collect();
    push(
        "p2sh redeem over 520 bytes (L12-F6)",
        big_addr.to_string(),
        ful(&bip322::sign_full(
            &big_addr,
            MSG,
            &big_privs,
            Some(&big),
            LockParams::default(),
        )
        .expect("big")),
        "Invalid",
    );

    // Other message, other address.
    v.push(V {
        note: "other message",
        body: wpkh.clone(),
        message: "Onyx proof vector v2",
        signature: simple(&wpkh),
        expect: "Invalid",
    });
    v.push(V {
        note: "proof for another address",
        body: tr_s.clone(),
        message: MSG,
        signature: simple(&wpkh),
        expect: "Invalid",
    });

    // Independent: BIP-322 basic vector (made by another implementation).
    v.push(V {
        note: "BIP-322 basic vector, Hello World",
        body: "bc1q9vza2e8x573nczrlzms0wvx3gsqjx7vavgkx0l".into(),
        message: "Hello World",
        signature: "smpAkcwRAIgZRfIY3p7/DoVTty6YZbWS71bc5Vct9p9Fia83eRmw2QCICK/ENGfwLtptFluMGs2KsqoNSk89pO7F29zJLUx9a/sASECx/EgAxlkQpQ9hYjgGu6EBCPMVPwVIVJqO4XCsMvViHI=".into(),
        expect: "Valid",
    });

    // Inconclusive: P2WSH whose script is not multisig (OP_1).
    let op1 = ScriptBuf::builder().push_int(1).into_script();
    let op1_addr = Address::p2wsh(&op1, Network::Bitcoin);
    let mut wi = Witness::new();
    wi.push([]);
    wi.push([1u8]);
    wi.push(op1.as_bytes());
    v.push(V {
        note: "p2wsh non-multisig script",
        body: op1_addr.to_string(),
        message: MSG,
        signature: smp(&wi),
        expect: "Inconclusive",
    });

    println!("//! Generated by `examples/gen-proof-vectors.rs`. Do not edit by hand.");
    println!();
    println!("use onyx_core::{{ProofOutcome, ProofVector}};");
    println!();
    println!("/// Registry vectors for `onyx.proof.btc-mainnet.bip322.v1`.");
    println!("pub(crate) const VECTORS: &[ProofVector] = &[");
    for x in &v {
        println!("    // {}", x.note);
        println!(
            "    ProofVector {{ body: {:?}, message: {:?}, signature: {:?}, expect: ProofOutcome::{} }},",
            x.body, x.message, x.signature, x.expect
        );
    }
    println!("];");
}
