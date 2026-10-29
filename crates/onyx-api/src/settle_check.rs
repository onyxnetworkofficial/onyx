//! The authorization check after a settle whose outcome is unknown (THREATS
//! M7-F5 residual; ADR-0023 Amendment 2). A facilitator settle that times
//! out or fails in transport may or may not have moved the payer's USDC.
//! Before the server re-sends it or gives up on it, it asks the token:
//!
//! 1. `authorizationState(from, nonce)` at the latest block.
//! 2. Used: the token's own logs decide ([`crate::arrival::use_of`]). The
//!    `AuthorizationUsed(from, nonce)` log followed by `Transfer(from ->
//!    OnyxRevenue, value)` in one successful transaction is proof of payment:
//!    the request is served (with that transaction in `PAYMENT-RESPONSE`)
//!    and the receipt written as for any settled payment. A use that paid
//!    someone else, or a cancel, is the payer's refusal: 402, nothing served.
//!    A used authorization whose use is not found (a backend behind another)
//!    is read again within the check's budget, then stays unknown.
//! 3. Unused: the same signed authorization is sent once more. Its nonce
//!    lets it move money at most once, so a re-send can never make it pay
//!    twice, even if the first settle is still waiting in a mempool;
//!    whichever lands is the payment. A re-send that does not succeed is
//!    followed, about one block later, by one more check (the first settle
//!    may have landed meanwhile). If the token refused the re-send as used,
//!    that check reads the logs even when a lagging node still reads the
//!    authorization unused ([`SettleCheck::logged_use`], M7L3-R1-1). Only a
//!    facilitator success or the token's logs ever make the request served.
//! 4. The identical retry of a request left unknown runs this check again
//!    while its intent is open (no new verify or settle), and is served once
//!    the transfer is proven (M7L3-R1-1). Once the resolver has recovered
//!    the intent, the retry is served from the resolver's proof instead
//!    (no check; [`crate::intents::IntentLog::take_proven`], M7L3-R2-1). One
//!    authorization is re-sent at most [`crate::intents::MAX_RESENDS`]
//!    times in all (M7L3-R2-5).
//!
//! Anything still undecided keeps the M7-F5 semantics: 503 with the outcome
//! unknown, the payer's unit spent, the settle intent left to the
//! [`crate::intents::IntentResolver`]. Self-settled payments
//! ([`crate::self_settle`]) already read `authorizationState` and keep a
//! broadcast settle as pending; this check is for facilitator settles.

use crate::arrival::{logged_use, use_of, Payment, UseOf};
use crate::roots::JsonRpc;
use crate::x402::BoxFuture;

/// Reads what became of an authorization (see the module docs).
pub trait SettleCheck: Send + Sync {
    /// What became of the authorization `nonce` of `from` for `value`,
    /// searching blocks whose timestamps lie in `earliest..=latest` (unix
    /// seconds, chain time). An error is never a "no".
    fn use_of<'a>(
        &'a self,
        from: [u8; 20],
        nonce: [u8; 32],
        value: u128,
        earliest: u64,
        latest: u64,
    ) -> BoxFuture<'a, Result<UseOf, String>>;

    /// Like [`SettleCheck::use_of`] from the token's logs alone, without
    /// reading `authorizationState` (M7L3-R1-1: a re-send the token refused
    /// while a lagging node still reads the authorization unused). Never
    /// [`UseOf::Unused`].
    fn logged_use<'a>(
        &'a self,
        from: [u8; 20],
        nonce: [u8; 32],
        value: u128,
        earliest: u64,
        latest: u64,
    ) -> BoxFuture<'a, Result<UseOf, String>>;
}

/// [`SettleCheck`] against the token and `OnyxRevenue` through a node.
pub struct ChainSettleCheck<T: JsonRpc> {
    rpc: T,
    token: [u8; 20],
    revenue: [u8; 20],
}

impl<T: JsonRpc> ChainSettleCheck<T> {
    /// Check payments in `token` (EIP-3009) to `revenue` through `rpc`.
    pub fn new(rpc: T, token: [u8; 20], revenue: [u8; 20]) -> Self {
        ChainSettleCheck {
            rpc,
            token,
            revenue,
        }
    }
}

impl<T: JsonRpc> SettleCheck for ChainSettleCheck<T> {
    fn use_of<'a>(
        &'a self,
        from: [u8; 20],
        nonce: [u8; 32],
        value: u128,
        earliest: u64,
        latest: u64,
    ) -> BoxFuture<'a, Result<UseOf, String>> {
        Box::pin(async move {
            let p = Payment {
                token: self.token,
                revenue: self.revenue,
                from,
                nonce,
                value,
            };
            use_of(&self.rpc, &p, earliest, latest).await
        })
    }

    fn logged_use<'a>(
        &'a self,
        from: [u8; 20],
        nonce: [u8; 32],
        value: u128,
        earliest: u64,
        latest: u64,
    ) -> BoxFuture<'a, Result<UseOf, String>> {
        Box::pin(async move {
            let p = Payment {
                token: self.token,
                revenue: self.revenue,
                from,
                nonce,
                value,
            };
            logged_use(&self.rpc, &p, earliest, latest).await
        })
    }
}
