//! Common asset names for the bulk dataset search (`GET /v1/bulk?q=`).
//!
//! A bulk dataset id is `<asset>.<metric words>` (`btc.mvrv`). A reader
//! types the asset's name ("bitcoin price"), so a search word that is a
//! known name also finds the datasets of that asset's id, and the datasets
//! of the asset asked for are listed before those that only hold its id as
//! a part of another word (`btc.price` before `wbtc.price`: Wrapped Bitcoin
//! is another asset).
//!
//! The table is the website's (`ASSETS` in apps/web/src/lib/glossary.ts);
//! the test below reads that file and fails when the two lists differ. The
//! agent toolkit ships the same list in apps/agent/catalog.json, written by
//! its sync script.
//!
//! Cost: a search is at most [`super::MAX_SEARCH_CHARS`] characters, so at
//! most 32 words; each word start is looked up at most [`MAX_NAME_WORDS`]
//! times in a map built once. No pattern is built from what a reader sent.

use super::{id_words, AnyTerm};
use std::collections::BTreeMap;
use std::sync::LazyLock;

/// Asset id and name, as in the website's table and in its order.
pub const ASSETS: &[(&str, &str)] = &[
    ("1inch", "1inch"),
    ("aave", "Aave"),
    ("ada", "Cardano"),
    ("ape", "ApeCoin"),
    ("axs", "Axie Infinity"),
    ("bat", "Basic Attention Token"),
    ("bch", "Bitcoin Cash"),
    ("bnb", "BNB"),
    ("bonk", "Bonk"),
    ("btc", "Bitcoin"),
    ("btt-trx", "BitTorrent on Tron"),
    ("chz", "Chiliz"),
    ("comp", "Compound"),
    ("crv", "Curve"),
    ("dai", "Dai"),
    ("doge", "Dogecoin"),
    ("ens", "Ethereum Name Service"),
    ("eth", "Ethereum"),
    ("fet", "Fetch.ai"),
    ("gala", "Gala"),
    ("grt", "The Graph"),
    ("hype", "Hyperliquid"),
    ("imx", "Immutable"),
    ("jst-trx", "JUST on Tron"),
    ("jto", "Jito"),
    ("jup", "Jupiter"),
    ("ldo", "Lido DAO"),
    ("leo", "LEO"),
    ("link", "Chainlink"),
    ("lrc", "Loopring"),
    ("ltc", "Litecoin"),
    ("mana", "Decentraland"),
    ("mkr", "Maker"),
    ("mnt", "Mantle"),
    ("ondo", "Ondo"),
    ("paxg", "PAX Gold"),
    ("pengu", "Pudgy Penguins"),
    ("pepe", "Pepe"),
    ("pump", "Pump.fun"),
    ("pyth", "Pyth Network"),
    ("pyusd", "PayPal USD"),
    ("ray", "Raydium"),
    ("render", "Render"),
    ("sand", "The Sandbox"),
    ("shib", "Shiba Inu"),
    ("snx", "Synthetix"),
    ("sol", "Solana"),
    ("sun-trx", "SUN on Tron"),
    ("trump", "Official Trump"),
    ("trx", "Tron"),
    ("tusd-trx", "TrueUSD on Tron"),
    ("uni", "Uniswap"),
    ("usd1", "USD1"),
    ("usdc", "USD Coin"),
    ("usdc-sol", "USD Coin on Solana"),
    ("usdc-trx", "USD Coin on Tron"),
    ("usdd-trx", "USDD on Tron"),
    ("usde", "Ethena USDe"),
    ("usdg", "Global Dollar"),
    ("usds", "USDS"),
    ("usdt", "Tether"),
    ("usdt-sol", "Tether on Solana"),
    ("usdt-trx", "Tether on Tron"),
    ("usdy", "Ondo US Dollar Yield"),
    ("usyc", "USYC"),
    ("wbtc", "Wrapped Bitcoin"),
    ("weth", "Wrapped Ether"),
    ("wif", "dogwifhat"),
    ("win-trx", "WINkLink on Tron"),
    ("wld", "Worldcoin"),
    ("wlfi", "World Liberty Financial"),
    ("xaut", "Tether Gold"),
    ("xrp", "XRP"),
];

/// Names in common use that are not the table's own (`ether` for Ethereum).
/// The agent toolkit has the same three (`EXTRA_NAMES` in
/// apps/agent/src/connect.mjs; the test below compares them).
pub const EXTRA_NAMES: &[(&str, &str)] = &[("ether", "eth"), ("ripple", "xrp"), ("binance", "bnb")];

/// Words of the longest name ("Ondo US Dollar Yield").
pub const MAX_NAME_WORDS: usize = 4;

/// Lowercase name (words split by one space) -> asset id. A name that is
/// its own id (`bnb`) needs no entry: the id is searched as typed.
static BY_NAME: LazyLock<BTreeMap<String, &'static str>> = LazyLock::new(|| {
    let mut m = BTreeMap::new();
    for (id, name) in ASSETS {
        let name = name.to_ascii_lowercase();
        if name != *id {
            m.insert(name, *id);
        }
    }
    for (name, id) in EXTRA_NAMES {
        m.insert((*name).to_string(), *id);
    }
    m
});

/// The asset whose name is exactly `name` (lowercase), if any.
pub fn asset_of(name: &str) -> Option<&'static str> {
    BY_NAME.get(name).copied()
}

/// A name found in a search: the words typed and the asset they name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Named {
    /// The lowercase words typed (`["bitcoin"]`, `["usd", "coin"]`).
    pub typed: Vec<String>,
    /// The asset id (`btc`).
    pub asset: &'static str,
    /// The id as whole words of a dataset id (`usdt-trx`: usdt then trx).
    term: AnyTerm,
}

impl Named {
    fn new(typed: Vec<String>, asset: &'static str) -> Self {
        let term = AnyTerm {
            words: asset.split('-').map(str::to_string).collect(),
            prefix: false,
        };
        Named { typed, asset, term }
    }

    /// Whether `id` (`publisher/dataset`, its words in `words`) is found by
    /// this name: it holds every typed word (as any search word), or the
    /// asset id as whole words (`btc`, never the btc in `wbtc`).
    pub fn matches(&self, id: &str, words: &[&str]) -> bool {
        self.typed.iter().all(|w| id.contains(w.as_str())) || self.term.matches(words)
    }
}

/// Split lowercase search words into the asset names among them and the
/// other words, in their order. The longest name at a place wins ("bitcoin
/// cash" is Bitcoin Cash, not Bitcoin and the word cash).
pub fn split_names(words: Vec<String>) -> (Vec<Named>, Vec<String>) {
    let (mut names, mut rest) = (Vec::new(), Vec::new());
    let mut i = 0;
    while i < words.len() {
        let hit = (1..=MAX_NAME_WORDS.min(words.len() - i))
            .rev()
            .find_map(|n| asset_of(&words[i..i + n].join(" ")).map(|a| (n, a)));
        match hit {
            Some((n, asset)) => {
                names.push(Named::new(words[i..i + n].to_vec(), asset));
                i += n;
            }
            None => {
                rest.push(words[i].clone());
                i += 1;
            }
        }
    }
    (names, rest)
}

/// Where a dataset stands in a search, smallest first: `(0, _)` its asset
/// (the part of the dataset id before the first dot) is an asset asked for,
/// by name or as a typed word, or the whole id was typed; `(1, _)` it only
/// holds the words. Within each, `false` when the rest of the id (the
/// metric) is exactly the other words typed (`btc.price` for "bitcoin
/// price", before `btc.price-drawdown`).
pub fn rank(names: &[Named], words: &[String], publisher: &str, dataset: &str) -> (u8, bool) {
    if words.iter().any(|w| {
        w == dataset
            || w.strip_prefix(publisher)
                .and_then(|r| r.strip_prefix('/'))
                .is_some_and(|d| d == dataset)
    }) {
        return (0, false);
    }
    let (asset, metric) = dataset.split_once('.').unwrap_or((dataset, ""));
    let own = names.iter().any(|n| n.asset == asset) || words.iter().any(|w| w == asset);
    let mut typed = words
        .iter()
        .filter(|w| *w != asset)
        .flat_map(|w| id_words(w));
    let mut metric_words = id_words(metric).into_iter();
    let mut any = false;
    let exact = loop {
        match (typed.next(), metric_words.next()) {
            (None, None) => break any,
            (Some(a), Some(b)) if a == b => any = true,
            _ => break false,
        }
    };
    (u8::from(!own), !exact)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lower(words: &str) -> Vec<String> {
        words.split(' ').map(str::to_string).collect()
    }

    /// The node's table is the website's: same ids, same names, same order.
    /// The website file is the source; copy a change from it to here.
    #[test]
    fn the_asset_table_is_the_websites() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../apps/web/src/lib/glossary.ts"
        );
        let text = std::fs::read_to_string(path).expect("the website's glossary.ts");
        let body = text
            .split_once("export const ASSETS: Record<string, Asset> = {")
            .expect("the ASSETS table")
            .1
            .split_once("\n};")
            .expect("its end")
            .0;
        let mut web = Vec::new();
        for line in body.lines().filter(|l| !l.trim().is_empty()) {
            let (id, more) = line.trim().split_once(": { name: '").expect(line);
            let name = more.split_once('\'').expect(line).0;
            web.push((id.trim_matches('\'').to_string(), name.to_string()));
        }
        let here: Vec<(String, String)> = ASSETS
            .iter()
            .map(|(i, n)| ((*i).to_string(), (*n).to_string()))
            .collect();
        assert!(web.len() > 50, "the table was read");
        assert_eq!(
            here, web,
            "crates/onyx-api/src/bulk_names.rs ASSETS differs from apps/web/src/lib/glossary.ts"
        );
        // Every name fits a search: its characters, and the words looked up.
        for (_, name) in ASSETS {
            assert!(name.split(' ').count() <= MAX_NAME_WORDS, "{name}");
            assert!(
                name.chars()
                    .all(|c| c.is_ascii_alphanumeric() || " .-_/".contains(c)),
                "{name}"
            );
        }
    }

    /// The names that are not the website's are the agent toolkit's too.
    #[test]
    fn the_extra_names_are_the_agent_toolkits() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../apps/agent/src/connect.mjs"
        );
        let text = std::fs::read_to_string(path).expect("connect.mjs");
        let line = text
            .lines()
            .find(|l| l.starts_with("export const EXTRA_NAMES = "))
            .expect("EXTRA_NAMES");
        let want = EXTRA_NAMES
            .iter()
            .map(|(n, i)| format!("{n}: '{i}'"))
            .collect::<Vec<_>>()
            .join(", ");
        assert_eq!(line, format!("export const EXTRA_NAMES = {{ {want} }};"));
        for (name, id) in EXTRA_NAMES {
            assert!(ASSETS.iter().any(|(i, _)| i == id), "{name}: {id}");
        }
    }

    #[test]
    fn common_names_are_asset_ids() {
        for (name, id) in [
            ("bitcoin", "btc"),
            ("ethereum", "eth"),
            ("ether", "eth"),
            ("tron", "trx"),
            ("solana", "sol"),
            ("cardano", "ada"),
            ("tether", "usdt"),
            ("ripple", "xrp"),
            ("bitcoin cash", "bch"),
            ("wrapped bitcoin", "wbtc"),
            ("usd coin on solana", "usdc-sol"),
        ] {
            assert_eq!(asset_of(name), Some(id), "{name}");
        }
        // An id is no name, and neither is a part of one.
        for no in ["btc", "bnb", "bit", "coin", "wrapped", ""] {
            assert_eq!(asset_of(no), None, "{no}");
        }
        let (names, rest) = split_names(lower("tron staking flows"));
        assert_eq!(names.len(), 1);
        assert_eq!(
            (names[0].typed.clone(), names[0].asset),
            (lower("tron"), "trx")
        );
        assert_eq!(rest, lower("staking flows"));
        // The longest name wins, and a name may come anywhere.
        let (names, rest) = split_names(lower("price bitcoin cash ethereum btc"));
        let found: Vec<&str> = names.iter().map(|n| n.asset).collect();
        assert_eq!(found, ["bch", "eth"]);
        assert_eq!(rest, lower("price btc"));
        assert_eq!(split_names(Vec::new()), (Vec::new(), Vec::new()));
    }

    #[test]
    fn a_name_finds_its_asset_as_a_whole_word_only() {
        let (names, _) = split_names(lower("bitcoin"));
        let hit = |id: &str| names[0].matches(id, &id_words(id));
        assert!(hit("example.publisher/btc.mvrv"));
        assert!(hit("example.publisher/eth.btc-ratio"));
        assert!(hit("example.publisher/x.bitcoin-days"), "the word as typed");
        assert!(!hit("example.publisher/wbtc.mvrv"), "wbtc is another asset");
        let (names, _) = split_names(lower("tether on tron"));
        let hit = |id: &str| names[0].matches(id, &id_words(id));
        assert!(hit("p/usdt-trx.supply"));
        assert!(!hit("p/usdt.supply"));
        assert!(!hit("p/trx.usdt-volume"), "the id's words in their order");
    }

    #[test]
    fn the_asset_asked_for_ranks_first_then_the_exact_metric() {
        let r = |q: &str, d: &str| {
            let (names, rest) = split_names(lower(q));
            rank(&names, &rest, "example.publisher", d)
        };
        // By name: btc before wbtc, the exact metric before a longer one.
        assert_eq!(r("bitcoin price", "btc.price"), (0, false));
        assert_eq!(r("bitcoin price", "btc.price-drawdown"), (0, true));
        assert_eq!(r("bitcoin price", "eth.btc-price"), (1, true));
        // As a typed id: the same, though wbtc.price holds both words (to
        // another asset's dataset the typed id is one more word).
        assert_eq!(r("btc price", "btc.price"), (0, false));
        assert_eq!(r("btc price", "wbtc.price"), (1, true));
        assert!(r("btc price", "btc.realized-price") < r("btc price", "wbtc.price"));
        // The metric's words in the order typed, however they are joined.
        assert_eq!(r("btc mvrv z-score", "btc.mvrv-z-score"), (0, false));
        assert_eq!(r("btc score mvrv", "btc.mvrv-z-score"), (0, true));
        // No asset typed: only the metric counts.
        assert_eq!(r("mvrv", "btc.mvrv"), (1, false));
        assert_eq!(r("mvrv", "btc.mvrv-z-score"), (1, true));
        assert_eq!(r("btc", "btc.mvrv"), (0, true), "no metric word typed");
        // The whole id typed is that dataset first.
        assert_eq!(r("btc.mvrv", "btc.mvrv"), (0, false));
        assert_eq!(r("example.publisher/btc.mvrv", "btc.mvrv"), (0, false));
        assert_eq!(r("btc.mvrv", "btc.mvrv-z-score"), (1, true));
        // A dataset id without a dot is its own asset.
        assert_eq!(r("heights", "heights"), (0, false));
    }
}
