//! One observation round's methods: open, commits, reveals and the settle with its judgments (the
//! round itself is [`ObservationRound`] in observe.rs).

use super::*;

impl ObservationRound {
    /// Open a round. Refuses a malformed or forbidden feed, bad timing, and
    /// fewer observers than the feed needs. Crate-private: rounds open through
    /// [`ObservationResults::open_round`], which checks the feed template.
    pub(crate) fn open(
        spec: ObservationSpec,
        cfg: ObserveConfig,
        pool: Vec<Observer>,
        draw: DrawSeed,
    ) -> Result<Self, ObserveError> {
        check_pool(&pool)?;
        if pool_hash(&pool) != draw.pool || draw.attempt >= MAX_DRAW_ATTEMPTS {
            return Err(ObserveError::BadPool);
        }
        let spec_id = spec.id();
        let observers = draw_observers(
            &spec_id,
            &pool,
            &draw,
            usize::from(spec.min_observers),
            cfg.outlier_slash,
        );
        if observers.is_empty() {
            return Err(ObserveError::TooFewObservers {
                have: pool.iter().filter(|o| o.bond >= cfg.outlier_slash).count(),
                need: spec.min_observers,
            });
        }
        check_round_params(&spec, &cfg, &observers)?;
        let round_id = round_id(&spec_id, &cfg, &observers, &draw);
        Ok(ObservationRound {
            spec,
            spec_id,
            round_id,
            cfg,
            pool,
            draw,
            observers,
            commits: BTreeMap::new(),
            reveals: BTreeMap::new(),
            log: None,
            sealed: None,
        })
    }

    /// A saved round (review REVLOG-R3-2), re-checked as if it were opened
    /// and played again: the observers are the draw of the pool, the ids
    /// are recomputed, every commit and reveal carries its observer's
    /// signature, every reveal opens its commit and passes the feed's
    /// checks, and a log snapshot names a block past `commit_close` and
    /// holds only drawn observers in order.
    pub(super) fn restore(w: RoundWire) -> Result<Self, ObserveError> {
        let bad = ObserveError::BadUnfinished;
        check_draw(&w.spec, &w.cfg, &w.pool, &w.draw, &w.observers)?;
        let spec_id = w.spec.id();
        if w.spec_id != spec_id || w.round_id != round_id(&spec_id, &w.cfg, &w.observers, &w.draw) {
            return Err(bad("ids do not match the feed and draw"));
        }
        check_round_params(&w.spec, &w.cfg, &w.observers)?;
        let r = ObservationRound {
            spec: w.spec,
            spec_id,
            round_id: w.round_id,
            cfg: w.cfg,
            pool: w.pool,
            draw: w.draw,
            observers: w.observers,
            commits: w.commits,
            reveals: w.reveals,
            log: w.log,
            sealed: None,
        };
        for (a, (h, sig)) in &r.commits {
            if !r.is_observer(a) || !commit_signed(&r.round_id, r.draw.attempt, a, h, sig) {
                return Err(bad("a commit not signed by a drawn observer"));
            }
        }
        for (a, (o, salt, sig)) in &r.reveals {
            let (h, _) = r.commits.get(a).ok_or(bad("a reveal without a commit"))?;
            if observation_commit(&r.round_id, a, o, salt) != *h
                || !reveal_signed(&r.round_id, r.draw.attempt, a, o, salt, sig)
            {
                return Err(bad("a reveal that does not open its signed commit"));
            }
            o.check(&r.spec)?;
        }
        match &r.log {
            None if !r.reveals.is_empty() => {
                return Err(bad("reveals without a log snapshot"));
            }
            Some(l) => r.check_snapshot(l)?,
            None => {}
        }
        // ROADMAP C11: a saved sealed snapshot is re-checked as when it was
        // taken (shape, key proofs, every dismissal, every in-phase post
        // revealed or dismissed); its reveals are among `reveals`, checked above.
        let mut r = r;
        if let Some(snap) = w.sealed {
            if r.log.is_none() {
                return Err(bad("a sealed snapshot without a log snapshot"));
            }
            r.check_sealed(&snap)?;
            r.sealed = Some(snap);
        }
        Ok(r)
    }

    /// A sealed snapshot's shape (ROADMAP C11): read at a block past
    /// `reveal_close`, slots of drawn observers in order, each post's stored
    /// key proof valid for this round (the registry never holds another,
    /// C4-R3-2), dismissals in order, of in-phase posts only, each checking
    /// against the snapshot's reveal key, of observers with no reveal; and every
    /// in-phase post accounted for, as a reveal the round holds or a dismissal.
    fn check_sealed(&self, snap: &SealedSnapshot) -> Result<(), ObserveError> {
        self.check_sealed_slots(snap)?;
        self.check_dismissals(snap)
    }

    /// [`Self::check_sealed`]'s first half: block, slots and key proofs.
    fn check_sealed_slots(&self, snap: &SealedSnapshot) -> Result<(), ObserveError> {
        let bad = ObserveError::BadSealedSnapshot;
        if snap.block.time <= self.cfg.reveal_close {
            return Err(bad("the block is not past reveal close"));
        }
        if !snap.posts.windows(2).all(|w| w[0].0 < w[1].0)
            || !snap.posts.iter().all(|(a, _)| self.is_observer(a))
        {
            return Err(bad("slots of observers not drawn, or out of order"));
        }
        for (a, p) in &snap.posts {
            if !crate::sealed::key_proof_valid(
                &p.ephemeral,
                &self.round_id,
                self.draw.attempt,
                a,
                &p.key_proof,
            ) {
                return Err(bad(
                    "a post whose key proof fails (not one the registry holds)",
                ));
            }
        }
        Ok(())
    }

    /// [`Self::check_sealed`]'s second half: every dismissal, and every
    /// in-phase post accounted for.
    fn check_dismissals(&self, snap: &SealedSnapshot) -> Result<(), ObserveError> {
        let bad = ObserveError::BadSealedSnapshot;
        let (cc, rc) = (self.cfg.commit_close, self.cfg.reveal_close);
        if !snap.dismissals.windows(2).all(|w| w[0].0 < w[1].0) {
            return Err(bad("dismissals out of order"));
        }
        // ROADMAP C51: a time-locked snapshot's lock round is the window's, its
        // signature the pinned chain's, its openings in order and each of a
        // dismissal (checked with it below). ROADMAP C52: its far round comes
        // after the lock round (the store compares it with the template's when
        // it restores a book, `restore_unfinished`).
        let tl = match &snap.timelock {
            None => None,
            Some(t) => {
                let q = crate::timelock::QuicknetTimeLock::pinned()
                    .map_err(|_| bad("the pinned beacon key"))?;
                use crate::timelock::TimeLock as _;
                if crate::timelock::lock_round(q.chain(), rc) != Some(t.lock_round) {
                    return Err(bad("a time-lock read for another lock round"));
                }
                let rounds = crate::timelock::LockRounds::new(t.lock_round, t.far_round)
                    .map_err(|_| bad("a far round that is not after the lock round"))?;
                if !q.verify_signature(t.lock_round, &t.signature) {
                    return Err(bad("a lock-round signature that does not verify"));
                }
                if !t.openings.windows(2).all(|w| w[0].0 < w[1].0)
                    || !t
                        .openings
                        .iter()
                        .all(|(a, _)| snap.dismissals.iter().any(|(d, _)| d == a))
                {
                    return Err(bad("time-lock openings out of order or of no dismissal"));
                }
                Some((q, t, rounds))
            }
        };
        let in_phase: BTreeMap<&MinerAddress, &PostedSealed> =
            snap.in_phase(cc, rc).map(|(a, p)| (a, p)).collect();
        for (a, d) in &snap.dismissals {
            let p = in_phase
                .get(a)
                .ok_or(bad("a dismissal without an in-phase post"))?;
            if self.reveals.contains_key(a) {
                return Err(bad(
                    "a dismissal of an observer whose reveal the round holds",
                ));
            }
            let ctx = crate::sealed::RevealContext {
                round_id: self.round_id,
                attempt: self.draw.attempt,
                spec: &self.spec,
                commit: self.commits.get(a).map(|(h, _)| *h),
            };
            match &tl {
                None => {
                    crate::sealed::check_dismissal_posted(&snap.reveal_key, a, p, &ctx, d)
                        .map_err(|_| bad("a dismissal that does not check"))?;
                }
                Some((q, t, rounds)) => {
                    let opening = t.openings.iter().find(|(o, _)| o == a).map(|(_, o)| *o);
                    crate::timelock::check_timelocked_dismissal(
                        q,
                        &snap.reveal_key,
                        a,
                        p,
                        &ctx,
                        *rounds,
                        Some(&t.signature),
                        &crate::timelock::TimedDismissal {
                            dismissal: *d,
                            opening,
                        },
                    )
                    .map_err(|_| bad("a time-locked dismissal that does not check"))?;
                }
            }
        }
        let dismissed: BTreeSet<&MinerAddress> = snap.dismissals.iter().map(|(a, _)| a).collect();
        if in_phase
            .keys()
            .any(|a| !self.reveals.contains_key(*a) && !dismissed.contains(a))
        {
            return Err(bad("an in-phase post neither revealed nor dismissed"));
        }
        Ok(())
    }

    /// Open the round's sealed posts as read ONCE (ROADMAP C11, ADR-0027
    /// amendment 10 item 5; [`finish_reveals_sealed`] reads them): `posts` is
    /// every drawn observer's filled slot at `block` (its time past
    /// `reveal_close`, at or below the finalized block), `reveal_key` the
    /// draw's `drawRevealKey`, which must be the public point of
    /// `reveal_secret`. Each post in the reveal phase (`commit_close`,
    /// `reveal_close`] of an observer whose reveal the round does not hold yet
    /// (one taken from the spool is kept) is opened through
    /// [`crate::sealed::open_posted`] (key proof re-checked, C4-R3-2): a valid
    /// reveal is taken as arriving at the post's block time; anything else is
    /// dismissed with a proof, kept in the snapshot. A post outside the phase is
    /// ignored (C4-5). Needs the log snapshot (commits absorbed) first; refused
    /// once a sealed snapshot is kept; all or nothing. Returns the observers
    /// revealed and dismissed here.
    pub fn absorb_sealed(
        &mut self,
        block: LogBlock,
        reveal_key: crate::sealed::Point,
        posts: Vec<(MinerAddress, PostedSealed)>,
        reveal_secret: &[u8; 32],
    ) -> Result<(Vec<MinerAddress>, Vec<MinerAddress>), ObserveError> {
        self.absorb_sealed_locked(block, reveal_key, posts, reveal_secret, None)
    }

    /// [`Self::absorb_sealed`] for a draw that may be TIME-LOCKED (ROADMAP C51,
    /// ADR-0027 amendment 14): with `lock` (the signature of the round's
    /// [`crate::timelock::lock_round`], which must verify against the pinned
    /// quicknet key, there is no other way in, and the round's
    /// [`crate::timelock::LockRounds`], whose far round the caller derived from
    /// the registered template, ROADMAP C52) every in-phase post is opened
    /// through [`crate::timelock::open_timelocked`] (`TL ‖ TF ‖ w ‖ body`, the
    /// time-lock kept only when it re-encrypts byte for byte, the far copy
    /// only when it is exactly the unwrapped body key's), and each dismissal
    /// keeps its time-lock opening when the time-lock opened. The snapshot then
    /// carries the read ([`crate::timelock::TimeLockRead`]). `None`: amendment
    /// 10 posts, as [`Self::absorb_sealed`].
    pub fn absorb_sealed_locked(
        &mut self,
        block: LogBlock,
        reveal_key: crate::sealed::Point,
        posts: Vec<(MinerAddress, PostedSealed)>,
        reveal_secret: &[u8; 32],
        lock: Option<(&[u8], crate::timelock::LockRounds)>,
    ) -> Result<(Vec<MinerAddress>, Vec<MinerAddress>), ObserveError> {
        let bad = ObserveError::BadSealedSnapshot;
        if self.sealed.is_some() {
            return Err(ObserveError::SealedAlreadyRead);
        }
        if self.log.is_none() {
            return Err(bad(
                "the transcript log was not read: no sealed post before the commits",
            ));
        }
        if crate::sealed::public_point(reveal_secret).ok() != Some(reveal_key) {
            return Err(bad("the draw's reveal key is not this coordinator's"));
        }
        let mut next = self.clone();
        let (cc, rc) = (self.cfg.commit_close, self.cfg.reveal_close);
        let locked = match lock {
            None => None,
            Some((sig, rounds)) => {
                use crate::timelock::TimeLock as _;
                let q = crate::timelock::QuicknetTimeLock::pinned()
                    .map_err(|_| bad("the pinned beacon key"))?;
                let rho = crate::timelock::lock_round(q.chain(), rc)
                    .ok_or(bad("no lock round for this window"))?;
                if rounds.lock() != rho {
                    return Err(bad("lock rounds for another window"));
                }
                if !q.verify_signature(rho, sig) {
                    return Err(bad("not the pinned chain's signature of the lock round"));
                }
                Some((q, rounds, sig.to_vec()))
            }
        };
        let mut snap = SealedSnapshot {
            block,
            reveal_key,
            posts,
            dismissals: vec![],
            timelock: locked
                .as_ref()
                .map(|(_, rounds, sig)| crate::timelock::TimeLockRead {
                    lock_round: rounds.lock(),
                    far_round: rounds.far(),
                    signature: sig.clone(),
                    openings: vec![],
                }),
        };
        // Shape and key proofs first: no post is opened before every one checks.
        next.check_sealed_slots(&snap)?;
        let mut revealed = vec![];
        let in_phase: Vec<(MinerAddress, PostedSealed)> = snap.in_phase(cc, rc).cloned().collect();
        for (a, p) in in_phase {
            if next.reveals.contains_key(&a) {
                continue;
            }
            let ctx = crate::sealed::RevealContext {
                round_id: next.round_id,
                attempt: next.draw.attempt,
                spec: &next.spec,
                commit: next.commits.get(&a).map(|(h, _)| *h),
            };
            let Some((q, rounds, sig)) = &locked else {
                match crate::sealed::open_posted(reveal_secret, &a, &p, &ctx)
                    .map_err(|_| bad("a post that cannot be opened"))?
                {
                    crate::sealed::Opened::Reveal((o, obs, salt, sig)) => {
                        next.reveal(p.posted_at, o, obs, salt, sig)?;
                        revealed.push(a);
                    }
                    crate::sealed::Opened::Dismissed(d) => snap.dismissals.push((a, d)),
                }
                continue;
            };
            match crate::timelock::open_timelocked(
                q,
                reveal_secret,
                Some(sig),
                &a,
                &p,
                &ctx,
                *rounds,
            )
            .map_err(|_| bad("a time-locked post that cannot be opened"))?
            {
                crate::timelock::TimedOpened::Reveal((o, obs, salt, sig)) => {
                    next.reveal(p.posted_at, o, obs, salt, sig)?;
                    revealed.push(a);
                }
                crate::timelock::TimedOpened::Dismissed(d) => {
                    snap.dismissals.push((a, d.dismissal));
                    if let (Some(o), Some(t)) = (d.opening, snap.timelock.as_mut()) {
                        t.openings.push((a, o));
                    }
                }
            }
        }
        let dismissed = snap.dismissals.iter().map(|(a, _)| *a).collect();
        next.check_sealed(&snap)?;
        next.sealed = Some(snap);
        *self = next;
        Ok((revealed, dismissed))
    }

    /// The round's sealed snapshot, once read (ROADMAP C11).
    pub fn sealed_snapshot(&self) -> Option<&SealedSnapshot> {
        self.sealed.as_ref()
    }

    /// The `revealLogComplete` / `attemptMatches` query of this round as it is
    /// published (ROADMAP C11): every drawn observer, its commit, and its
    /// status (revealed, dismissed in the sealed snapshot, or neither), the
    /// window of the round's config and its round id.
    pub fn reveal_log_query(&self, registry_fact: Hash32) -> crate::sealed::RevealLogQuery {
        let dismissed: BTreeSet<MinerAddress> = self
            .sealed
            .iter()
            .flat_map(|s| s.dismissals.iter().map(|(a, _)| *a))
            .collect();
        crate::sealed::reveal_log_query(
            registry_fact,
            &self.spec,
            &self.cfg,
            self.draw.attempt,
            self.round_id,
            &self.observers,
            &self.signed_commits(),
            &self.reveals.keys().copied().collect(),
            &dismissed,
        )
    }

    /// The log snapshot's own shape (review REVLOG-R3-1).
    fn check_snapshot(&self, snap: &LogSnapshot) -> Result<(), ObserveError> {
        let bad = ObserveError::BadLogSnapshot;
        if snap.block.is_some_and(|b| b.time <= self.cfg.commit_close) {
            return Err(bad("the block is not past commit close"));
        }
        if snap.block.is_none() && !snap.posted.is_empty() {
            return Err(bad("posts without a block"));
        }
        if !snap.posted.windows(2).all(|w| w[0].0 < w[1].0)
            || !snap.posted.iter().all(|(a, _)| self.is_observer(a))
        {
            return Err(bad("slots of observers not drawn, or out of order"));
        }
        Ok(())
    }

    /// Absorb the transcript log as read ONCE, from one block (review
    /// REVLOG-R3-1; [`absorb_posted_commits`] reads it): every commit posted
    /// inside the commit phase replaces the observer's own
    /// ([`absorb_posted_commit`](Self::absorb_posted_commit)), one posted
    /// outside it is ignored (`transcriptComplete` ignores it too), and the
    /// snapshot is kept in the round. Refused once a snapshot is kept
    /// ([`ObserveError::LogAlreadyRead`]) or after a reveal was taken; all or
    /// nothing. Returns the observers whose posted commit was absorbed.
    pub fn absorb_log(&mut self, snap: LogSnapshot) -> Result<Vec<MinerAddress>, ObserveError> {
        if self.log.is_some() {
            return Err(ObserveError::LogAlreadyRead);
        }
        if !self.reveals.is_empty() {
            return Err(ObserveError::BadLogSnapshot(
                "reveals were taken before the log was read",
            ));
        }
        self.check_snapshot(&snap)?;
        let mut next = self.clone();
        let mut absorbed = vec![];
        for (a, p) in &snap.posted {
            match next.absorb_posted_commit(p.posted_at, *a, p.commit, p.sig) {
                Ok(()) => absorbed.push(*a),
                Err(ObserveError::WrongPhase) => {}
                Err(e) => return Err(e),
            }
        }
        next.log = Some(snap);
        *self = next;
        Ok(absorbed)
    }

    /// The round's log snapshot, once read (review REVLOG-R3-1).
    pub fn log_snapshot(&self) -> Option<&LogSnapshot> {
        self.log.as_ref()
    }

    /// The observers whose post in `snap` lies in the commit phase.
    pub(super) fn absorbed_of(&self, snap: &LogSnapshot) -> Vec<MinerAddress> {
        snap.posted
            .iter()
            .filter(|(_, p)| {
                p.posted_at >= self.spec.window_start && p.posted_at <= self.cfg.commit_close
            })
            .map(|(a, _)| *a)
            .collect()
    }

    /// The feed id.
    pub fn spec_id(&self) -> Hash32 {
        self.spec_id
    }

    /// The round id: what every commit of this round is bound to.
    pub fn round_id(&self) -> Hash32 {
        self.round_id
    }

    /// The drawn observers, ascending by address.
    pub fn observers(&self) -> &[Observer] {
        &self.observers
    }

    /// The pool snapshot the observers were drawn from.
    pub fn pool(&self) -> &[Observer] {
        &self.pool
    }

    /// The draw's public inputs.
    pub fn draw(&self) -> DrawSeed {
        self.draw
    }

    /// The feed spec.
    pub fn spec(&self) -> &ObservationSpec {
        &self.spec
    }

    /// Timing and economics.
    pub fn config(&self) -> ObserveConfig {
        self.cfg
    }

    pub(super) fn is_observer(&self, a: &MinerAddress) -> bool {
        self.observers
            .binary_search_by(|o| o.address.cmp(a))
            .is_ok()
    }

    /// Every commit posted, ascending by observer: observer, hash, signature.
    pub fn signed_commits(&self) -> Vec<(MinerAddress, Hash32, ObserverSig)> {
        self.commits
            .iter()
            .map(|(a, (h, s))| (*a, *h, *s))
            .collect()
    }

    /// Every reveal accepted, ascending by observer: observer, observation,
    /// salt, signature.
    pub fn signed_reveals(&self) -> Vec<(MinerAddress, Observation, Hash32, ObserverSig)> {
        self.reveals
            .iter()
            .map(|(a, (o, salt, sig))| (*a, o.clone(), *salt, *sig))
            .collect()
    }

    /// Take a commit the observer posted on chain (`OnyxRegistry.postCommit`,
    /// ADR-0027 amendment 8) at block time `posted_at`. One posted inside the
    /// commit phase (from the window start to `commit_close`) must be in the
    /// transcript, so it replaces any commit the observer sent off chain
    /// (and drops a reveal of that one). The coordinator reads the log once
    /// the chain's time passes `commit_close`, before it takes reveals.
    pub fn absorb_posted_commit(
        &mut self,
        posted_at: u64,
        observer: MinerAddress,
        hash: Hash32,
        sig: ObserverSig,
    ) -> Result<(), ObserveError> {
        if self.log.is_some() {
            return Err(ObserveError::LogAlreadyRead);
        }
        if !self.is_observer(&observer) {
            return Err(ObserveError::NotObserver);
        }
        if posted_at < self.spec.window_start || posted_at > self.cfg.commit_close {
            return Err(ObserveError::WrongPhase);
        }
        if !commit_signed(&self.round_id, self.draw.attempt, &observer, &hash, &sig) {
            return Err(ObserveError::BadSignature);
        }
        if self.commits.get(&observer).map(|(h, _)| *h) != Some(hash) {
            self.reveals.remove(&observer);
        }
        self.commits.insert(observer, (hash, sig));
        Ok(())
    }

    /// The receipt for `observer`'s accepted reveal, signed with the draw's
    /// receipt key ([`reveal_receipt_message`]); `None` when its reveal was
    /// not accepted. The coordinator hands it back at once; the observer
    /// posts it on chain (`postReceipt`), and from then on a transcript
    /// without that reveal is refused.
    pub fn reveal_receipt(
        &self,
        receipt_key: &[u8; 32],
        observer: &MinerAddress,
    ) -> Option<ObserverSig> {
        self.reveals.get(observer)?;
        let (commit, _) = self.commits.get(observer)?;
        sign_reveal_receipt(
            receipt_key,
            &self.round_id,
            self.draw.attempt,
            observer,
            commit,
        )
    }

    /// The `transcriptComplete` query of this round as it would be published
    /// with every accepted reveal (`registry_fact`: the on-chain fact key).
    pub fn transcript_query(&self, registry_fact: Hash32) -> TranscriptLogQuery {
        TranscriptLogQuery::of(
            registry_fact,
            &self.spec,
            &self.cfg,
            &self.draw,
            self.round_id,
            &self.observers,
            &self.signed_commits(),
            &self.reveals.keys().copied().collect(),
        )
    }

    /// Post a commit (from the window start until `commit_close`), signed by
    /// the observer's key ([`observation_commit_message`], ATT-R1-F3). The
    /// same hash posted by two observers is not refused: the address is inside
    /// the commit, so a copied hash can never be revealed by the copier, and
    /// refusing it would let an observer who saw a hash first block its owner
    /// (L2R-F5).
    pub fn commit(
        &mut self,
        now: u64,
        observer: MinerAddress,
        hash: Hash32,
        sig: ObserverSig,
    ) -> Result<(), ObserveError> {
        if !self.is_observer(&observer) {
            return Err(ObserveError::NotObserver);
        }
        if now < self.spec.window_start || now > self.cfg.commit_close {
            return Err(ObserveError::WrongPhase);
        }
        if self.commits.contains_key(&observer) {
            return Err(ObserveError::AlreadyCommitted);
        }
        if !commit_signed(&self.round_id, self.draw.attempt, &observer, &hash, &sig) {
            return Err(ObserveError::BadSignature);
        }
        self.commits.insert(observer, (hash, sig));
        Ok(())
    }

    /// Reveal (after `commit_close`, until `reveal_close`). Nobody's answer is
    /// visible before every commit is closed.
    pub fn reveal(
        &mut self,
        now: u64,
        observer: MinerAddress,
        observation: Observation,
        salt: Hash32,
        sig: ObserverSig,
    ) -> Result<(), ObserveError> {
        if now <= self.cfg.commit_close || now > self.cfg.reveal_close {
            return Err(ObserveError::WrongPhase);
        }
        let (commit, _) = self.commits.get(&observer).ok_or(ObserveError::NoCommit)?;
        if self.reveals.contains_key(&observer) {
            return Err(ObserveError::AlreadyRevealed);
        }
        if observation_commit(&self.round_id, &observer, &observation, &salt) != *commit {
            return Err(ObserveError::HashMismatch);
        }
        if !reveal_signed(
            &self.round_id,
            self.draw.attempt,
            &observer,
            &observation,
            &salt,
            &sig,
        ) {
            return Err(ObserveError::BadSignature);
        }
        observation.check(&self.spec)?;
        self.reveals.insert(observer, (observation, salt, sig));
        Ok(())
    }

    /// Settle after `reveal_close`. Crate-private (L2R3-F5): rounds settle only
    /// through [`ObservationResults::settle`], which records each result once.
    pub(crate) fn settle(&self, now: u64) -> Result<ObservationSettlement, ObserveError> {
        if now <= self.cfg.reveal_close {
            return Err(ObserveError::WrongPhase);
        }
        let answers: Vec<Answer> = self
            .reveals
            .values()
            .map(|(o, _, _)| o.answer.clone())
            .collect();
        let (outcome, center) = aggregate(self.spec.kind, self.observers.len(), &answers);
        let mut statuses = Vec::with_capacity(self.observers.len());
        let mut entries = Vec::new();
        for o in &self.observers {
            let status = match (self.reveals.get(&o.address).map(|(r, _, _)| r), &outcome) {
                (None, _) if self.commits.contains_key(&o.address) => ObserverStatus::Unrevealed,
                (None, _) => ObserverStatus::Absent,
                // L2R-F2: an outage pays nobody. Saying `Unavailable` costs no
                // fetch and leaves no evidence, so paying it would make it a
                // free answer for a lazy majority.
                (Some(r), ObservationOutcome::SourceUnavailable) => {
                    if r.answer == Answer::Unavailable
                        || corroborated(self.spec.kind, &answers, &r.answer)
                    {
                        // L2R3-F4: a value at least one other observer
                        // matched (within tolerance) is evidence the source
                        // was up, so it is not slashed: a lazy majority saying
                        // `Unavailable` cannot slash the honest minority that
                        // fetched. Still nobody is paid for an outage (L2R-F2).
                        ObserverStatus::NoCall
                    } else {
                        // L2R2-F4: a lone value while the majority found the
                        // source down (a stale guess) costs the small slash,
                        // so guessing never beats an honest `Unavailable`.
                        ObserverStatus::Missed
                    }
                }
                (Some(r), ObservationOutcome::NoResult) => {
                    if far_from_cluster(
                        self.spec.kind,
                        self.observers.len(),
                        &answers,
                        center,
                        &r.answer,
                    ) {
                        ObserverStatus::Outlier
                    } else {
                        ObserverStatus::NoCall
                    }
                }
                (Some(r), ObservationOutcome::Value(v)) => {
                    if r.answer == Answer::Unavailable {
                        ObserverStatus::Missed
                    } else {
                        match judge(self.spec.kind, v, &r.answer, center) {
                            Judgment::Agrees => ObserverStatus::Agreed,
                            Judgment::Near => ObserverStatus::NoCall,
                            Judgment::Disagrees => ObserverStatus::Outlier,
                        }
                    }
                }
            };
            let delta: i128 = match status {
                ObserverStatus::Agreed => to_i128(self.cfg.reward),
                ObserverStatus::Outlier => -to_i128(self.cfg.outlier_slash.min(o.bond)),
                ObserverStatus::Missed => -to_i128(self.cfg.miss_slash.min(o.bond)),
                // SIG-R1-F1: never a slash on the coordinator's word alone.
                ObserverStatus::NoCall | ObserverStatus::Unrevealed | ObserverStatus::Absent => 0,
            };
            statuses.push((o.address, status));
            if delta != 0 {
                entries.push((o.address, delta));
            }
        }
        Ok(ObservationSettlement {
            spec_id: self.spec_id,
            round_id: self.round_id,
            outcome,
            statuses,
            entries,
        })
    }
}

/// L2R3-F4: `answer` is a value and at least one OTHER revealed value agrees
/// with it (identical bytes, or a number within `tolerance`). `answers` holds
/// every reveal, including `answer` itself.
fn corroborated(kind: ObservationKind, answers: &[Answer], answer: &Answer) -> bool {
    let matching = answers
        .iter()
        .filter(|a| match (kind, *a, answer) {
            (ObservationKind::Exact { .. }, Answer::Bytes(x), Answer::Bytes(y)) => x == y,
            (ObservationKind::Numeric { tolerance, .. }, Answer::Number(x), Answer::Number(y)) => {
                x.abs_diff(*y) <= tolerance
            }
            _ => false,
        })
        .count();
    // `answer` matches itself, so a second match is another observer's value.
    matching >= 2
}

/// L2R2-F5: in a numeric round with no result, a far number is still judged
/// when the honest cluster is identifiable: a strict majority of ALL assigned
/// observers revealed numbers within twice the tolerance of the center. A
/// number beyond twice the tolerance is then an outlier, so a minority cannot
/// force `NoResult` for free when honest answers drift between one and two
/// tolerances apart. With no such majority (an honest split) nobody is judged.
/// Exact feeds and `Unavailable` answers are never judged here.
fn far_from_cluster(
    kind: ObservationKind,
    members: usize,
    answers: &[Answer],
    center: i128,
    answer: &Answer,
) -> bool {
    let ObservationKind::Numeric { tolerance, .. } = kind else {
        return false;
    };
    let Answer::Number(a) = answer else {
        return false;
    };
    let near = tolerance.saturating_mul(2);
    let cluster = answers
        .iter()
        .filter(|x| matches!(x, Answer::Number(n) if n.abs_diff(center) <= near))
        .count();
    cluster >= quorum(members) && a.abs_diff(center) > near
}

/// Amounts are checked at open to fit `i128` (`ObserveConfig::validate`), so
/// this never saturates on a valid round.
fn to_i128(v: u128) -> i128 {
    i128::try_from(v).unwrap_or(i128::MAX)
}
