//! The onboarding questions and the route they lead to (ADR-0038 items 1-3).
//!
//! Five questions, each a closed choice, so the route is a pure function of
//! the answers and the website can run the same function offline:
//!
//! 1. What is it? ([`Kind`])
//! 2. Can anyone fetch it with no login, key or payment? (`no_login`)
//! 3. What are the rights? ([`Rights`], with a licence id for an open licence)
//! 4. Does it name a person? (`names_person`)
//! 5. How often does it change? ([`Changes`])
//!
//! Refusals come first and never depend on the kind: data that names a person
//! (hard limit: entities only, never individuals) and data whose terms forbid
//! redistribution (hard limit) are refused. "Not sure" about rights is held for
//! a rights review and never published until a reviewer clears it.

use borsh::BorshSerialize;
use onyx_core::ProofLevel;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Largest licence id, in bytes.
pub const MAX_LICENSE_ID: usize = 64;

/// Question 1: what the data is.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize, Serialize, BorshSerialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// Data anyone can recompute from a public chain.
    Chain,
    /// A public web page or API.
    PublicApi,
    /// Data the source itself signs.
    Signed,
    /// The submitter's own or private data, or anything else.
    Own,
    /// A forecast: a statement about a value known only later.
    Forecast,
    /// A public API that needs the submitter's own key (e.g. FRED). The key
    /// never reaches Onyx; the submitter's side would make the request and
    /// send a proof of the response (zkTLS direction, ROADMAP S2).
    KeyedApi,
}

/// Question 3: the data's rights, as the submitter states them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize, BorshSerialize)]
#[serde(rename_all = "snake_case")]
pub enum Rights {
    /// Public domain.
    PublicDomain,
    /// An open licence; its id is [`Answers::license_id`].
    OpenLicense,
    /// The submitter's own data, theirs to share.
    OwnData,
    /// The submitter does not know.
    NotSure,
    /// The source's terms forbid redistribution.
    TermsForbid,
}

/// Question 5: how often the value changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize, BorshSerialize)]
#[serde(rename_all = "snake_case")]
pub enum Changes {
    /// Once (a fixed fact).
    Once,
    /// About daily.
    Daily,
    /// Continuously (a live price, a counter).
    Live,
}

/// The five answers. JSON form (shared with the website):
/// `{"what": "public_api", "noLogin": true, "rights": "open_license",
/// "licenseId": "...", "namesPerson": false, "changes": "daily"}`; unknown
/// fields are refused.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize, BorshSerialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Answers {
    /// Question 1.
    pub what: Kind,
    /// Question 2: anyone can fetch it with no login, key or payment.
    pub no_login: bool,
    /// Question 3.
    pub rights: Rights,
    /// The open licence's id (SPDX or a short name), with
    /// [`Rights::OpenLicense`] only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license_id: Option<String>,
    /// Question 4: the data names a person.
    pub names_person: bool,
    /// Question 5.
    pub changes: Changes,
}

/// What happens to a submission.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Recorded for review on its route.
    Accept,
    /// Recorded, held for a rights review; never published until cleared.
    Held,
    /// Not recorded.
    Refused,
}

/// The next step on an accepted or held route.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Path {
    /// Level 1: someone writes a recipe over pinned chain inputs.
    RecipeRequest,
    /// Level 2: bonded observers pull it themselves and re-observe it.
    AutoPull,
    /// Level 3: the source registers its signing key.
    SignedSource,
    /// Level 4: an unverified claim.
    Claim,
    /// Level 5: judged later against a stronger answer.
    TrackRecord,
    /// Level 4 until a real proof of the keyed response exists (S2).
    KeyedProof,
}

/// One plain reason on a route. The codes are stable (the website and the
/// shared vectors use them); [`Reason::text`] is the sentence shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    /// Refused: names a person.
    NamesPerson,
    /// Refused: terms forbid redistribution.
    TermsForbid,
    /// Held: rights not known.
    RightsReview,
    /// Level 1 path.
    ChainRecipe,
    /// Level 2 path.
    PublicAutoPull,
    /// Level 2: the value moves faster than a round.
    LiveWindow,
    /// Level 4 because a login, key or payment is needed.
    NeedsLogin,
    /// Level 3 path.
    SignedKey,
    /// Level 4 path.
    OwnClaim,
    /// Level 4: nobody checked it.
    Unverified,
    /// Level 5 path.
    ForecastTrack,
    /// Held: a source behind the submitter's own key.
    KeyedSource,
    /// The proof step is a labelled stub until S2.
    ProofNotReal,
    /// Held: the source is blocked in Onyx's rights review
    /// (docs/ops/data-sources.md), e.g. FRED (N10).
    SourceBlocked,
}

impl Reason {
    /// Every reason, in a fixed order (the vectors list each one's text).
    pub const ALL: [Reason; 14] = [
        Reason::NamesPerson,
        Reason::TermsForbid,
        Reason::RightsReview,
        Reason::ChainRecipe,
        Reason::PublicAutoPull,
        Reason::LiveWindow,
        Reason::NeedsLogin,
        Reason::SignedKey,
        Reason::OwnClaim,
        Reason::Unverified,
        Reason::ForecastTrack,
        Reason::KeyedSource,
        Reason::ProofNotReal,
        Reason::SourceBlocked,
    ];

    /// The stable code.
    pub fn code(self) -> &'static str {
        match self {
            Reason::NamesPerson => "names_person",
            Reason::TermsForbid => "terms_forbid",
            Reason::RightsReview => "rights_review",
            Reason::ChainRecipe => "chain_recipe",
            Reason::PublicAutoPull => "public_auto_pull",
            Reason::LiveWindow => "live_window",
            Reason::NeedsLogin => "needs_login",
            Reason::SignedKey => "signed_key",
            Reason::OwnClaim => "own_claim",
            Reason::Unverified => "unverified",
            Reason::ForecastTrack => "forecast_track",
            Reason::KeyedSource => "keyed_source",
            Reason::ProofNotReal => "proof_not_real",
            Reason::SourceBlocked => "source_blocked",
        }
    }

    /// The sentence shown to the submitter.
    pub fn text(self) -> &'static str {
        match self {
            Reason::NamesPerson => "Onyx never publishes data about a person. Data may name companies, funds and protocols only.",
            Reason::TermsForbid => "The source's terms forbid redistribution, so Onyx cannot take it.",
            Reason::RightsReview => "The rights are not known yet. It is held for a rights review and never published until a reviewer clears it.",
            Reason::ChainRecipe => "Chain data can be recomputed by anyone. It needs a recipe: a small open program that reads pinned blocks. Your submission becomes a recipe request.",
            Reason::PublicAutoPull => "Anyone can fetch it with no login, so it can reach level 2 once a reviewer clears its source's rights: bonded observers would then re-read it at the same time and must agree (built for the U.S. Treasury source after approval; other sources need their own adapter, ROADMAP S4).",
            Reason::LiveWindow => "It changes continuously, so observers must read it inside one short window and agree within a tolerance.",
            Reason::NeedsLogin => "It needs a login, key or payment, so observers cannot re-read it. It stays a claim until a way to prove the response exists (ROADMAP S2).",
            Reason::SignedKey => "The source signs it. Once the source registers its signing key, anyone can check each value's signature.",
            Reason::OwnClaim => "It is your own or private data. Onyx records what you say it means as a claim.",
            Reason::Unverified => "Labelled unverified: nobody has checked it. Buyers see it as level 4.",
            Reason::ForecastTrack => "A forecast cannot be checked today. It is scored later against the real answer, and your track record builds up.",
            Reason::KeyedSource => "It needs your own API key. Your key stays in your browser. A real prover (ROADMAP S2) would make the request from your side and send Onyx only a proof of the response; the stub in this build makes no request. A keyed source is held for a rights review of its terms and never published until cleared.",
            Reason::ProofNotReal => "The proof step is not built yet (ROADMAP S2). Today the page makes no request and sends a placeholder labelled not real; it proves nothing.",
            Reason::SourceBlocked => "Onyx's rights review blocks this source: its terms restrict redistribution. It is held and will not be published.",
        }
    }
}

/// The classifier's answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Route {
    /// Accept, held or refused.
    pub outcome: Outcome,
    /// The level the data can reach on this route (`None` when refused).
    pub level: Option<ProofLevel>,
    /// The next step (`None` when refused).
    pub path: Option<Path>,
    /// Plain reasons, in order.
    pub reasons: Vec<Reason>,
}

/// The plain name of a level, as the website shows it.
pub fn level_name(level: ProofLevel) -> &'static str {
    match level {
        ProofLevel::Recomputed => "Recomputed",
        ProofLevel::ReObserved => "Re-observed",
        ProofLevel::SourceProven => "Source-proven",
        ProofLevel::BondedClaim => "Claim (unverified)",
        ProofLevel::TrackRecord => "Track record",
    }
}

impl Outcome {
    /// The stable code.
    pub fn code(self) -> &'static str {
        match self {
            Outcome::Accept => "accept",
            Outcome::Held => "held",
            Outcome::Refused => "refused",
        }
    }
}

impl Path {
    /// The stable code.
    pub fn code(self) -> &'static str {
        match self {
            Path::RecipeRequest => "recipe_request",
            Path::AutoPull => "auto_pull",
            Path::SignedSource => "signed_source",
            Path::Claim => "claim",
            Path::TrackRecord => "track_record",
            Path::KeyedProof => "keyed_proof",
        }
    }
}

impl Route {
    /// The route's JSON form (shared with the website and the vectors):
    /// `{"outcome", "level", "path", "reasons": [codes]}`.
    pub fn to_json(&self) -> Value {
        json!({
            "outcome": self.outcome.code(),
            "level": self.level.map(ProofLevel::number),
            "path": self.path.map(Path::code),
            "reasons": self.reasons.iter().map(|r| r.code()).collect::<Vec<_>>(),
            "status": self.status(),
        })
    }

    /// What a recorded submission on this route is called: `pending_review`
    /// (accepted), `held_keyed_source_review` (held on the keyed-source
    /// path), `held_rights_review` (any other hold), or `refused` (never
    /// recorded).
    pub fn status(&self) -> &'static str {
        match (self.outcome, self.path) {
            (Outcome::Refused, _) => "refused",
            (Outcome::Held, Some(Path::KeyedProof)) => "held_keyed_source_review",
            (Outcome::Held, _) => "held_rights_review",
            (Outcome::Accept, _) => "pending_review",
        }
    }

    /// The JSON form with the level's name and each reason's sentence, for a
    /// person to read.
    pub fn to_json_explained(&self) -> Value {
        let mut v = self.to_json();
        v["levelName"] = self.level.map(level_name).into();
        v["reasonTexts"] = self
            .reasons
            .iter()
            .map(|r| r.text())
            .collect::<Vec<_>>()
            .into();
        v
    }
}

/// Why a set of answers is malformed.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum AnswersError {
    /// An open licence needs its id.
    #[error("an open licence needs its id (for example CC-BY-4.0)")]
    LicenseIdMissing,
    /// A licence id was given with another rights answer.
    #[error("a licence id goes with the open-licence answer only")]
    LicenseIdUnexpected,
    /// The licence id is too long or has characters outside `A-Z a-z 0-9 . - + _ :`.
    #[error("the licence id must be 1 to 64 characters from A-Z, a-z, 0-9 and . - + _ :")]
    LicenseIdBad,
}

impl AnswersError {
    /// The stable code.
    pub fn code(&self) -> &'static str {
        match self {
            AnswersError::LicenseIdMissing => "license_id_missing",
            AnswersError::LicenseIdUnexpected => "license_id_unexpected",
            AnswersError::LicenseIdBad => "license_id_bad",
        }
    }
}

impl Answers {
    /// The shape rules: a licence id with an open licence only, 1 to 64
    /// characters from a small set.
    pub fn validate(&self) -> Result<(), AnswersError> {
        match (&self.rights, &self.license_id) {
            (Rights::OpenLicense, None) => Err(AnswersError::LicenseIdMissing),
            (Rights::OpenLicense, Some(id)) => {
                let ok = !id.is_empty()
                    && id.len() <= MAX_LICENSE_ID
                    && id
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b".-+_:".contains(&b));
                if ok {
                    Ok(())
                } else {
                    Err(AnswersError::LicenseIdBad)
                }
            }
            (_, Some(_)) => Err(AnswersError::LicenseIdUnexpected),
            (_, None) => Ok(()),
        }
    }
}

/// The route for `a` (ADR-0038 item 2). Pure and total: the shape rules are
/// [`Answers::validate`]'s, checked before a submission is taken.
///
/// - Names a person, or terms forbid: refused (both reasons when both hold).
/// - Chain data: level 1, a recipe request.
/// - A public page or API anyone can fetch: level 2, pulled by observers
///   (plus a note when it changes live). Behind a login, key or payment:
///   level 4, an unverified claim.
/// - Signed by the source: level 3.
/// - Own or private data, anything else: level 4, labelled unverified.
/// - A forecast: level 5, a track record.
/// - A public API behind the submitter's own key: level 4 with a proof step
///   (a stub until S2), always held for a rights review.
/// - Rights not sure: the same route, held for a rights review.
pub fn classify(a: &Answers) -> Route {
    let mut refusals = Vec::new();
    if a.names_person {
        refusals.push(Reason::NamesPerson);
    }
    if a.rights == Rights::TermsForbid {
        refusals.push(Reason::TermsForbid);
    }
    if !refusals.is_empty() {
        return Route {
            outcome: Outcome::Refused,
            level: None,
            path: None,
            reasons: refusals,
        };
    }
    let (level, path, mut reasons) = match a.what {
        Kind::Chain => (
            ProofLevel::Recomputed,
            Path::RecipeRequest,
            vec![Reason::ChainRecipe],
        ),
        Kind::PublicApi if a.no_login => {
            let mut r = vec![Reason::PublicAutoPull];
            if a.changes == Changes::Live {
                r.push(Reason::LiveWindow);
            }
            (ProofLevel::ReObserved, Path::AutoPull, r)
        }
        Kind::PublicApi => (
            ProofLevel::BondedClaim,
            Path::Claim,
            vec![Reason::NeedsLogin, Reason::Unverified],
        ),
        Kind::Signed => (
            ProofLevel::SourceProven,
            Path::SignedSource,
            vec![Reason::SignedKey],
        ),
        Kind::Own => (
            ProofLevel::BondedClaim,
            Path::Claim,
            vec![Reason::OwnClaim, Reason::Unverified],
        ),
        Kind::Forecast => (
            ProofLevel::TrackRecord,
            Path::TrackRecord,
            vec![Reason::ForecastTrack],
        ),
        Kind::KeyedApi => (
            ProofLevel::BondedClaim,
            Path::KeyedProof,
            vec![
                Reason::KeyedSource,
                Reason::ProofNotReal,
                Reason::Unverified,
            ],
        ),
    };
    let mut outcome = Outcome::Accept;
    if a.rights == Rights::NotSure {
        reasons.insert(0, Reason::RightsReview);
        outcome = Outcome::Held;
    }
    // A keyed source is always held: whoever holds the key, the source's
    // terms decide whether its data may be passed on.
    if a.what == Kind::KeyedApi {
        outcome = Outcome::Held;
    }
    Route {
        outcome,
        level: Some(level),
        path: Some(path),
        reasons,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KINDS: [Kind; 6] = [
        Kind::Chain,
        Kind::PublicApi,
        Kind::Signed,
        Kind::Own,
        Kind::Forecast,
        Kind::KeyedApi,
    ];
    const RIGHTS: [Rights; 5] = [
        Rights::PublicDomain,
        Rights::OpenLicense,
        Rights::OwnData,
        Rights::NotSure,
        Rights::TermsForbid,
    ];
    const CHANGES: [Changes; 3] = [Changes::Once, Changes::Daily, Changes::Live];

    fn all() -> Vec<Answers> {
        let mut v = Vec::new();
        for what in KINDS {
            for no_login in [false, true] {
                for rights in RIGHTS {
                    for names_person in [false, true] {
                        for changes in CHANGES {
                            v.push(Answers {
                                what,
                                no_login,
                                rights,
                                license_id: (rights == Rights::OpenLicense)
                                    .then(|| "CC-BY-4.0".to_string()),
                                names_person,
                                changes,
                            });
                        }
                    }
                }
            }
        }
        v
    }

    /// Every one of the 360 answer sets: the hard limits hold whatever the
    /// kind, and only a public no-login source reaches level 2.
    #[test]
    fn every_answer_set_keeps_the_hard_limits() {
        let sets = all();
        assert_eq!(sets.len(), 360);
        for a in &sets {
            a.validate().expect("valid");
            let r = classify(a);
            if a.names_person || a.rights == Rights::TermsForbid {
                assert_eq!(r.outcome, Outcome::Refused, "{a:?}");
                assert_eq!((r.level, r.path), (None, None));
                continue;
            }
            assert_ne!(r.outcome, Outcome::Refused, "{a:?}");
            assert_eq!(
                r.outcome == Outcome::Held,
                a.rights == Rights::NotSure || a.what == Kind::KeyedApi,
                "{a:?}"
            );
            assert_eq!(
                r.reasons.first() == Some(&Reason::RightsReview),
                a.rights == Rights::NotSure
            );
            let level = r.level.expect("level");
            assert_eq!(
                level == ProofLevel::ReObserved,
                a.what == Kind::PublicApi && a.no_login,
                "{a:?}"
            );
            // Level 4 is always labelled unverified.
            assert_eq!(
                level == ProofLevel::BondedClaim,
                r.reasons.contains(&Reason::Unverified)
            );
            assert_eq!(
                r.path == Some(Path::AutoPull),
                level == ProofLevel::ReObserved
            );
        }
    }

    #[test]
    fn licence_ids_go_with_open_licences_only() {
        let mut a = all()
            .into_iter()
            .find(|a| a.rights == Rights::OpenLicense)
            .expect("one");
        a.license_id = None;
        assert_eq!(a.validate(), Err(AnswersError::LicenseIdMissing));
        for bad in ["", "has space", "x/y", &"a".repeat(65), "é"] {
            a.license_id = Some(bad.into());
            assert_eq!(a.validate(), Err(AnswersError::LicenseIdBad), "{bad:?}");
        }
        a.license_id = Some("onyx:us-treasury-fiscal-data-open".into());
        assert_eq!(a.validate(), Ok(()));
        a.rights = Rights::PublicDomain;
        assert_eq!(a.validate(), Err(AnswersError::LicenseIdUnexpected));
    }

    #[test]
    fn unknown_answer_fields_and_values_are_refused() {
        let ok = r#"{"what":"own","noLogin":false,"rights":"own_data","namesPerson":false,"changes":"once"}"#;
        assert!(serde_json::from_str::<Answers>(ok).is_ok());
        for bad in [
            r#"{"what":"own","noLogin":false,"rights":"own_data","namesPerson":false,"changes":"once","email":"x"}"#,
            r#"{"what":"other","noLogin":false,"rights":"own_data","namesPerson":false,"changes":"once"}"#,
            r#"{"what":"own","rights":"own_data","namesPerson":false,"changes":"once"}"#,
            r#"{"what":"own","noLogin":"no","rights":"own_data","namesPerson":false,"changes":"once"}"#,
        ] {
            assert!(serde_json::from_str::<Answers>(bad).is_err(), "{bad}");
        }
    }
}
