//! Opening the revenue book of a level-2 fact before it is sold (THREATS
//! L2API-4, ROADMAP B1). `OnyxRevenue.settle` reverts `NotRegistered` for a
//! book that is not open, so in self-settle mode a paid level-2 request
//! before `registerObservation(fact)` (R-REV-12) answers 503. The deploy run
//! opens every served fact's book from the reporter key BEFORE level 2 is
//! served, through [`open_observation_books`]:
//!
//! - idempotent: the book is read first (`books(OBSERVATION_ROUND, fact)`)
//!   and opened only when closed, so a rerun sends nothing and never
//!   double-opens (the contract would revert `AlreadyRegistered` anyway);
//!   a send that reverts or would revert is success when the book now reads
//!   open (another opener got there first: the node's why-4 registrar, an
//!   earlier run whose answer was lost);
//! - fail closed: a key that is not the reporter is refused before anything
//!   is sent (`NotReporter`), and any outcome that leaves the book closed or
//!   unknown (a revert, an unmined send, a node that cannot be read) is an
//!   error naming the fact; the caller must not serve level 2 then.

use crate::evm_tx::{SendError, Sender};
use crate::observations::{register_observation_calldata, OBSERVATION_ROUND};
use crate::roots::JsonRpc;
use onyx_core::Hash32;
use std::time::Duration;

fn selector(sig: &str) -> [u8; 4] {
    let mut s = [0u8; 4];
    s.copy_from_slice(&onyx_core::keccak256(sig.as_bytes()).0[..4]);
    s
}

fn hex0x(b: &[u8]) -> String {
    format!("0x{}", hex::encode(b))
}

/// Calldata of `OnyxRevenue.books(round, key)`.
pub fn book_calldata(round: u64, key: &Hash32) -> Vec<u8> {
    let mut d = selector("books(uint64,bytes32)").to_vec();
    d.extend_from_slice(&[0u8; 24]);
    d.extend_from_slice(&round.to_be_bytes());
    d.extend_from_slice(&key.0);
    d
}

/// Calldata of `OnyxRevenue.reporter()`.
pub fn reporter_calldata() -> Vec<u8> {
    selector("reporter()").to_vec()
}

/// The `registered` flag of a `books(..)` return (its first word).
pub fn registered_from_return(ret: &[u8]) -> Result<bool, String> {
    let w = ret
        .get(..32)
        .ok_or_else(|| format!("books(): {} bytes, want at least 32", ret.len()))?;
    match (w[..31].iter().all(|b| *b == 0), w[31]) {
        (true, 0) => Ok(false),
        (true, 1) => Ok(true),
        _ => Err(format!("books(): registered is not a bool: {}", hex0x(w))),
    }
}

/// The address in a `reporter()` return.
fn address_from_return(ret: &[u8]) -> Result<[u8; 20], String> {
    let w = ret
        .get(..32)
        .filter(|w| w[..12].iter().all(|b| *b == 0))
        .ok_or_else(|| format!("reporter(): not an address word: {}", hex0x(ret)))?;
    let mut a = [0u8; 20];
    a.copy_from_slice(&w[12..]);
    Ok(a)
}

/// What [`open_observation_book`] found or did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Opened {
    /// The book reads open (at the node's latest block) and this call did
    /// not see its own send mined: nothing was sent, this run's send
    /// reverted because another opener got there first, or this run's send
    /// landed after its deadline (review B5/B1-I2: it may have been ours).
    AlreadyOpen,
    /// This call opened it.
    Now {
        /// The `registerObservation` transaction.
        tx: String,
        /// Its block.
        block: u64,
    },
}

/// Why a fact's book is not known to be open. Every case means: do not
/// serve level 2 for the fact (its paid requests would answer 503).
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum OpenError {
    /// The key is not `OnyxRevenue.reporter()`: nothing was sent.
    #[error("the operator key {key} is not OnyxRevenue's reporter {reporter}: registerObservation would revert NotReporter; nothing sent (the owner sets the reporter with setReporter)")]
    NotReporter {
        /// The key's address.
        key: String,
        /// The reporter on chain.
        reporter: String,
    },
    /// The send was not mined before the deadline: the book may or may not
    /// be open. Running again is safe (the book is read first).
    #[error("registerObservation({fact}) {tx} was not mined before the deadline: the book may still be closed; run again (opening reads the book first, so it never opens twice)")]
    Pending {
        /// The fact.
        fact: String,
        /// The transaction.
        tx: String,
    },
    /// The book reads closed after the attempt.
    #[error("registerObservation({fact}) did not open the book: {why}")]
    Failed {
        /// The fact.
        fact: String,
        /// What happened.
        why: String,
    },
    /// The node could not be read: nothing is known.
    #[error("reading OnyxRevenue {revenue}: {why}")]
    Read {
        /// The contract.
        revenue: String,
        /// The node's error.
        why: String,
    },
}

/// Whether the revenue book `(OBSERVATION_ROUND, fact)` is open, at the
/// node's latest block.
pub async fn observation_book_open<T: JsonRpc>(
    sender: &Sender<T>,
    revenue: &[u8; 20],
    fact: &Hash32,
) -> Result<bool, OpenError> {
    let read = |why: String| OpenError::Read {
        revenue: hex0x(revenue),
        why,
    };
    let ret = sender
        .call(revenue, &book_calldata(OBSERVATION_ROUND, fact))
        .await
        .map_err(read)?;
    registered_from_return(&ret).map_err(read)
}

/// Open the revenue book of the level-2 fact `fact` (its registry fact key)
/// from `sender`, OnyxRevenue's reporter, unless it is open; see the module
/// docs. `deadline` bounds the send.
pub async fn open_observation_book<T: JsonRpc>(
    sender: &Sender<T>,
    revenue: &[u8; 20],
    fact: &Hash32,
    deadline: Duration,
) -> Result<Opened, OpenError> {
    let f = hex0x(&fact.0);
    if *fact == Hash32::ZERO {
        return Err(OpenError::Failed {
            fact: f,
            why: "the zero fact has no book (the contract reverts BadFields)".into(),
        });
    }
    if observation_book_open(sender, revenue, fact).await? {
        return Ok(Opened::AlreadyOpen);
    }
    let reporter = sender
        .call(revenue, &reporter_calldata())
        .await
        .and_then(|r| address_from_return(&r))
        .map_err(|why| OpenError::Read {
            revenue: hex0x(revenue),
            why,
        })?;
    if reporter != sender.address() {
        return Err(OpenError::NotReporter {
            key: hex0x(&sender.address()),
            reporter: hex0x(&reporter),
        });
    }
    let sent = sender
        .send(revenue, &register_observation_calldata(fact), 0, deadline)
        .await;
    let open = observation_book_open(sender, revenue, fact).await;
    match (sent, open) {
        (Ok(m), Ok(true)) => Ok(Opened::Now {
            tx: m.hash,
            block: m.block,
        }),
        // Another opener first: this send would revert or reverted
        // (`AlreadyRegistered`); the book is open, which is all that counts.
        (Err(SendError::WouldRevert(_) | SendError::Reverted(_)), Ok(true)) => {
            Ok(Opened::AlreadyOpen)
        }
        (Err(SendError::Pending(tx)), Ok(false) | Err(_)) => {
            Err(OpenError::Pending { fact: f, tx })
        }
        // An unmined send whose book reads open: someone's opening landed.
        (Err(SendError::Pending(_)), Ok(true)) => Ok(Opened::AlreadyOpen),
        (Ok(m), Ok(false)) => Err(OpenError::Failed {
            fact: f,
            why: format!(
                "{} mined in block {} but the book still reads closed (a lagging node?); run again",
                m.hash, m.block
            ),
        }),
        (Err(e), Ok(true)) => Err(OpenError::Failed {
            fact: f,
            why: format!("{e}, though the book reads open: run again"),
        }),
        (Err(e), Ok(false)) => Err(OpenError::Failed {
            fact: f,
            why: e.to_string(),
        }),
        (_, Err(e)) => Err(e),
    }
}

/// [`open_observation_book`] for every fact in `facts` (sorted, each once),
/// stopping at the first that is not known to be open: level 2 is served
/// only when every served fact's book is open.
pub async fn open_observation_books<T: JsonRpc>(
    sender: &Sender<T>,
    revenue: &[u8; 20],
    facts: &[Hash32],
    deadline: Duration,
) -> Result<Vec<(Hash32, Opened)>, OpenError> {
    let mut facts = facts.to_vec();
    facts.sort_by_key(|f| f.0);
    facts.dedup();
    let mut out = Vec::with_capacity(facts.len());
    for f in facts {
        let o = open_observation_book(sender, revenue, &f, deadline).await?;
        out.push((f, o));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::x402::BoxFuture;
    use serde_json::{json, Value};
    use std::collections::BTreeSet;
    use std::sync::{Arc, Mutex};

    /// Public test key; it holds nothing.
    const OPERATOR: [u8; 32] = [0x11; 32];
    const REVENUE: [u8; 20] = [0x5e; 20];
    const FACT: Hash32 = Hash32([1; 32]);

    /// `cast calldata "books(uint64,bytes32)" 18446744073709551615 0x01…01`
    /// (Foundry 1.8.5).
    const CAST_BOOKS: &str = "280666b5000000000000000000000000000000000000000000000000ffffffffffffffff0101010101010101010101010101010101010101010101010101010101010101";

    /// `OnyxRevenue` as far as opening a book goes.
    #[derive(Default)]
    struct Revenue {
        reporter: [u8; 20],
        /// Facts whose observation book is open.
        open: BTreeSet<[u8; 32]>,
        /// Transactions the node took.
        sends: usize,
        /// `registerObservation` estimates asked (a send starts with one).
        estimates: usize,
        /// The fact the last estimate carried.
        pending: Option<[u8; 32]>,
        /// Someone else opens the book between this key's estimate and
        /// its send (the send then reverts `AlreadyRegistered`).
        stranger_first: bool,
        /// Receipts are not visible before the sender's deadline.
        unmined: bool,
        /// The node takes a transaction but it stays unmined (its effect
        /// waits in `held` until the test mines it).
        hold: bool,
        held: Option<[u8; 32]>,
        /// Every estimate reverts for a reason other than the book.
        broken: bool,
        /// Reads fail (the node is down).
        down: bool,
        last: Value,
    }

    struct Node(Arc<Mutex<Revenue>>);

    fn word(b: &[u8]) -> String {
        let mut w = [0u8; 32];
        w[32 - b.len()..].copy_from_slice(b);
        hex0x(&w)
    }

    impl JsonRpc for Node {
        fn call<'a>(
            &'a self,
            method: &'a str,
            params: Value,
        ) -> BoxFuture<'a, Result<Value, String>> {
            let mut c = self.0.lock().expect("lock");
            let data = |p: &Value| {
                hex::decode(
                    p[0]["data"]
                        .as_str()
                        .unwrap_or_default()
                        .trim_start_matches("0x"),
                )
                .expect("hex")
            };
            let r = match method {
                _ if c.down => Err("connection refused".to_string()),
                "eth_call" => {
                    let d = data(&params);
                    if d[..4] == selector("reporter()") {
                        Ok(json!(word(&c.reporter)))
                    } else {
                        assert_eq!(d[..4], selector("books(uint64,bytes32)"));
                        let key: [u8; 32] = d[36..68].try_into().expect("32");
                        let open = c.open.contains(&key) && d[28..36] == [0xff; 8];
                        Ok(json!(format!(
                            "{}{}",
                            word(&[u8::from(open)]),
                            "00".repeat(32 * 7)
                        )))
                    }
                }
                "eth_estimateGas" => {
                    let d = data(&params);
                    assert_eq!(d[..4], selector("registerObservation(bytes32)"));
                    c.estimates += 1;
                    let key: [u8; 32] = d[4..36].try_into().expect("32");
                    let from = params[0]["from"].as_str().unwrap_or_default().to_string();
                    if c.broken {
                        Err("execution reverted: out of gas".into())
                    } else if from != hex0x(&c.reporter) {
                        Err("execution reverted: NotReporter()".into())
                    } else if c.open.contains(&key) {
                        Err("execution reverted: AlreadyRegistered()".into())
                    } else {
                        c.pending = Some(key);
                        Ok(json!("0xb000"))
                    }
                }
                "eth_blockNumber" => Ok(json!("0x10")),
                "eth_getTransactionCount" => Ok(json!(format!("0x{:x}", c.sends))),
                "eth_getBlockByNumber" => {
                    Ok(json!({"number": "0x10", "baseFeePerGas": "0x3b9aca00"}))
                }
                "eth_maxPriorityFeePerGas" => Ok(json!("0xf4240")),
                "eth_sendRawTransaction" => {
                    c.sends += 1;
                    let key = c.pending.take().expect("estimated first");
                    if c.hold {
                        c.held = Some(key);
                        c.last = Value::Null;
                        return Box::pin(async move { Ok(json!("0xab")) });
                    }
                    // A stranger's opening lands first: ours reverts.
                    let ok = !c.stranger_first && c.open.insert(key);
                    c.open.insert(key);
                    c.last = json!({"status": if ok { "0x1" } else { "0x0" }, "blockNumber": "0x11",
                                    "gasUsed": "0xb000", "effectiveGasPrice": "0x1"});
                    Ok(json!(format!("0x{:064x}", c.sends)))
                }
                "eth_getTransactionReceipt" if c.unmined => Ok(Value::Null),
                "eth_getTransactionReceipt" => Ok(c.last.clone()),
                _ => Err(format!("unexpected {method}")),
            };
            Box::pin(async move { r })
        }
    }

    fn chain(f: impl FnOnce(&mut Revenue)) -> (Arc<Mutex<Revenue>>, Sender<Node>) {
        let mut r = Revenue::default();
        let s = Sender::new(
            Node(Arc::new(Mutex::new(Revenue::default()))),
            84_532,
            OPERATOR,
        )
        .expect("key");
        r.reporter = s.address();
        f(&mut r);
        let c = Arc::new(Mutex::new(r));
        let s = Sender::new(Node(c.clone()), 84_532, OPERATOR)
            .expect("key")
            .with_poll(Duration::from_millis(1));
        (c, s)
    }

    /// Paused tokio time (ROADMAP E12): deadlines and polls follow the
    /// test's order, not the machine's speed.
    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .start_paused(true)
            .build()
            .expect("runtime")
    }

    const DEADLINE: Duration = Duration::from_millis(20);

    #[test]
    fn calldata_matches_cast() {
        assert_eq!(
            hex::encode(book_calldata(OBSERVATION_ROUND, &FACT)),
            CAST_BOOKS
        );
        assert_eq!(hex::encode(reporter_calldata()), "010ec441");
    }

    /// L2API-4 (ROADMAP B1): a closed book is opened once by the reporter;
    /// running the deploy again reads it open and sends nothing.
    #[test]
    fn a_closed_book_is_opened_once_and_a_rerun_sends_nothing() {
        let (c, s) = chain(|_| {});
        let rt = rt();
        let o = rt
            .block_on(open_observation_book(&s, &REVENUE, &FACT, DEADLINE))
            .expect("opened");
        assert!(matches!(o, Opened::Now { block: 0x11, .. }), "{o:?}");
        assert!(rt
            .block_on(observation_book_open(&s, &REVENUE, &FACT))
            .expect("read"));
        let again = rt
            .block_on(open_observation_books(
                &s,
                &REVENUE,
                &[FACT, FACT],
                DEADLINE,
            ))
            .expect("open already");
        assert_eq!(again, vec![(FACT, Opened::AlreadyOpen)]);
        let g = c.lock().expect("lock");
        assert_eq!(g.sends, 1, "never opened twice");
        // Review B5/B1-R2: the rerun reads the book and stops there; it does
        // not lean on `AlreadyRegistered` (an estimate pinned to an older
        // block, REVLOG-R13-2, could pass and send a transaction that
        // reverts on chain and costs gas).
        assert_eq!(g.estimates, 1, "the rerun does not even estimate a send");
    }

    /// A book someone else opened (the node's why-4 registrar, an earlier
    /// run), before the read or between this key's estimate and its send,
    /// is success: open is open.
    #[test]
    fn a_book_another_opener_opened_first_is_success() {
        let rt = rt();
        let (c, s) = chain(|r| {
            r.open.insert(FACT.0);
        });
        assert_eq!(
            rt.block_on(open_observation_book(&s, &REVENUE, &FACT, DEADLINE)),
            Ok(Opened::AlreadyOpen)
        );
        assert_eq!(c.lock().expect("lock").sends, 0);
        assert_eq!(
            c.lock().expect("lock").estimates,
            0,
            "read first: nothing tried"
        );
        let (c, s) = chain(|r| r.stranger_first = true);
        assert_eq!(
            rt.block_on(open_observation_book(&s, &REVENUE, &FACT, DEADLINE)),
            Ok(Opened::AlreadyOpen),
            "our send reverted AlreadyRegistered; the book is open"
        );
        assert_eq!(c.lock().expect("lock").sends, 1);
    }

    /// Fail closed: a key that is not the reporter is refused before
    /// anything is sent; a revert for another reason, a closed book after
    /// an unmined send, a dead node, and the zero fact are errors naming
    /// the fact or the contract, so level 2 is not served.
    #[test]
    fn every_outcome_that_leaves_the_book_closed_or_unknown_is_an_error() {
        let rt = rt();
        let open = |f: &dyn Fn(&mut Revenue)| {
            let (c, s) = chain(f);
            let r = rt.block_on(open_observation_book(&s, &REVENUE, &FACT, DEADLINE));
            let sends = c.lock().expect("lock").sends;
            (r, sends)
        };
        let (r, sends) = open(&|r| r.reporter = [0x77; 20]);
        assert!(matches!(r, Err(OpenError::NotReporter { .. })), "{r:?}");
        assert_eq!(sends, 0, "nothing sent from a key that is not the reporter");
        let (r, _) = open(&|r| r.broken = true);
        let e = r.expect_err("a revert for another reason");
        assert!(
            matches!(&e, OpenError::Failed { fact, why } if fact == &hex0x(&FACT.0) && why.contains("out of gas")),
            "{e}"
        );
        let (r, _) = open(&|r| r.down = true);
        assert!(matches!(r, Err(OpenError::Read { .. })), "{r:?}");
        let (c, s) = chain(|_| {});
        assert!(matches!(
            rt.block_on(open_observation_book(&s, &REVENUE, &Hash32::ZERO, DEADLINE)),
            Err(OpenError::Failed { .. })
        ));
        assert_eq!(c.lock().expect("lock").sends, 0);
    }

    /// An open sent but not seen mined before the deadline: the book is
    /// read again. Open (it landed) is success; still closed is `Pending`
    /// (fail closed), and the rerun once it mined reads it open and sends
    /// nothing, so it never double-opens.
    #[test]
    fn an_unmined_open_fails_closed_and_the_rerun_never_double_opens() {
        let rt = rt();
        let (c, s) = chain(|r| r.unmined = true);
        assert_eq!(
            rt.block_on(open_observation_book(&s, &REVENUE, &FACT, DEADLINE)),
            Ok(Opened::AlreadyOpen),
            "the send landed: the book reads open"
        );
        assert_eq!(c.lock().expect("lock").sends, 1);
        let (c, s) = chain(|r| r.hold = true);
        let e = rt
            .block_on(open_observation_book(&s, &REVENUE, &FACT, DEADLINE))
            .expect_err("unmined and closed");
        assert!(
            matches!(&e, OpenError::Pending { tx, .. } if tx.len() == 66),
            "{e}"
        );
        assert!(e.to_string().contains("run again"), "{e}");
        {
            let mut g = c.lock().expect("lock");
            let k = g.held.take().expect("held");
            g.open.insert(k);
            g.hold = false;
        }
        assert_eq!(
            rt.block_on(open_observation_book(&s, &REVENUE, &FACT, DEADLINE)),
            Ok(Opened::AlreadyOpen)
        );
        assert_eq!(c.lock().expect("lock").sends, 1, "opened once");
    }

    #[test]
    fn registered_flag_is_a_strict_bool() {
        assert_eq!(registered_from_return(&[0u8; 256]), Ok(false));
        let mut one = [0u8; 32];
        one[31] = 1;
        assert_eq!(registered_from_return(&one), Ok(true));
        one[0] = 1;
        assert!(registered_from_return(&one).is_err());
        assert!(registered_from_return(&[0u8; 31]).is_err());
    }
}
