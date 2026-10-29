//! Key files of the level-2 actors (ROADMAP B2, B3): an observer's own key
//! and the deployment's hot receipt key (THREATS REVLOG-4).
//!
//! A key file holds one 32-byte secp256k1 secret key as hex (`0x` optional,
//! surrounding whitespace ignored) and must not be readable by group or
//! others (mode `0600` or stricter on unix). Nothing here prints a key: errors
//! name the file, never its contents, and [`ReceiptKey`]'s `Debug` shows only
//! the address.

use crate::observe::test_keys;
use onyx_labels::attest::secret_address;

/// The 20-byte address of a 32-byte secret key (`None`: not a valid
/// secp256k1 key).
pub fn key_address(secret: &[u8; 32]) -> Option<[u8; 20]> {
    secret_address(secret)
}

/// Read the 32-byte secret key in `path` (see the module docs). Refused
/// when the file is readable by group or others, is not hex of 32 bytes, or
/// is not a valid secp256k1 key.
pub fn read_key_file(path: &std::path::Path) -> Result<[u8; 32], String> {
    use std::io::Read;
    // Review B2-R1-L6: opened once; the checks read the open file's own
    // metadata (no swap between check and read), and the read is bounded.
    let mut o = std::fs::OpenOptions::new();
    o.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.custom_flags(libc::O_NONBLOCK);
    }
    let f = o
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let meta = f
        .metadata()
        .map_err(|e| format!("{}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = meta.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            return Err(format!(
                "{}: a key file readable by group or others (mode {mode:o}) is refused; chmod 600",
                path.display()
            ));
        }
    }
    if !meta.is_file() || meta.len() > 256 {
        return Err(format!("{}: not a key file", path.display()));
    }
    let mut text = String::new();
    f.take(257)
        .read_to_string(&mut text)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let hex_part = text.trim().trim_start_matches("0x");
    let mut k = [0u8; 32];
    if hex_part.len() != 64 || hex::decode_to_slice(hex_part, &mut k).is_err() {
        return Err(format!("{}: not a 32-byte hex secret key", path.display()));
    }
    if secret_address(&k).is_none() {
        return Err(format!("{}: not a valid secp256k1 key", path.display()));
    }
    Ok(k)
}

/// The key that signs a deployment's reveal receipts (ADR-0027 amendment 8,
/// `OnyxRegistry.setReceiptSigner`; THREATS REVLOG-4, ROADMAP B3): a hot key
/// of its own, never the coordinator's transaction key, the reporter's
/// (`OnyxRevenue.reporter`), the deployer's, the registry owner's or an
/// observer's ([`ReceiptKey::check_distinct`]). Rotated by configuring a new
/// key: the next announced draw sets it with `setReceiptSigner`; draws
/// announced before keep the key they snapshotted (`drawReceiptSigner`), so
/// a round of such a draw is finished only with that key
/// ([`crate::observe::finish_reveals`] refuses another before any receipt).
#[derive(Clone)]
pub struct ReceiptKey {
    secret: [u8; 32],
    address: [u8; 20],
    public_test: bool,
}

impl std::fmt::Debug for ReceiptKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "ReceiptKey(0x{}{})",
            hex::encode(self.address),
            if self.public_test {
                ", public test key"
            } else {
                ""
            }
        )
    }
}

/// Chain id of a local anvil (as [`crate::cast_chain::ANVIL_CHAIN_ID`]).
const LOCAL_CHAIN: u64 = 31_337;

/// The addresses of anvil's (and hardhat's) default dev accounts 0..9,
/// derived from the published "test test ... junk" mnemonic: their keys are
/// as public as the test receipt key (review B2-R2-1).
const DEV_ACCOUNTS: [&str; 10] = [
    "f39fd6e51aad88f6f4ce6ab8827279cfffb92266",
    "70997970c51812dc3a010c7d01b50e0d17dc79c8",
    "3c44cdddb6a900fa2b585dd299e03d12fa4293bc",
    "90f79bf6eb2c4f870365e785982e1f101e93b906",
    "15d34aaf54267db7d7c367839aaf71a00a2c6a65",
    "9965507d1a55bcc2695c58ba16fb37d819b0a4dc",
    "976ea74026e726554db657fa54763abd0c3a0aa9",
    "14dc79964da2c08b23698b3d3cc7ca32193d9955",
    "23618e81e3f5cdf7f54c3d65f7fbc0abf5b21e8f",
    "a0ee7a142d267c1f36714e4a8f75612f20a79720",
];

/// Whether `address` is one of anvil's default dev accounts.
pub fn is_dev_account(address: &[u8; 20]) -> bool {
    DEV_ACCOUNTS.contains(&hex::encode(address).as_str())
}

impl ReceiptKey {
    /// The deployment's receipt key from its key file ([`read_key_file`]).
    /// The public test key ([`test_keys::receipt_secret`]) is refused here:
    /// anyone can sign with it.
    pub fn from_file(path: &std::path::Path) -> Result<Self, String> {
        let secret = read_key_file(path)?;
        if secret == test_keys::receipt_secret() {
            return Err(format!(
                "{}: holds the PUBLIC test receipt key; a deployment needs a key of its own (THREATS REVLOG-4)",
                path.display()
            ));
        }
        Self::from_secret(secret, false)
    }

    /// The public test receipt key of recorded rehearsals and tests: anyone
    /// can recompute it, so it proves nothing on a real deployment.
    pub fn public_test() -> Self {
        Self::from_secret(test_keys::receipt_secret(), true)
            .unwrap_or_else(|_| unreachable!("the test receipt key is a valid key"))
    }

    fn from_secret(secret: [u8; 32], public_test: bool) -> Result<Self, String> {
        let address = secret_address(&secret).ok_or("not a valid secp256k1 key")?;
        Ok(ReceiptKey {
            secret,
            address,
            public_test,
        })
    }

    /// Which receipt key a runner uses (ROADMAP B3): the key file when one is
    /// given; without one, the public test key only for a replay (`chain_id`
    /// `None`) or a local anvil (31337); any other chain (a fork keeps the
    /// forked chain's id, a testnet) is refused without a key file.
    pub fn for_chain(
        chain_id: Option<u64>,
        path: Option<&std::path::Path>,
    ) -> Result<Self, String> {
        match (path, chain_id) {
            (Some(p), id) => {
                let k = Self::from_file(p)?;
                // Review B2-R2-1: an anvil dev key is public too; only on a
                // replay or a local anvil.
                if k.is_well_known() && !matches!(id, None | Some(LOCAL_CHAIN)) {
                    return Err(format!(
                        "{}: holds a PUBLIC anvil dev key (0x{}), which anyone can sign with: chain {} needs a receipt key of its own (THREATS REVLOG-4)",
                        p.display(),
                        hex::encode(k.address),
                        id.unwrap_or_default()
                    ));
                }
                Ok(k)
            }
            (None, None | Some(LOCAL_CHAIN)) => Ok(Self::public_test()),
            (None, Some(id)) => Err(format!(
                "chain {id} is not a local anvil: give the deployment's own receipt key with --receipt-key FILE (THREATS REVLOG-4, ROADMAP B3); the public test key is for local rehearsals only"
            )),
        }
    }

    /// The signer's address (what `setReceiptSigner` names).
    pub fn address(&self) -> [u8; 20] {
        self.address
    }

    /// The secret key, to sign receipts with.
    pub fn secret(&self) -> &[u8; 32] {
        &self.secret
    }

    /// Whether this is the public test key.
    pub fn is_public_test(&self) -> bool {
        self.public_test
    }

    /// Whether anyone can sign with this key: the public test key or an
    /// anvil dev account (review B2-R2-1). Such a key runs only a replay or
    /// a local anvil.
    pub fn is_well_known(&self) -> bool {
        self.public_test || is_dev_account(&self.address)
    }

    /// Refuse to start when the receipt key is one of the deployment's other
    /// keys (THREATS REVLOG-4, ROADMAP B3): `others` names each (`"the
    /// reporter"`, `"the coordinator"`, an observer). A receipt key shared
    /// with the reporter or the coordinator's transaction key makes one leak
    /// cost both; shared with an observer, that observer receipts its own
    /// reveals.
    pub fn check_distinct(&self, others: &[(&str, [u8; 20])]) -> Result<(), String> {
        match others.iter().find(|(_, a)| *a == self.address) {
            Some((who, a)) => Err(format!(
                "the receipt key 0x{} is also {who} (0x{}): refused; the receipt signer is a hot key of its own (THREATS REVLOG-4, ROADMAP B3)",
                hex::encode(self.address),
                hex::encode(a)
            )),
            None => Ok(()),
        }
    }
}

/// The coordinator's reveal key (ADR-0027 amendment 10 item 3, ROADMAP C11):
/// the secret observers' sealed reveal posts are encrypted to.
/// `setRevealKey` names its public point and each draw snapshots that point
/// (`drawRevealKey`), so a rotation never changes the key of a draw already
/// announced. A hot key of its own ([`RevealKey::check_distinct`]): never the
/// receipt key (THREATS REVLOG-4), the coordinator's transaction key, the
/// reporter's, the registry owner's or an observer's. Whoever holds it reads
/// every value ever posted to it (THREATS C4-9), so a key shared with another
/// role makes one leak cost both; shared with an observer, that observer
/// reads the others' posts before it posts its own. There is no public test
/// reveal key: without a key file a coordinator announces draws with no
/// reveal key, and their observers reveal through the spool (REVLOG-1 open
/// for them). Nothing prints the secret: `Debug` shows the address of the
/// point only.
#[derive(Clone)]
pub struct RevealKey {
    secret: [u8; 32],
    point: crate::sealed::Point,
    address: [u8; 20],
}

impl std::fmt::Debug for RevealKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "RevealKey(0x{})", hex::encode(self.address))
    }
}

impl RevealKey {
    /// The reveal key in `path` ([`read_key_file`]) for the chain `chain_id`
    /// (`None`: a replay). A key anyone can recompute (the public test receipt
    /// key, an anvil dev key) is refused except on a replay or a local anvil:
    /// with it anyone would read every sealed post.
    pub fn for_chain(chain_id: Option<u64>, path: &std::path::Path) -> Result<Self, String> {
        let secret = read_key_file(path)?;
        let k = Self::from_secret(secret)?;
        if k.is_well_known() && !matches!(chain_id, None | Some(LOCAL_CHAIN)) {
            return Err(format!(
                "{}: holds a PUBLIC key (0x{}), with which anyone reads every sealed post: chain {} needs a reveal key of its own (THREATS C4-9)",
                path.display(),
                hex::encode(k.address),
                chain_id.unwrap_or_default()
            ));
        }
        Ok(k)
    }

    /// A reveal key from its secret (tests).
    pub fn from_secret(secret: [u8; 32]) -> Result<Self, String> {
        let address = secret_address(&secret).ok_or("not a valid secp256k1 key")?;
        let point = crate::sealed::public_point(&secret).map_err(|e| e.to_string())?;
        Ok(RevealKey {
            secret,
            point,
            address,
        })
    }

    /// The public point `setRevealKey` names (`x ‖ y`).
    pub fn point(&self) -> crate::sealed::Point {
        self.point
    }

    /// The key's address (what the distinctness checks compare).
    pub fn address(&self) -> [u8; 20] {
        self.address
    }

    /// The secret, to open sealed posts with.
    pub fn secret(&self) -> &[u8; 32] {
        &self.secret
    }

    /// Whether anyone can recompute this key: the public test receipt key or
    /// an anvil dev account.
    pub fn is_well_known(&self) -> bool {
        self.secret == test_keys::receipt_secret() || is_dev_account(&self.address)
    }

    /// Refuse to start when the reveal key is one of the deployment's other
    /// keys (ROADMAP C11, the B3 checks): `others` names each (`"the receipt
    /// key"`, `"the coordinator"`, `"the reporter"`, an observer).
    pub fn check_distinct(&self, others: &[(&str, [u8; 20])]) -> Result<(), String> {
        match others.iter().find(|(_, a)| *a == self.address) {
            Some((who, a)) => Err(format!(
                "the reveal key 0x{} is also {who} (0x{}): refused; the reveal key is a hot key of its own (THREATS C4-9, ROADMAP C11)",
                hex::encode(self.address),
                hex::encode(a)
            )),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_key(dir: &std::path::Path, name: &str, text: &str, mode: u32) -> std::path::PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, text).expect("write");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode)).expect("chmod");
        }
        #[cfg(not(unix))]
        let _ = mode;
        p
    }

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("onyx-keyfile-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&d).expect("dir");
        d
    }

    /// ROADMAP B3: the receipt key is refused when it is the reporter, the
    /// coordinator or an observer key; a distinct one passes.
    #[test]
    fn b3_a_receipt_key_equal_to_another_key_is_refused() {
        let d = tmp("distinct");
        let sk = test_keys::secret("b3 receipt", 1);
        let k =
            ReceiptKey::from_file(&write_key(&d, "r.key", &hex::encode(sk), 0o600)).expect("key");
        let me = k.address();
        let other = test_keys::address("b3 other", 1);
        assert!(k.check_distinct(&[("the reporter", other)]).is_ok());
        for who in ["the reporter", "the coordinator", "observer 3"] {
            let e = k
                .check_distinct(&[("someone else", other), (who, me)])
                .expect_err("same key");
            assert!(e.contains(who) && e.contains("REVLOG-4"), "{e}");
        }
        // The Debug form never shows the secret.
        let shown = format!("{k:?}");
        assert!(!shown.contains(&hex::encode(sk)) && shown.contains(&hex::encode(me)));
    }

    /// ROADMAP B3: the public test key runs only a replay or a local anvil;
    /// a fork (it keeps the forked chain's id) or a testnet needs a key
    /// file, and a key file holding the public test key is refused.
    #[test]
    fn b3_the_public_test_key_is_for_local_rehearsals_only() {
        assert!(ReceiptKey::for_chain(None, None)
            .expect("replay")
            .is_public_test());
        assert!(ReceiptKey::for_chain(Some(31_337), None)
            .expect("anvil")
            .is_public_test());
        let e = ReceiptKey::for_chain(Some(84_532), None).expect_err("testnet");
        assert!(e.contains("--receipt-key"), "{e}");
        let d = tmp("public");
        let p = write_key(
            &d,
            "t.key",
            &hex::encode(test_keys::receipt_secret()),
            0o600,
        );
        assert!(ReceiptKey::for_chain(Some(31_337), Some(&p)).is_err());
        let own = write_key(
            &d,
            "own.key",
            &format!("0x{}\n", hex::encode(test_keys::secret("b3 own", 0))),
            0o600,
        );
        let k = ReceiptKey::for_chain(Some(84_532), Some(&own)).expect("own key");
        assert!(!k.is_public_test() && !k.is_well_known());
        assert_eq!(k.address(), test_keys::address("b3 own", 0));
    }

    /// Review B2-R2-1: an anvil dev key (its secret is published with the
    /// "test test ... junk" mnemonic) in a key file runs only a replay or a
    /// local anvil, like the public test key; never a fork or a testnet.
    #[test]
    fn b2_r2_an_anvil_dev_key_is_for_local_rehearsals_only() {
        let d = tmp("devkey");
        // anvil's default account 0 (public; allowlisted by check-secrets).
        let p = write_key(
            &d,
            "dev0.key",
            "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80",
            0o600,
        );
        for chain in [8_453, 84_532] {
            let e = ReceiptKey::for_chain(Some(chain), Some(&p)).expect_err("public dev key");
            assert!(
                e.contains("anvil dev key") && !e.contains("ac0974be"),
                "{e}"
            );
        }
        let k = ReceiptKey::for_chain(Some(31_337), Some(&p)).expect("local anvil");
        assert!(k.is_well_known() && !k.is_public_test());
        assert!(ReceiptKey::for_chain(None, Some(&p)).is_ok(), "replay");
        assert!(ReceiptKey::public_test().is_well_known());
        let mut a = [0u8; 20];
        hex::decode_to_slice("a0Ee7A142d267C1f36714E4a8F75612F20a79720", &mut a).expect("hex");
        assert!(is_dev_account(&a), "account 9, checksummed input");
        assert!(!is_dev_account(&test_keys::address("b3 own", 0)));
    }

    /// ROADMAP C11: the reveal key is a key of its own: refused when it is
    /// the receipt key, the coordinator, the reporter, the registry owner or an
    /// observer; a public key (the test receipt key, an anvil dev key) only on
    /// a replay or a local anvil; and nothing prints its secret.
    #[test]
    fn c11_the_reveal_key_is_a_key_of_its_own() {
        let d = tmp("reveal");
        let sk = test_keys::secret("c11 reveal", 1);
        let p = write_key(&d, "reveal.key", &hex::encode(sk), 0o600);
        let k = RevealKey::for_chain(Some(84_532), &p).expect("own key");
        assert_eq!(k.point(), crate::sealed::public_point(&sk).expect("point"));
        let me = k.address();
        let other = test_keys::address("c11 other", 1);
        assert!(k.check_distinct(&[("the receipt key", other)]).is_ok());
        for who in [
            "the receipt key",
            "the coordinator",
            "the reporter",
            "the registry owner",
            "an observer",
        ] {
            let e = k
                .check_distinct(&[("someone else", other), (who, me)])
                .expect_err("same key");
            assert!(e.contains(who) && e.contains("C11"), "{e}");
        }
        let shown = format!("{k:?}");
        assert!(!shown.contains(&hex::encode(sk)) && shown.contains(&hex::encode(me)));
        // Public keys: local only.
        for (name, text) in [
            ("test.key", hex::encode(test_keys::receipt_secret())),
            (
                "dev0.key",
                "ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80".to_string(),
            ),
        ] {
            let p = write_key(&d, name, &text, 0o600);
            let e = RevealKey::for_chain(Some(84_532), &p).expect_err("public");
            assert!(e.contains("PUBLIC") && !e.contains(&text), "{e}");
            assert!(
                RevealKey::for_chain(Some(31_337), &p).is_ok(),
                "local anvil"
            );
            assert!(RevealKey::for_chain(None, &p).is_ok(), "replay");
        }
        // A loose file is refused as any key file.
        #[cfg(unix)]
        {
            let loose = write_key(&d, "loose.key", &hex::encode(sk), 0o640);
            assert!(RevealKey::for_chain(Some(84_532), &loose).is_err());
        }
    }

    /// A key file readable by others, or not a 32-byte key, is refused,
    /// and the error never shows the file's contents.
    #[test]
    fn b3_a_loose_or_malformed_key_file_is_refused() {
        let d = tmp("loose");
        let sk = hex::encode(test_keys::secret("b3 loose", 0));
        #[cfg(unix)]
        {
            let e = read_key_file(&write_key(&d, "open.key", &sk, 0o644)).expect_err("0644");
            assert!(e.contains("chmod 600") && !e.contains(&sk), "{e}");
        }
        for (name, text) in [
            ("short.key", "0x1234"),
            ("nothex.key", &"zz".repeat(32) as &str),
            ("zero.key", &"00".repeat(32) as &str),
        ] {
            let e = read_key_file(&write_key(&d, name, text, 0o600)).expect_err(name);
            assert!(!e.contains(text), "{e}");
        }
        assert!(read_key_file(&d.join("missing.key")).is_err());
    }
}
