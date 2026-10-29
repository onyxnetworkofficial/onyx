//! A clean stop (OPS-L1-F1): no new connections, requests in flight finish,
//! then the settle tasks they started finish, so a settle that ends in time
//! writes its receipt (facilitator mode) or its pending entry (self-settle).
//! Only then is the runtime dropped.
//!
//! Recovery never depends on the stop being clean (OPS-L2-F1): every paid
//! settle was written as an intent before it was made, so a settle that
//! ended with an unknown outcome, was cut short, or died with the process
//! (SIGKILL) is still on disk, and the intent resolver later turns it into
//! a receipt or drops it as provably unused, from the chain's own record.

use onyx_api::SettleTasks;
use std::future::Future;
use std::time::Duration;

/// A self-settle waits this long for its transaction (Base Sepolia mines
/// every 2 s; the server's settle budget stays under client deadlines).
pub const SELF_SETTLE_DEADLINE: Duration = Duration::from_secs(30);

/// Added to each wait, for scheduling and the last disk sync.
pub const STOP_SLACK: Duration = Duration::from_secs(2);

/// The supervisor stop timeout the shipped configs use (systemd
/// `TimeoutStopSec`, Docker `--stop-timeout`). [`total`] must stay below it.
pub const SUPERVISOR_STOP: Duration = Duration::from_secs(75);

/// The longest [`drain`] waits for a server with this stop budget
/// ([`onyx_api::ApiState::stop_budget`]).
pub fn total(stop_budget: Duration) -> Duration {
    stop_budget + STOP_SLACK * 2
}

/// What a stop managed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stopped {
    /// Every connection closed in time.
    pub requests_drained: bool,
    /// Settle tasks still running when the wait ended (0 on a clean stop).
    pub settles_left: usize,
    /// Settles that ended with an unknown outcome since start (OPS-L2-F3):
    /// each is an intent the resolver settles from the chain.
    pub unknown_outcomes: u64,
}

impl Stopped {
    /// The same stop, also reporting `n` unknown settle outcomes.
    pub fn with_unknown(mut self, n: u64) -> Self {
        self.unknown_outcomes = n;
        self
    }

    /// Nothing was cut short and no settle ended unknown.
    pub fn clean(&self) -> bool {
        self.requests_drained && self.settles_left == 0 && self.unknown_outcomes == 0
    }
}

/// Wait for `server` (an axum server already told to shut down gracefully)
/// for at most `request_timeout` plus slack, then for every settle task in
/// `tasks` for at most `settle_budget` plus slack.
pub async fn drain<F: Future>(
    server: F,
    tasks: &SettleTasks,
    request_timeout: Duration,
    settle_budget: Duration,
) -> Stopped {
    let requests_drained = tokio::time::timeout(request_timeout + STOP_SLACK, server)
        .await
        .is_ok();
    let settled = tasks.wait_idle(settle_budget + STOP_SLACK).await;
    Stopped {
        requests_drained,
        settles_left: if settled { 0 } else { tasks.live() },
        unknown_outcomes: 0,
    }
}
