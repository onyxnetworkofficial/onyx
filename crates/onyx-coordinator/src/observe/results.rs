//! The settled-results store's methods: feed templates, settles, the challenge window, slashes and
//! the claims a consumer reads (the store itself is [`ObservationResults`] in observe.rs).

use super::*;

impl ObservationResults {
    /// An empty store and the one capability that may register feed templates
    /// in it.
    #[allow(clippy::new_ret_no_self)]
    pub fn new() -> (Self, FeedAdmin) {
        let id = NEXT_STORE.fetch_add(1, Ordering::Relaxed);
        (
            ObservationResults {
                id,
                templates: BTreeMap::new(),
                decisions: Vec::new(),
                by_result: BTreeMap::new(),
                draw_of_result: BTreeMap::new(),
                rounds: BTreeSet::new(),
                facts: BTreeSet::new(),
                non_value: BTreeMap::new(),
                draws: BTreeMap::new(),
                non_value_cap: MAX_NON_VALUE_ROUNDS,
                closed: BTreeSet::new(),
                fact_decisions: Vec::new(),
                announcements: BTreeMap::new(),
                opened: BTreeMap::new(),
                released: BTreeSet::new(),
                exposure: BTreeMap::new(),
                unfinished: BTreeMap::new(),
                failed: BTreeSet::new(),
                results_read: BTreeMap::new(),
            },
            FeedAdmin { store: id },
        )
    }

    /// Register `template` as its metric's one canonical feed (L2SRC-R2-F1).
    /// A reviewer action: needs this store's [`FeedAdmin`], is recorded with
    /// `reason` and `now`, and is final (a metric's template is never
    /// replaced; a new feed is a new metric).
    pub fn register_feed(
        &mut self,
        admin: &FeedAdmin,
        template: FeedTemplate,
        reason: &str,
        now: u64,
    ) -> Result<(), ObserveError> {
        if admin.store != self.id {
            return Err(ObserveError::NotFeedAdmin);
        }
        template.validate()?;
        if self.templates.contains_key(&template.metric) {
            return Err(ObserveError::FeedAlreadyRegistered);
        }
        self.decisions.push(FeedDecision {
            at: now,
            metric: template.metric.clone(),
            template: template.id(),
            reason: reason.to_owned(),
        });
        self.templates.insert(template.metric.clone(), template);
        Ok(())
    }

    /// Reopen a dated or period fact closed after the cap of rounds without a
    /// value (SEC-R2-F1). A reviewer action: needs this store's
    /// [`FeedAdmin`], is recorded with `reason`; the fact's count restarts.
    pub fn reopen_fact(
        &mut self,
        admin: &FeedAdmin,
        fact: Hash32,
        reason: &str,
        now: u64,
    ) -> Result<(), ObserveError> {
        if admin.store != self.id {
            return Err(ObserveError::NotFeedAdmin);
        }
        if !self.closed.remove(&fact) {
            return Err(ObserveError::FactNotClosed);
        }
        self.non_value.remove(&fact);
        self.fact_decisions.push(FactDecision {
            at: now,
            fact: Some(fact),
            cap: self.non_value_cap,
            reason: reason.to_owned(),
        });
        Ok(())
    }

    /// Set the cap of rounds without a value per dated or period fact
    /// (default [`MAX_NON_VALUE_ROUNDS`]; at least 1). A reviewer action,
    /// recorded. Facts already at or over a lower cap close at their next
    /// round without a value.
    pub fn set_non_value_cap(
        &mut self,
        admin: &FeedAdmin,
        cap: u32,
        reason: &str,
        now: u64,
    ) -> Result<(), ObserveError> {
        if admin.store != self.id {
            return Err(ObserveError::NotFeedAdmin);
        }
        self.non_value_cap = cap.max(1);
        self.fact_decisions.push(FactDecision {
            at: now,
            fact: None,
            cap: self.non_value_cap,
            reason: reason.to_owned(),
        });
        Ok(())
    }

    /// Every reopening and cap change, in order (SEC-R2-F1).
    pub fn fact_decisions(&self) -> &[FactDecision] {
        &self.fact_decisions
    }

    /// The book of dated and period facts that settled rounds without a
    /// value, as JSON a runner keeps between restarts (AUD-2-F9, AUD-2R-F4):
    /// `{"version": 8, "cap": n, "facts": [{"fact": "0x..", "rounds": n,
    /// "closed": bool, "draws": n}], "decisions": [{"at", "fact", "cap", "reason"}],
    /// "unfinished": ["0x<borsh>"], "announced": ["0x<borsh>"],
    /// "failed": ["0x<fact>"]}`, facts sorted. The cap and the reviewer decisions travel with the
    /// counts, so a restart neither resets a reviewer's cap nor loses the
    /// record of a reopening. Under the same guard as [`Self::save_book`]
    /// (review R9-F3): refused while a settled round waits for its results
    /// to be written, or when the book would drop a draw it holds.
    pub fn non_value_book(&self) -> Result<serde_json::Value, String> {
        if self.awaiting_publication() {
            return Err(
                "a settled round waits for its results to be written; the book keeps it unsettled until then"
                    .into(),
            );
        }
        if let Some(why) = self.unwritable_draw() {
            return Err(format!(
                "{why}; the book would lose it (the draw invariant)"
            ));
        }
        Ok(self.book_json())
    }

    /// [`Self::non_value_book`] without the guard.
    fn book_json(&self) -> serde_json::Value {
        let facts: BTreeSet<&Hash32> = self
            .non_value
            .keys()
            .chain(&self.closed)
            .chain(self.draws.keys())
            .collect();
        serde_json::json!({
            "version": 8,
            "cap": self.non_value_cap,
            "facts": facts
                .into_iter()
                .map(|f| {
                    serde_json::json!({
                        "fact": f.to_string(),
                        "rounds": self.non_value.get(f).copied().unwrap_or(0),
                        "closed": self.closed.contains(f),
                        "draws": self.draws.get(f).copied().unwrap_or(0),
                    })
                })
                .collect::<Vec<_>>(),
            "decisions": self
                .fact_decisions
                .iter()
                .map(|d| {
                    serde_json::json!({
                        "at": d.at,
                        "fact": d.fact.map(|f| f.to_string()),
                        "cap": d.cap,
                        "reason": d.reason,
                    })
                })
                .collect::<Vec<_>>(),
            // Review REVLOG-R3-2: unfinished rounds, Borsh as 0x-hex, each
            // re-checked when read back; since version 4 (review
            // REVLOG-R4-3) each reveal carries its arrival time.
            "unfinished": self
                .unfinished
                .values()
                .map(|u| format!("0x{}", hex::encode(borsh::to_vec(u).unwrap_or_default())))
                .collect::<Vec<_>>(),
            // Version 5 (review REVLOG-R5): each fact's latest draw announced
            // and not opened, so the next run resumes it; and the facts whose
            // kept round is a failed attempt not yet published (REVLOG-R4-5).
            "announced": self
                .pending_announcements()
                .into_iter()
                .map(|w| format!("0x{}", hex::encode(borsh::to_vec(&w).unwrap_or_default())))
                .collect::<Vec<_>>(),
            "failed": self.failed.iter().map(ToString::to_string).collect::<Vec<_>>(),
        })
    }

    /// Each fact's draw the book holds ([`Self::held`]) that is not kept
    /// as an opened round (review REVLOG-R5; review R8: an opened round not
    /// kept, a run stopped between the opening and the keeping, is written
    /// as announced, so the next run opens it again on the same draw rather
    /// than the book forgetting it).
    fn pending_announcements(&self) -> Vec<AnnouncedWire> {
        self.draws
            .keys()
            .filter_map(|fact| {
                let attempt = self.held(fact)?;
                if self.kept_at(fact, attempt) {
                    return None;
                }
                let a = self.announcements.get(&(*fact, attempt))?;
                Some(AnnouncedWire {
                    fact: *fact,
                    attempt,
                    spec_id: a.spec_id,
                    cfg: a.cfg,
                    pool: a.pool.clone(),
                    beacon_height: a.beacon_height,
                    full_bond: a.full_bond.iter().map(|(k, v)| (*k, *v)).collect(),
                })
            })
            .collect()
    }

    /// Take in a book written by [`Self::non_value_book`] (AUD-2-F9,
    /// AUD-2R-F4). Counts only tighten: a count rises to the book's count and
    /// a fact the book closed, or whose count reaches the cap, is closed.
    /// Nothing is reopened here; that stays the admin's recorded
    /// [`Self::reopen_fact`]. The book's cap is the last recorded reviewer
    /// setting and replaces the default; its decisions are kept as the
    /// history. A version-1 book (a bare array of facts) is still read.
    pub fn absorb_non_value_book(&mut self, book: &serde_json::Value) -> Result<(), ObserveError> {
        let bad = |m: &str| ObserveError::BadNonValueBook(m.to_owned());
        let mut unfinished = vec![];
        let mut announced: Vec<AnnouncedWire> = vec![];
        let mut failed: Vec<Hash32> = vec![];
        let rows = match book {
            serde_json::Value::Array(rows) => rows,
            serde_json::Value::Object(o) => {
                let version = o.get("version").and_then(serde_json::Value::as_u64);
                if !matches!(version, Some(2..=8)) {
                    return Err(bad("unknown book version"));
                }
                // Version 5 (review REVLOG-R5): draws announced and not
                // opened, and failed attempts not yet published.
                if matches!(version, Some(5..=8)) {
                    announced = o
                        .get("announced")
                        .and_then(serde_json::Value::as_array)
                        .ok_or_else(|| bad("announced is not an array"))?
                        .iter()
                        .map(|x| {
                            x.as_str()
                                .and_then(|h| hex::decode(h.strip_prefix("0x")?).ok())
                                .and_then(|b| AnnouncedWire::try_from_slice(&b).ok())
                                .ok_or_else(|| bad("an announced draw that does not decode"))
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    failed = o
                        .get("failed")
                        .and_then(serde_json::Value::as_array)
                        .ok_or_else(|| bad("failed is not an array"))?
                        .iter()
                        .map(|x| {
                            x.as_str()
                                .and_then(parse_hash)
                                .ok_or_else(|| bad("a failed fact that is not a 0x hash"))
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                }
                // Version 3 (review REVLOG-R3-2) carries unfinished rounds;
                // version 4 (review REVLOG-R4-3) their reveals' arrival times.
                // Version 6 (ROADMAP C11): a round's sealed snapshot.
                // Version 7 (ROADMAP C51): its time-lock read. Version 8
                // (ROADMAP C52): the read's far round; a version-7
                // time-locked round (posts without a far copy, never
                // deployed) is refused, any other version-7 round loads.
                if let Some(v @ (3..=8)) = version {
                    unfinished = o
                        .get("unfinished")
                        .and_then(serde_json::Value::as_array)
                        .ok_or_else(|| bad("unfinished is not an array"))?
                        .iter()
                        .map(|x| {
                            let b = x
                                .as_str()
                                .and_then(|h| hex::decode(h.strip_prefix("0x")?).ok());
                            b.and_then(|b| match v {
                                3 => UnfinishedWireV3::try_from_slice(&b).ok().map(Into::into),
                                4 | 5 => UnfinishedWireV5::try_from_slice(&b).ok().map(Into::into),
                                6 => UnfinishedWireV6::try_from_slice(&b).ok().map(Into::into),
                                7 => UnfinishedWireV7::try_from_slice(&b)
                                    .ok()
                                    .and_then(UnfinishedWireV7::upgrade),
                                _ => UnfinishedWire::try_from_slice(&b).ok(),
                            })
                            .ok_or_else(|| {
                                bad(if v == 7 {
                                    "an unfinished round that does not decode (a version-7 time-locked round has no far-date copy, ROADMAP C52: finish it with the build that wrote it)"
                                } else {
                                    "an unfinished round that does not decode"
                                })
                            })
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                }
                let cap = o
                    .get("cap")
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|n| u32::try_from(n).ok())
                    .filter(|n| *n >= 1)
                    .ok_or_else(|| bad("cap is not a count of at least 1"))?;
                self.non_value_cap = cap;
                let decisions = o
                    .get("decisions")
                    .and_then(serde_json::Value::as_array)
                    .ok_or_else(|| bad("decisions is not an array"))?;
                for d in decisions {
                    let fact = match &d["fact"] {
                        serde_json::Value::Null => None,
                        v => Some(
                            v.as_str()
                                .and_then(parse_hash)
                                .ok_or_else(|| bad("decision fact is not a 0x hash"))?,
                        ),
                    };
                    self.fact_decisions.push(FactDecision {
                        at: d["at"].as_u64().ok_or_else(|| bad("decision at"))?,
                        fact,
                        cap: d["cap"]
                            .as_u64()
                            .and_then(|n| u32::try_from(n).ok())
                            .ok_or_else(|| bad("decision cap"))?,
                        reason: d["reason"]
                            .as_str()
                            .ok_or_else(|| bad("decision reason"))?
                            .to_owned(),
                    });
                }
                o.get("facts")
                    .and_then(serde_json::Value::as_array)
                    .ok_or_else(|| bad("facts is not an array"))?
            }
            _ => return Err(bad("not a book")),
        };
        // Review REVLOG-R4-2: unfinished rounds are restored BEFORE any fact
        // is closed, and a fact with one is not closed by its count here: it
        // was opened before a lower cap and closes at its next round without
        // a value ([`Self::settle`]), as it would have in the process that
        // saved the book. Only a fact the book itself closed stays closed.
        let mut pending = BTreeSet::new();
        for w in unfinished {
            pending.insert(self.restore_unfinished(w)?);
        }
        for f in failed {
            if !self.unfinished.contains_key(&f) {
                return Err(bad("a failed attempt without its kept round"));
            }
            self.failed.insert(f);
        }
        for w in announced {
            pending.insert(self.restore_announced(w)?);
        }
        for row in rows {
            let hex = row["fact"]
                .as_str()
                .ok_or_else(|| bad("fact is not a string"))?;
            let fact = parse_hash(hex).ok_or_else(|| bad("fact is not a 0x hash"))?;
            let rounds = row["rounds"]
                .as_u64()
                .and_then(|n| u32::try_from(n).ok())
                .ok_or_else(|| bad("rounds is not a count"))?;
            let closed = row["closed"]
                .as_bool()
                .ok_or_else(|| bad("closed is not a bool"))?;
            // Draws used (L2-F2); a book written before the draw has none.
            let draws = match &row["draws"] {
                serde_json::Value::Null => rounds,
                v => v
                    .as_u64()
                    .and_then(|n| u32::try_from(n).ok())
                    .ok_or_else(|| bad("draws is not a count"))?,
            };
            let d = self.draws.entry(fact).or_insert(0);
            *d = (*d).max(draws);
            if rounds == 0 && !closed {
                continue;
            }
            let n = self.non_value.entry(fact).or_insert(0);
            *n = (*n).max(rounds);
            if closed || (*n >= self.non_value_cap && !pending.contains(&fact)) {
                self.closed.insert(fact);
            }
        }
        Ok(())
    }

    /// Whether a dated or period fact is closed after the cap of rounds
    /// without a value, and how many such rounds it has taken.
    pub fn fact_state(&self, fact: &Hash32) -> (bool, u32) {
        (
            self.closed.contains(fact),
            self.non_value.get(fact).copied().unwrap_or(0),
        )
    }

    /// The registered template of `metric`.
    pub fn template(&self, metric: &MetricId) -> Option<&FeedTemplate> {
        self.templates.get(metric)
    }

    /// Every feed registration, in order.
    pub fn feed_decisions(&self) -> &[FeedDecision] {
        &self.decisions
    }

    /// The template `spec` must equal, checked (L2SRC-R2-F1).
    fn template_of(&self, spec: &ObservationSpec) -> Result<&FeedTemplate, ObserveError> {
        let t = self
            .templates
            .get(&spec.metric)
            .ok_or(ObserveError::UnknownFeed)?;
        t.matches(spec)?;
        Ok(t)
    }

    /// Open a round, the only way to (L2SRC-R2-F1): `spec` must be exactly its
    /// metric's registered template instantiated for the spec's subject and
    /// window start, and a dated subject's date must be observable in that
    /// window.
    ///
    /// The observers are never chosen by the caller (L2-F2): they are the
    /// draw ([`draw_observers`]) of the pool snapshot fixed by
    /// [`announce_draw`](Self::announce_draw), with `beacon` (the chain's block
    /// hash at the announced height). Each drawn observer's bond must still
    /// cover the slash after the rounds it already sits on; the slash is then
    /// held until the round settles.
    pub fn open_round(
        &mut self,
        spec: ObservationSpec,
        cfg: ObserveConfig,
        pool: Vec<Observer>,
        chain: &dyn ChainView,
    ) -> Result<ObservationRound, ObserveError> {
        let (key, round) = self.prepare_open(spec, cfg, pool, chain)?;
        self.commit_open(key, &round);
        Ok(round)
    }

    /// [`Self::open_round`] on the registry's record `rec` of the draw, and
    /// the opened round kept in the book ([`Self::keep_unfinished`]) in the
    /// same step (review R8-1): the record is checked against the round
    /// BEFORE anything changes, so a mismatch (a stale or mixed read, or
    /// another draw) leaves the draw announced in the book, never opened and
    /// not kept. [`draw_and_open`] opens only through this.
    pub fn open_drawn(
        &mut self,
        spec: ObservationSpec,
        cfg: ObserveConfig,
        pool: Vec<Observer>,
        chain: &dyn ChainView,
        rec: &DrawRecord,
    ) -> Result<ObservationRound, ObserveError> {
        let (key, round) = self.prepare_open(spec, cfg, pool, chain)?;
        let d = round.draw();
        let why = if rec.attempt != d.attempt {
            Some("attempt")
        } else if rec.draw_count != d.attempt.saturating_add(1) {
            Some("drawCount (a later draw, or a read from another block)")
        } else if rec.commit != draw_commit(&d.pool, &round.spec_id, &cfg_hash(&round.cfg)) {
            Some("commitment")
        } else if rec.beacon_height != d.beacon_height {
            Some("beacon height")
        } else if rec.beacon != d.beacon {
            Some("beacon")
        } else if rec.announced_at >= d.beacon_height {
            Some("announced at or after its beacon height")
        } else {
            None
        };
        if let Some(why) = why {
            return Err(ObserveError::RegistryDrawMismatch(why));
        }
        self.commit_open(key, &round);
        self.unfinished.insert(
            key.0,
            UnfinishedRound {
                round: round.clone(),
                onchain_draw: *rec,
                reveals: vec![],
            },
        );
        self.failed.remove(&key.0);
        Ok(round)
    }

    /// Every check of [`Self::open_round`], changing nothing: the draw's
    /// key and the round.
    fn prepare_open(
        &self,
        spec: ObservationSpec,
        cfg: ObserveConfig,
        pool: Vec<Observer>,
        chain: &dyn ChainView,
    ) -> Result<((Hash32, u32), ObservationRound), ObserveError> {
        let (fact, _) = self.fact_of(&spec)?;
        // The draw opened is the fact's latest announced one (L2RND-R1-F2):
        // an earlier attempt is never opened after a later one was announced.
        let attempt = self
            .draws
            .get(&fact)
            .copied()
            .unwrap_or(0)
            .checked_sub(1)
            .ok_or(ObserveError::DrawNotAnnounced)?;
        if self.opened.contains_key(&(fact, attempt)) || self.released.contains(&(fact, attempt)) {
            return Err(ObserveError::DrawMismatch);
        }
        let ann = self
            .announcements
            .get(&(fact, attempt))
            .ok_or(ObserveError::DrawNotAnnounced)?;
        if ann.spec_id != spec.id() || ann.cfg != cfg || ann.pool != pool {
            return Err(ObserveError::DrawMismatch);
        }
        // The beacon is the chain's block hash at the announced height, read
        // here, never chosen by the caller (L2RND-R1-F1).
        let beacon = chain
            .block_hash(ann.beacon_height)
            .map_err(ObserveError::Chain)?
            .ok_or(ObserveError::BeaconNotYet)?;
        let draw = DrawSeed {
            pool: pool_hash(&pool),
            beacon_height: ann.beacon_height,
            beacon,
            attempt,
        };
        let full_bond = ann.full_bond.clone();
        let round = ObservationRound::open(spec, cfg, pool, draw)?;
        for o in &round.observers {
            let held = self.exposure.get(&o.address).copied().unwrap_or(0);
            let full = full_bond.get(&o.address).copied().unwrap_or(0);
            if held.saturating_add(cfg.outlier_slash) > full {
                return Err(ObserveError::ObserverExposed);
            }
        }
        Ok(((fact, attempt), round))
    }

    /// Record `round` opened on draw `key`: its observers' slashes held.
    pub(super) fn commit_open(&mut self, key: (Hash32, u32), round: &ObservationRound) {
        for o in &round.observers {
            let e = self.exposure.entry(o.address).or_insert(0);
            *e = e.saturating_add(round.cfg.outlier_slash);
        }
        self.opened.insert(key, round.round_id);
    }

    /// The fact a spec's round settles and whether its subject is dated.
    /// Refuses an unregistered or mismatched spec, a closed fact, and a fact
    /// already settled.
    fn fact_of(&self, spec: &ObservationSpec) -> Result<(Hash32, bool), ObserveError> {
        let dated = self.template_of(spec)?.is_dated();
        let fact = if dated {
            fact_key_dated(spec)
        } else {
            fact_key(spec)
        };
        if self.closed.contains(&fact) {
            return Err(ObserveError::FactClosed);
        }
        if self.facts.contains(&fact) {
            return Err(ObserveError::FeedAlreadySettled);
        }
        Ok((fact, dated))
    }

    /// The fact a spec's round settles and the attempt its draw must name: 0
    /// for a fixed subject, the count of rounds without a value for a dated or
    /// period one. Refuses an unregistered or mismatched spec, a closed fact,
    /// and a fact already settled.
    fn fact_attempt(&self, spec: &ObservationSpec) -> Result<(Hash32, u32), ObserveError> {
        let (fact, _) = self.fact_of(spec)?;
        let attempt = self.draws.get(&fact).copied().unwrap_or(0);
        if attempt >= MAX_DRAW_ATTEMPTS {
            return Err(ObserveError::FactClosed);
        }
        Ok((fact, attempt))
    }

    /// The registry's fact arguments for `spec` (checked against its template).
    pub fn onchain_fact(&self, spec: &ObservationSpec) -> Result<OnchainFact, ObserveError> {
        let t = self.template_of(spec)?;
        Ok(OnchainFact {
            metric_key: onyx_core::onchain::metric_key(&spec.metric),
            subject_key: onyx_core::onchain::subject_key(&spec.subject),
            window_end: spec.window_end,
            dated: t.is_dated(),
            template: t.id(),
        })
    }

    /// Raise the fact's draw count to `onchain`, the registry's `drawCount`
    /// (L2RND-R1-F3): the chain is the authority, so a coordinator that
    /// announced on chain and crashed before saving its book never draws
    /// that attempt again. Counts only rise.
    pub fn sync_draws(&mut self, spec: &ObservationSpec, onchain: u32) -> Result<(), ObserveError> {
        let (fact, _) = self.fact_of(spec)?;
        // The draw invariant (review R8): a count past the book's held draw
        // would make it no longer the latest, so the book would forget it.
        // It is released first, under a proof ([`Self::release_draw`]).
        if let Some(a) = self.held(&fact) {
            if onchain > a.saturating_add(1) {
                return Err(ObserveError::DrawHeld(a));
            }
        }
        let d = self.draws.entry(fact).or_insert(0);
        *d = (*d).max(onchain);
        Ok(())
    }

    /// Raise a dated fact's rounds without a value to the chain's count
    /// (L2RND-R2-F3). A dated subject names one fact, so its `drawBudget`
    /// strikes since the last owner reopen are exactly the announced draws
    /// that did not end with a recorded value: a coordinator that saw a draw
    /// and dropped it (crash, restart, a lost book) cannot skip the count.
    /// The count is read from the chain, never from local memory. Closes the
    /// fact at the cap. Fixed subjects are bounded by the chain alone.
    pub fn sync_budget(
        &mut self,
        spec: &ObservationSpec,
        strikes: u32,
        allowance: u32,
    ) -> Result<(), ObserveError> {
        let (fact, dated) = self.fact_of(spec)?;
        let since_reopen = strikes.saturating_sub(allowance.saturating_sub(MAX_SUBJECT_DRAWS));
        if dated && since_reopen > 0 {
            let n = self.non_value.entry(fact).or_insert(0);
            *n = (*n).max(since_reopen);
            if *n >= self.non_value_cap {
                self.closed.insert(fact);
            }
        }
        Ok(())
    }

    /// The attempt of the draw the book holds for `fact` (the draw
    /// invariant, review R8): its latest announced draw, unless released
    /// under a proof ([`Self::release_draw`]). Opened or not, kept or not,
    /// settled or not: the book holds it until it is released.
    fn held(&self, fact: &Hash32) -> Option<u32> {
        let a = self.draws.get(fact)?.checked_sub(1)?;
        (self.announcements.contains_key(&(*fact, a)) && !self.released.contains(&(*fact, a)))
            .then_some(a)
    }

    /// The fact's kept round is on draw `attempt`.
    fn kept_at(&self, fact: &Hash32, attempt: u32) -> bool {
        self.unfinished
            .get(fact)
            .is_some_and(|u| u.round.draw.attempt == attempt)
    }

    /// The on-chain arguments of the draw the book holds for `spec`'s fact
    /// ([`Self::held`]), whatever its state (announced, opened, kept,
    /// settled; the fact closed or not): `None` when the book holds none.
    pub fn held_draw(&self, spec: &ObservationSpec) -> Option<OnchainDraw> {
        let fact = self.fact_key_of(spec).ok()?;
        let attempt = self.held(&fact)?;
        let ann = self.announcements.get(&(fact, attempt))?;
        Some(OnchainDraw {
            fact: self.onchain_fact(spec).ok()?,
            attempt,
            commit: draw_commit(&pool_hash(&ann.pool), &ann.spec_id, &cfg_hash(&ann.cfg)),
            beacon_height: ann.beacon_height,
        })
    }

    /// THE DRAW INVARIANT (review R8, ADR-0027 amendment 8 item 8f): the
    /// ONLY way a draw leaves the book. Once the book is saved with a draw
    /// (before `announceDraw` is sent), the draw stays in it, and in every
    /// book saved after, until one of:
    ///
    /// - [`Release::Published`]: its round, settled here, or its failed
    ///   attempt is durably written (the round id of the book's draw);
    /// - [`Release::NeverLands`]: a final block at or past its beacon height
    ///   shows no such draw at its attempt (none, or another one):
    ///   `announceDraw` reverts `BeaconNotFuture`/`BadAttempt` from there on;
    /// - [`Release::Unfixable`]: a final block past its beacon height plus
    ///   the 256-block `blockhash` window shows it with a zero beacon:
    ///   `fixBeacon` reverts `BeaconUnavailable` from there on;
    /// - [`Release::NeverSent`]: the save that held it failed and nothing
    ///   was sent ([`draw_and_open`] only).
    ///
    /// Each proof is checked here against the book's own draw (attempt,
    /// commitment, beacon height); anything short of one is refused
    /// ([`ObserveError::NoReleaseProof`]) and the draw stays. The chain
    /// proofs are built only from ONE final-block read
    /// ([`DrawChain::draw_of_final`], [`final_read`]). A draw that leaves
    /// can never become a fixed beacon without a published round, the state
    /// every client refuses for good.
    pub(super) fn release_draw(
        &mut self,
        spec: &ObservationSpec,
        why: Release,
    ) -> Result<(), ObserveError> {
        let (fact, dated) = (self.fact_key_of(spec)?, self.template_of(spec)?.is_dated());
        let attempt = self.held(&fact).ok_or(ObserveError::DrawNotAnnounced)?;
        let key = (fact, attempt);
        let ann = self
            .announcements
            .get(&key)
            .ok_or(ObserveError::DrawNotAnnounced)?;
        let commit = draw_commit(&pool_hash(&ann.pool), &ann.spec_id, &cfg_hash(&ann.cfg));
        let ours = |r: &DrawRecord| {
            r.attempt == attempt && r.commit == commit && r.beacon_height == ann.beacon_height
        };
        let bh = ann.beacon_height;
        match why {
            Release::Published(round_id) => {
                let failed = self.failed.contains(&fact) && self.kept_at(&fact, attempt);
                let kept_id = self
                    .unfinished
                    .get(&fact)
                    .filter(|u| u.round.draw.attempt == attempt)
                    .map(|u| u.round.round_id);
                let opened_id = self.opened.get(&key).copied();
                let settled = self.rounds.contains(&round_id);
                let this =
                    kept_id == Some(round_id) || (kept_id.is_none() && opened_id == Some(round_id));
                if !this || !(settled || failed) {
                    return Err(ObserveError::NoReleaseProof(
                        "not the round of the book's draw, settled here or kept as a failed attempt",
                    ));
                }
                if kept_id.is_some() {
                    self.unfinished.remove(&fact);
                    self.failed.remove(&fact);
                }
                self.released.insert(key);
            }
            Release::NeverLands(r) => {
                // Another draw at its attempt (`BadAttempt` for ever), or
                // none at or past its beacon height (`BeaconNotFuture`).
                match &r.record {
                    Some(x) if ours(x) => {
                        return Err(ObserveError::NoReleaseProof("the chain holds the draw"))
                    }
                    None if r.block < bh => {
                        return Err(ObserveError::NoReleaseProof(
                            "the final block is below the draw's beacon height",
                        ))
                    }
                    _ => {}
                }
                if self.opened.contains_key(&key) || self.kept_at(&fact, attempt) {
                    return Err(ObserveError::NoReleaseProof("the draw was opened here"));
                }
                // Never on chain: its attempt is free again (L2RND-R2-F5).
                self.announcements.remove(&key);
                self.draws.insert(fact, attempt);
            }
            Release::Unfixable(r) => {
                let ok = r.block > bh.saturating_add(BLOCKHASH_WINDOW)
                    && r.record
                        .as_ref()
                        .is_some_and(|x| ours(x) && x.beacon == Hash32::ZERO);
                if !ok {
                    return Err(ObserveError::NoReleaseProof(
                        "no final block past the blockhash window showing the draw's beacon zero",
                    ));
                }
                if self.kept_at(&fact, attempt) {
                    return Err(ObserveError::NoReleaseProof("the draw's round is kept"));
                }
                // Counted (it was announced on chain): no beacon, no
                // committee; for a dated fact a round without a value.
                self.released.insert(key);
                if dated {
                    let n = self.non_value.entry(fact).or_insert(0);
                    *n = n.saturating_add(1);
                    if *n >= self.non_value_cap {
                        self.closed.insert(fact);
                    }
                }
            }
            Release::NeverSent => {
                if self.opened.contains_key(&key) || self.kept_at(&fact, attempt) {
                    return Err(ObserveError::NoReleaseProof("the draw was opened here"));
                }
                self.announcements.remove(&key);
                self.draws.insert(fact, attempt);
            }
        }
        Ok(())
    }

    /// Give up the book's draw of `spec` (it cannot open: a drawn
    /// observer's bond is committed elsewhere, too few eligible members, its
    /// beacon block left the `blockhash` window) only under the draw
    /// invariant ([`Self::release_draw`]): ONE final-block read
    /// ([`DrawChain::draw_of_final`], retried per `wait`) must show it can
    /// never land, or never have its beacon fixed. Otherwise Err and the
    /// draw stays in the book.
    pub fn abandon_draw(
        &mut self,
        spec: &ObservationSpec,
        chain: &dyn DrawChain,
        wait: &ChainWait,
    ) -> Result<(), String> {
        let draw = self
            .held_draw(spec)
            .ok_or_else(|| ObserveError::DrawNotAnnounced.to_string())?;
        let proof = gone_proof(chain, &draw, wait)?;
        self.release_draw(spec, proof).map_err(|e| e.to_string())
    }

    /// The fact's latest draw if it is announced and not opened (review
    /// REVLOG-R5): a run stopped between its announcement and the opening of
    /// its round. [`draw_and_open`] resumes it rather than announce another.
    pub fn pending_draw(&self, spec: &ObservationSpec) -> Option<OnchainDraw> {
        let (fact, _) = self.fact_of(spec).ok()?;
        let attempt = self.held(&fact)?;
        if self.opened.contains_key(&(fact, attempt)) {
            return None;
        }
        self.latest_draw(spec).ok()
    }

    /// The pool snapshot the fact's latest announcement fixed (review
    /// REVLOG-R5: a resumed draw opens with it).
    pub fn announced_pool(&self, spec: &ObservationSpec) -> Result<Vec<Observer>, ObserveError> {
        let (fact, _) = self.fact_of(spec)?;
        let attempt = self
            .draws
            .get(&fact)
            .copied()
            .unwrap_or(0)
            .checked_sub(1)
            .ok_or(ObserveError::DrawNotAnnounced)?;
        self.announcements
            .get(&(fact, attempt))
            .map(|a| a.pool.clone())
            .ok_or(ObserveError::DrawNotAnnounced)
    }

    /// The on-chain arguments of the fact's latest announced draw.
    pub fn latest_draw(&self, spec: &ObservationSpec) -> Result<OnchainDraw, ObserveError> {
        let (fact, _) = self.fact_of(spec)?;
        let attempt = self
            .draws
            .get(&fact)
            .copied()
            .unwrap_or(0)
            .checked_sub(1)
            .ok_or(ObserveError::DrawNotAnnounced)?;
        let ann = self
            .announcements
            .get(&(fact, attempt))
            .ok_or(ObserveError::DrawNotAnnounced)?;
        Ok(OnchainDraw {
            fact: self.onchain_fact(spec)?,
            attempt,
            commit: draw_commit(&pool_hash(&ann.pool), &ann.spec_id, &cfg_hash(&ann.cfg)),
            beacon_height: ann.beacon_height,
        })
    }

    /// The fact key a spec's round settles (dated or not, by its template).
    pub(super) fn fact_key_of(&self, spec: &ObservationSpec) -> Result<Hash32, ObserveError> {
        Ok(if self.template_of(spec)?.is_dated() {
            fact_key_dated(spec)
        } else {
            fact_key(spec)
        })
    }

    /// Keep a round a run opened here and could not finish (review
    /// REVLOG-R3-2): it is saved with the book ([`Self::save_book`]), no new
    /// draw is announced for its fact ([`ObserveError::RoundUnfinished`]),
    /// and the next run takes a copy ([`Self::kept_round`]) to finish
    /// it on the same draw. Refused for a round not opened here. A round
    /// settled here is kept too (review REVLOG-R4-1): it stays until its
    /// results are written ([`Self::published`]).
    pub fn keep_unfinished(&mut self, u: UnfinishedRound) -> Result<(), ObserveError> {
        // Review R8: the fact key, whatever the fact's state (closed by the
        // cap, settled): a round of the draw the book holds is always kept.
        let fact = self.fact_key_of(&u.round.spec)?;
        let key = (fact, u.round.draw.attempt);
        // A round settled and published already (a retried finish, review
        // REVLOG-R3-3) has left the book for good: nothing to keep.
        if self.released.contains(&key)
            && self.opened.get(&key) == Some(&u.round.round_id)
            && self.rounds.contains(&u.round.round_id)
        {
            return Ok(());
        }
        if self.held(&fact) != Some(u.round.draw.attempt)
            || self.opened.get(&key) != Some(&u.round.round_id)
        {
            return Err(ObserveError::DrawNotAnnounced);
        }
        self.failed.remove(&fact);
        self.unfinished.insert(fact, u);
        Ok(())
    }

    /// Keep a round that failed before any reveal as a failed attempt
    /// waiting to be published (THREATS REVLOG-R4-5, ATT-R2-F3): saved with
    /// the book like an unfinished round (its draw gets no successor), but
    /// the next run publishes it as a failed attempt instead of finishing
    /// it. [`Self::attempt_published`] drops it once it is written.
    pub fn keep_failed(&mut self, u: UnfinishedRound) -> Result<(), ObserveError> {
        let fact = self.fact_key_of(&u.round.spec)?;
        self.keep_unfinished(u)?;
        self.failed.insert(fact);
        Ok(())
    }

    /// The kept failed attempt of `spec`'s fact, waiting to be published.
    pub fn failed_attempt(&self, spec: &ObservationSpec) -> Option<&UnfinishedRound> {
        let fact = self.fact_key_of(spec).ok()?;
        self.failed
            .contains(&fact)
            .then(|| self.unfinished.get(&fact))
            .flatten()
    }

    /// The failed attempt `round_id` of `spec`'s fact is durably written
    /// (published): drop it from the book (THREATS REVLOG-R4-5). Until then
    /// it stays kept, and the book on disk holds it, so a crash before the
    /// write publishes it again; idempotent.
    pub fn attempt_published(&mut self, spec: &ObservationSpec, round_id: &Hash32) {
        if self
            .failed_attempt(spec)
            .is_some_and(|u| u.round.round_id == *round_id)
        {
            // Only fails for another round: then nothing is released.
            let _ = self.release_draw(spec, Release::Published(*round_id));
        }
    }

    /// The round `round_id` of `spec`'s fact, settled here, has its results
    /// durably written (published): drop it from the book (review
    /// REVLOG-R4-1). Until then a settled round stays in the store, and
    /// [`Self::save_book`] refuses to write the book, so the book on disk
    /// keeps it UNSETTLED: a crash before the results are written makes the
    /// next run finish and publish the same round on the same draw (its
    /// receipts are on chain), never announce a new draw. Refused for a
    /// round not settled here.
    pub fn published(
        &mut self,
        spec: &ObservationSpec,
        round_id: &Hash32,
    ) -> Result<(), ObserveError> {
        if !self.rounds.contains(round_id) {
            return Err(ObserveError::RoundNotSettled);
        }
        let fact = self.fact_key_of(spec)?;
        // Idempotent: a round already released (or of an earlier draw)
        // changes nothing.
        let ours = self.held(&fact).is_some_and(|a| {
            self.opened.get(&(fact, a)) == Some(round_id)
                || self.kept_at(&fact, a)
                    && self.unfinished.get(&fact).map(|u| u.round.round_id) == Some(*round_id)
        });
        if ours {
            self.release_draw(spec, Release::Published(*round_id))?;
        }
        Ok(())
    }

    /// Whether the round `round_id` is settled in this store (its result is
    /// fixed: [`keep_taking_reveals`] takes no more reveals for it). A book
    /// never holds a round settled: one read back from it is unsettled
    /// (review REVLOG-R4-1).
    pub fn is_settled(&self, round_id: &Hash32) -> bool {
        self.rounds.contains(round_id)
    }

    /// Whether a kept round is settled here and waits for its results to be
    /// written ([`Self::published`], review REVLOG-R4-1).
    pub fn awaiting_publication(&self) -> bool {
        self.unfinished
            .values()
            .any(|u| self.rounds.contains(&u.round.round_id))
    }

    /// Why the book cannot be written without losing a draw it holds (the
    /// draw invariant, review R8), if so: a held draw that is not its
    /// fact's latest (a book holds one draw per fact), or a held draw whose
    /// round settled here and is neither kept nor published. `None`: every
    /// held draw is written ([`Self::non_value_book`]: kept rounds as
    /// unfinished, every other held draw as announced).
    pub(super) fn unwritable_draw(&self) -> Option<String> {
        self.announcements.keys().find_map(|key @ (fact, a)| {
            if self.released.contains(key) {
                return None;
            }
            if self.held(fact) != Some(*a) {
                return Some(format!("draw {a} is held but is not its fact's latest"));
            }
            let settled = self
                .opened
                .get(key)
                .is_some_and(|r| self.rounds.contains(r));
            (settled && !self.kept_at(fact, *a))
                .then(|| format!("draw {a}'s round settled here, is not kept and is not published"))
        })
    }

    /// The unfinished round of `spec`'s fact, if one is kept.
    pub fn unfinished(&self, spec: &ObservationSpec) -> Option<&UnfinishedRound> {
        self.unfinished.get(&self.fact_key_of(spec).ok()?)
    }

    /// A copy of the unfinished round of `spec`'s fact (review
    /// REVLOG-R3-2), to finish it on its draw. The book keeps it (review
    /// R8: nothing but [`Self::release_draw`] drops a draw's round) until
    /// its results are written ([`Self::published`]); a newer state of it
    /// replaces it ([`Self::keep_unfinished`]).
    pub fn kept_round(&self, spec: &ObservationSpec) -> Option<UnfinishedRound> {
        self.unfinished.get(&self.fact_key_of(spec).ok()?).cloned()
    }

    /// [`Self::settle`], idempotent for a retry (review REVLOG-R3-3): a round
    /// already settled here with the same result (the same settlement,
    /// recomputed) returns the recorded settlement; one whose result would
    /// differ (different reveals) stays [`ObserveError::AlreadySettled`].
    pub fn settle_once(
        &mut self,
        round: &ObservationRound,
        now: u64,
    ) -> Result<ObservationSettlement, ObserveError> {
        if !self.rounds.contains(&round.round_id) {
            return self.settle(round, now);
        }
        let again = round.settle(now)?;
        match self.by_result.get(&again.result_hash()) {
            Some((_, s)) if *s == again => Ok(s.clone()),
            _ => Err(ObserveError::AlreadySettled),
        }
    }

    /// Put a saved unfinished round back (review REVLOG-R3-2), re-checked:
    /// its feed is this store's registered template, its round is the draw
    /// it names ([`ObservationRound::restore`]) and that draw is the one the
    /// registry recorded ([`check_commit_allowed`]). Its announcement and
    /// opening are restored, and its observers' slashes held again.
    fn restore_unfinished(&mut self, w: UnfinishedWire) -> Result<Hash32, ObserveError> {
        let round = ObservationRound::restore(w.round)?;
        let (fact, _) = self.fact_of(&round.spec)?;
        // ROADMAP C52: a time-locked snapshot's far round is the one the
        // registered template gives this round (its dismissals were checked
        // against the far round the book names, `check_dismissals`).
        if let Some(t) = round.sealed.as_ref().and_then(|s| s.timelock.as_ref()) {
            let want = crate::timelock::LockRounds::of_round(
                &crate::timelock::BeaconChain::quicknet(),
                self.template_of(&round.spec)?,
                &round.spec,
                round.cfg.reveal_close,
            )
            .map_err(|_| ObserveError::BadSealedSnapshot("no far round for this round"))?;
            if want.far() != t.far_round {
                return Err(ObserveError::BadSealedSnapshot(
                    "a far round that is not the registered template's",
                ));
            }
        }
        check_commit_allowed(
            &round.draw,
            &draw_commit(&round.draw.pool, &round.spec_id, &cfg_hash(&round.cfg)),
            &w.onchain_draw,
        )?;
        let key = (fact, round.draw.attempt);
        if let Some(a) = self.announcements.get(&key) {
            if a.spec_id != round.spec_id
                || a.cfg != round.cfg
                || a.pool != round.pool
                || a.beacon_height != round.draw.beacon_height
            {
                return Err(ObserveError::DrawMismatch);
            }
        } else {
            self.announcements.insert(
                key,
                Announcement {
                    spec_id: round.spec_id,
                    cfg: round.cfg,
                    pool: round.pool.clone(),
                    beacon_height: round.draw.beacon_height,
                    full_bond: round.pool.iter().map(|o| (o.address, o.bond)).collect(),
                },
            );
        }
        if self.opened.insert(key, round.round_id).is_none() {
            for o in &round.observers {
                let e = self.exposure.entry(o.address).or_insert(0);
                *e = e.saturating_add(round.cfg.outlier_slash);
            }
        }
        let d = self.draws.entry(fact).or_insert(0);
        *d = (*d).max(round.draw.attempt.saturating_add(1));
        self.unfinished.insert(
            fact,
            UnfinishedRound {
                round,
                onchain_draw: w.onchain_draw,
                reveals: w.reveals,
            },
        );
        Ok(fact)
    }

    /// Put a saved announced, unopened draw back (review REVLOG-R5): its
    /// announcement and its count. Opening it later checks it against the
    /// runner's spec, config and pool and the registry's record.
    fn restore_announced(&mut self, w: AnnouncedWire) -> Result<Hash32, ObserveError> {
        let key = (w.fact, w.attempt);
        if self.opened.contains_key(&key) || self.announcements.contains_key(&key) {
            return Err(ObserveError::BadNonValueBook(
                "an announced draw that is also opened or announced twice".into(),
            ));
        }
        check_pool(&w.pool)?;
        self.announcements.insert(
            key,
            Announcement {
                spec_id: w.spec_id,
                cfg: w.cfg,
                pool: w.pool,
                beacon_height: w.beacon_height,
                full_bond: w.full_bond.into_iter().collect(),
            },
        );
        let d = self.draws.entry(w.fact).or_insert(0);
        *d = (*d).max(w.attempt.saturating_add(1));
        Ok(w.fact)
    }

    /// Write the book ([`non_value_book`](Self::non_value_book)) to `path`
    /// atomically (AUD-2-F9, AUD-2R-F4): a temporary file in the same folder,
    /// synced, renamed over the old book, and the folder synced. A crash
    /// leaves the old book or the new one, never a torn file. Refused while
    /// a round settled here waits for its results to be written (review
    /// REVLOG-R4-1): the book on disk keeps that round unsettled until
    /// [`Self::published`], so the next run publishes it again rather than
    /// count it twice or lose it.
    pub fn save_book(&self, path: &std::path::Path) -> Result<(), String> {
        let book = self
            .non_value_book()
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let text = serde_json::to_string_pretty(&book).map_err(|e| e.to_string())?;
        write_atomic(path, &format!("{text}\n"))
    }

    /// Read the book at `path` into the store, if the file exists (AUD-2-F9).
    /// A missing file is a first run; a file that does not parse (a torn or
    /// edited book) is an error that stops the start: counts are never
    /// silently reset (AUD-2R-F4).
    pub fn load_book(&mut self, path: &std::path::Path) -> Result<(), String> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        let book: serde_json::Value = serde_json::from_str(&text).map_err(|e| {
            format!(
                "{}: the book does not parse ({e}); refusing to start rather than reset its counts",
                path.display()
            )
        })?;
        self.absorb_non_value_book(&book)
            .map_err(|e| format!("{}: {e}", path.display()))
    }

    /// Announce a round's draw (L2-F2), before its beacon exists: fixes the
    /// spec, config, pool snapshot and `beacon_height` (above `chain_height`,
    /// the deployment chain's current height). The snapshot's bonds are each
    /// member's bond minus the slashes it already has at stake, so a bond is
    /// never counted twice. The attempt is used here (L2RND-R1-F3): the book
    /// ([`to_json`](Self::to_json)) counts it from now on, so a coordinator
    /// that saw the beacon and restarts cannot draw the same attempt again;
    /// persist the book before the beacon height. The coordinator also
    /// announces the draw on chain (`OnyxRegistry.announceDraw`) before the
    /// beacon height; consumers check that announcement (`drawMatches`).
    pub fn announce_draw(
        &mut self,
        spec: &ObservationSpec,
        cfg: &ObserveConfig,
        members: &[Observer],
        beacon_height: u64,
        chain: &dyn ChainView,
    ) -> Result<Vec<Observer>, ObserveError> {
        let (fact, attempt) = self.fact_attempt(spec)?;
        // Review REVLOG-R3-2: a fact whose round is unfinished finishes that
        // round on its draw; a new draw would burn it and void the fact.
        if self.unfinished.contains_key(&fact) {
            return Err(ObserveError::RoundUnfinished);
        }
        // The draw invariant (review R8): the book's previous draw must have
        // left it under a proof ([`Self::release_draw`]) or settled here;
        // skipping a held draw would lose it.
        if attempt > 0 && self.blocks_next(&fact, attempt - 1) {
            return Err(ObserveError::DrawAlreadyAnnounced);
        }
        let chain_height = chain.height().map_err(ObserveError::Chain)?;
        if beacon_height <= chain_height {
            return Err(ObserveError::BeaconNotFuture);
        }
        let pool = self.pool_snapshot(spec, cfg, members)?;
        let full_bond = members.iter().map(|m| (m.address, m.bond)).collect();
        self.announcements.insert(
            (fact, attempt),
            Announcement {
                spec_id: spec.id(),
                cfg: *cfg,
                pool: pool.clone(),
                beacon_height,
                full_bond,
            },
        );
        self.draws.insert(fact, attempt + 1);
        Ok(pool)
    }

    /// The pool snapshot a draw of `spec` announces now: each member's bond
    /// minus the slashes it already has at stake ([`Self::announce_draw`]).
    fn pool_snapshot(
        &self,
        spec: &ObservationSpec,
        cfg: &ObserveConfig,
        members: &[Observer],
    ) -> Result<Vec<Observer>, ObserveError> {
        check_pool(members)?;
        cfg.validate(spec)?;
        Ok(members
            .iter()
            .map(|m| Observer {
                address: m.address,
                bond: m
                    .bond
                    .saturating_sub(self.exposure.get(&m.address).copied().unwrap_or(0)),
            })
            .collect())
    }

    /// The book's draw `prev` keeps the fact from a next draw (the draw
    /// invariant, review R8): announced, not released and not settled here.
    fn blocks_next(&self, fact: &Hash32, prev: u32) -> bool {
        let key = (*fact, prev);
        self.announcements.contains_key(&key)
            && !self.released.contains(&key)
            && !self
                .opened
                .get(&key)
                .is_some_and(|r| self.rounds.contains(r))
    }

    /// The attempt the fact's next announcement would take: the book's draw
    /// count (review R6, REVLOG-R5-2).
    pub fn next_attempt(&self, spec: &ObservationSpec) -> Result<u32, ObserveError> {
        Ok(self.fact_attempt(spec)?.1)
    }

    /// Review R9-F1: `draws`, the draws the results store publishes for
    /// `spec`'s fact (a round or a failed attempt each, as written), read
    /// from the results store by the runner before the run. A book that
    /// lacks a chain draw (a lost book) must never adopt one whose round is
    /// already published: the round it would open is a second, different
    /// entry for that draw, which the results store refuses, and the draw
    /// after it (fixable by anyone) would be left without a published
    /// round. Without this call no chain draw of the fact is adopted
    /// ([`Self::adopt_draw`]). Not saved with the book: the results store
    /// is the authority, read again by every run. Replaces an earlier note.
    pub fn note_published_draws(
        &mut self,
        spec: &ObservationSpec,
        draws: &[DrawSeed],
    ) -> Result<(), ObserveError> {
        let fact = self.fact_key_of(spec)?;
        let mut d = draws.to_vec();
        d.sort_by_key(|x| x.attempt);
        self.results_read.insert(fact, d);
        Ok(())
    }

    /// The draw at `attempt` the results store publishes for `spec`'s fact
    /// ([`Self::note_published_draws`]): `None` when the results store was
    /// not read, `Some(None)` when it publishes no draw at `attempt`.
    pub fn published_draw(&self, spec: &ObservationSpec, attempt: u32) -> Option<Option<DrawSeed>> {
        let fact = self.fact_key_of(spec).ok()?;
        let p = self.results_read.get(&fact)?;
        Some(p.iter().find(|d| d.attempt == attempt).copied())
    }

    /// Whether [`Self::adopt_draw`] would adopt `rec` now (nothing changes).
    pub fn adoptable(
        &self,
        spec: &ObservationSpec,
        cfg: &ObserveConfig,
        members: &[Observer],
        rec: &DrawRecord,
    ) -> Result<bool, ObserveError> {
        let (fact, attempt) = self.fact_attempt(spec)?;
        if rec.attempt != attempt
            || self.unfinished.contains_key(&fact)
            || (attempt > 0 && self.blocks_next(&fact, attempt - 1))
        {
            return Ok(false);
        }
        // Review R9-F1: never a draw the results store publishes (its round
        // is settled history), and never without the results store read.
        if self
            .results_read
            .get(&fact)
            .is_none_or(|p| p.iter().any(|d| d.attempt == attempt))
        {
            return Ok(false);
        }
        let pool = self.pool_snapshot(spec, cfg, members)?;
        Ok(draw_commit(&pool_hash(&pool), &spec.id(), &cfg_hash(cfg)) == rec.commit)
    }

    /// Adopt the registry's draw `rec` that the book lacks (THREATS
    /// REVLOG-R5-2: the process died after `announceDraw` landed and before
    /// the book was first saved), as [`Self::announce_draw`] would have
    /// recorded it, when it is this book's next attempt, the results store
    /// was read and publishes no draw at that attempt (review R9-F1,
    /// [`Self::note_published_draws`]), and its commitment equals
    /// [`draw_commit`] of the pool snapshot, spec and config this runner
    /// announces now; the beacon height is the registry's. `Ok(false)`
    /// (nothing changed) for any other draw. The commitment binds every input
    /// but the beacon, which the chain chose after the announcement, so
    /// adopting gives the coordinator no new choice (L2RND-R1-F3).
    /// [`draw_and_open`] adopts only a record read at ONE final block that
    /// shows it as the chain's last draw.
    pub fn adopt_draw(
        &mut self,
        spec: &ObservationSpec,
        cfg: &ObserveConfig,
        members: &[Observer],
        rec: &DrawRecord,
    ) -> Result<bool, ObserveError> {
        if !self.adoptable(spec, cfg, members, rec)? {
            return Ok(false);
        }
        let (fact, attempt) = self.fact_attempt(spec)?;
        let pool = self.pool_snapshot(spec, cfg, members)?;
        let full_bond = members.iter().map(|m| (m.address, m.bond)).collect();
        self.announcements.insert(
            (fact, attempt),
            Announcement {
                spec_id: spec.id(),
                cfg: *cfg,
                pool,
                beacon_height: rec.beacon_height,
                full_bond,
            },
        );
        self.draws.insert(fact, attempt + 1);
        Ok(true)
    }

    /// The slash amount `observer` has at stake in open rounds.
    pub fn exposure(&self, observer: &MinerAddress) -> u128 {
        self.exposure.get(observer).copied().unwrap_or(0)
    }

    /// Settle `round` at `now` and record its result. The only way to settle
    /// a round (`ObservationRound::settle` is crate-private, L2R3-F5). Refuses
    /// a round settled before, so its entries are produced once, and a second
    /// round for a fact already settled here (same metric, subject and window
    /// end, whatever the other spec fields, L2R4-F1), so a coordinator cannot
    /// run competing observer sets or competing feeds for one fact and keep
    /// the result it likes. One store is one coordinator's book; across
    /// coordinators `OnyxRegistry` records one result per fact on chain
    /// (ADR-0030).
    ///
    /// For a dated or period subject (one fact per date, whatever the window)
    /// only a round that settles a value consumes the fact (SEC-R1-F1): a
    /// `SourceUnavailable` or `NoResult` round (the source blocked the
    /// observers, a late filer had not filed yet, a disagreeing amended row)
    /// leaves the date open, so a later round can settle the real value. The
    /// first value still wins for good. A fixed subject's fact is its window,
    /// which no later round can observe again, so any outcome closes it. A
    /// dated fact takes at most [`MAX_NON_VALUE_ROUNDS`] rounds without a value
    /// (the store's cap) and is then closed until a reviewer reopens it
    /// (SEC-R2-F1): otherwise a blocking minority could force re-draws until a
    /// colluding majority settles.
    pub fn settle(
        &mut self,
        round: &ObservationRound,
        now: u64,
    ) -> Result<ObservationSettlement, ObserveError> {
        if self.rounds.contains(&round.round_id) {
            return Err(ObserveError::AlreadySettled);
        }
        let dated = self.template_of(&round.spec)?.is_dated();
        let fact = if dated {
            fact_key_dated(&round.spec)
        } else {
            fact_key(&round.spec)
        };
        if self.facts.contains(&fact) {
            return Err(ObserveError::FeedAlreadySettled);
        }
        if self.closed.contains(&fact) {
            return Err(ObserveError::FactClosed);
        }
        // Only a round opened here from its announced draw settles (L2-F2).
        let key = (fact, round.draw.attempt);
        let ann = self
            .announcements
            .get(&key)
            .ok_or(ObserveError::DrawNotAnnounced)?;
        if self.opened.get(&key) != Some(&round.round_id)
            || ann.spec_id != round.spec_id
            || ann.cfg != round.cfg
            || ann.pool != round.pool
            || ann.beacon_height != round.draw.beacon_height
        {
            return Err(ObserveError::DrawMismatch);
        }
        let s = round.settle(now)?;
        self.rounds.insert(round.round_id);
        // Review REVLOG-R4-1: a kept round stays kept until its results are
        // written ([`Self::published`]).
        for o in &round.observers {
            if let Some(e) = self.exposure.get_mut(&o.address) {
                *e = e.saturating_sub(round.cfg.outlier_slash);
            }
        }
        if !dated || matches!(s.outcome(), ObservationOutcome::Value(_)) {
            self.facts.insert(fact);
        } else {
            // A dated fact stays open after a round without a value, but only
            // `non_value_cap` times (SEC-R2-F1): then a reviewer must reopen it.
            // The next round is a new draw (L2-F2); its attempt was already
            // counted when it was announced (L2RND-R1-F3).
            let n = self.non_value.entry(fact).or_insert(0);
            *n = n.saturating_add(1);
            if *n >= self.non_value_cap {
                self.closed.insert(fact);
            }
        }
        self.by_result
            .insert(s.result_hash(), (round.spec.clone(), s.clone()));
        self.draw_of_result
            .insert(s.result_hash(), (round.draw.pool, cfg_hash(&round.cfg)));
        Ok(s)
    }

    /// The arguments of `OnyxRegistry.recordObservation` for the round settled
    /// under `result`, or `None` when it settled no value (SEC-R1-F1). The
    /// contract cannot see an outcome, only a result hash, so the coordinator
    /// records value outcomes only: a non-value record would occupy the fact
    /// key on chain. It could never mislead a consumer (`onchain_observation_check`
    /// refuses a transcript without a value), but it would block the date.
    pub fn onchain_record(&self, result: &Hash32) -> Option<OnchainObservationCheck> {
        let (spec, s) = self.by_result.get(result)?;
        if !matches!(s.outcome(), ObservationOutcome::Value(_)) {
            return None;
        }
        let (draw_pool, cfg_hash) = *self.draw_of_result.get(result)?;
        Some(OnchainObservationCheck {
            metric_key: onyx_core::onchain::metric_key(&spec.metric),
            subject_key: onyx_core::onchain::subject_key(&spec.subject),
            window_end: spec.window_end,
            spec_id: spec.id(),
            result: s.result_hash(),
            draw_pool,
            cfg_hash,
        })
    }

    /// The settlement recorded under `result`, if this coordinator settled it.
    pub fn get(&self, result: &Hash32) -> Option<&ObservationSettlement> {
        self.by_result.get(result).map(|(_, s)| s)
    }

    /// Accept a claim: it must validate against the registry, and a level-2
    /// claim must be backed by a result settled in this store and bound to
    /// that result's feed in every field a settled value could be reused for
    /// (L2R3-F1):
    /// - `method`: this feed's id and this result's hash, a round that settled
    ///   a value, and `value` equal to that value (`backs`);
    /// - `metric` and `subject`: exactly the feed's;
    /// - `source`: the feed's source id, snapshot `Document { published_unix:
    ///   window_end, content_hash: result_hash }` (the value is "as of" the end
    ///   of the window, and the snapshot names this result);
    /// - `rights`: exactly the feed's;
    /// - `label_set`: none (a level-2 value depends on no label set);
    /// - `evidence`: empty (the result hash is the evidence);
    /// - `confidence`: none (objective lane, already refused by `validate`).
    ///
    /// Returns `ProofLevel::ReObserved`. This is the level-2 intake only: a
    /// claim of any other method is refused with `NotLevelTwo` (L2R4-F4) and
    /// goes through its own lane (recompute, attestation, scoring), so this
    /// function never vouches for a level it did not check.
    pub fn accept_claim(
        &self,
        claim: &Claim,
        reg: &dyn Registry,
    ) -> Result<ProofLevel, ObserveError> {
        claim.validate(reg)?;
        let VerificationMethod::ReObserved { result, .. } = &claim.method else {
            return Err(ObserveError::NotLevelTwo);
        };
        {
            let (feed, s) = self
                .by_result
                .get(result)
                .ok_or(ObserveError::ClaimNotBacked(
                    "no round settled with this result",
                ))?;
            s.backs(feed, &claim.method, &claim.value)?;
            // The feed is still its metric's registered template.
            self.template_of(feed)?;
            if claim.metric != feed.metric {
                return Err(ObserveError::ClaimNotBacked("metric is not the feed's"));
            }
            if claim.subject != feed.subject {
                return Err(ObserveError::ClaimNotBacked("subject is not the feed's"));
            }
            if claim.source.source != feed.rights.source {
                return Err(ObserveError::ClaimNotBacked("source is not the feed's"));
            }
            let snapshot = Snapshot::Document {
                published_unix: feed.window_end,
                content_hash: s.result_hash(),
            };
            if claim.source.snapshot != snapshot {
                return Err(ObserveError::ClaimNotBacked(
                    "snapshot must be the feed's window end and this result",
                ));
            }
            if claim.rights != feed.rights {
                return Err(ObserveError::ClaimNotBacked(
                    "rights differ from the feed's",
                ));
            }
            if claim.label_set.is_some() {
                return Err(ObserveError::ClaimNotBacked(
                    "a level-2 value depends on no label set",
                ));
            }
            if !claim.evidence.is_empty() {
                return Err(ObserveError::ClaimNotBacked(
                    "a level-2 claim carries no evidence of its own",
                ));
            }
        }
        Ok(ProofLevel::ReObserved)
    }

    /// Decode a claim from canonical bytes and accept it (the level-2 ingest
    /// path; `Claim::decode` refuses level-2 claims, L2R3-F3).
    pub fn decode_claim(&self, bytes: &[u8], reg: &dyn Registry) -> Result<Claim, ObserveError> {
        let claim: Claim =
            from_canonical_bytes(bytes).map_err(|_: CanonicalError| ObserveError::NotCanonical)?;
        self.accept_claim(&claim, reg)?;
        Ok(claim)
    }
}
