//! Data rights. Every claim carries them; ingest refuses data we may not redistribute.

use crate::hash::{keccak256, Hash32};
use crate::ids::{is_dotted_id, is_license_id};
use crate::source::SourceId;
use borsh::{BorshDeserialize, BorshSerialize};

/// Whether we may serve this data to paying customers and, after the decay
/// window, to everyone.
#[derive(Clone, Copy, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum Redistribution {
    /// Redistribution allowed without restriction (public chain data, public domain, CC0, CC-BY).
    Allowed,
    /// Redistribution allowed only under terms the license names (for example
    /// attribution plus non-commercial). Ingest is allowed; delivery must honour `terms`.
    Restricted,
    /// Redistribution forbidden. Onyx never ingests this.
    Forbidden,
}

/// Rights metadata attached to every claim and every source.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct DataRights {
    /// Visible-ASCII license id. SPDX identifier where one exists (`CC0-1.0`, `CC-BY-4.0`), otherwise a
    /// stable custom id such as `onyx:public-chain-data` or `onyx:first-party-attestation`.
    pub license: String,
    /// Redistribution status.
    pub redistribution: Redistribution,
    /// Where the data comes from.
    pub source: SourceId,
    /// Display-only attribution line (visible ASCII), if the license requires credit.
    /// Never a place for terms: restrictions belong in `redistribution` and `terms`.
    pub attribution: Option<String>,
    /// Terms id (dotted id, resolved in the source registry from M3) for
    /// `Restricted`. Must be empty for other statuses.
    pub terms: String,
}

/// Rights error.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum RightsError {
    /// The license forbids redistribution.
    #[error("source {0:?} forbids redistribution; refusing to ingest")]
    RedistributionForbidden(SourceId),
    /// License id missing or not visible ASCII.
    #[error("license id missing or malformed")]
    BadLicense,
    /// Source id malformed.
    #[error("source id malformed")]
    BadSource,
    /// Restricted without a valid terms id.
    #[error("restricted redistribution needs a terms id")]
    RestrictedWithoutTerms,
    /// Allowed but terms given: terms would be silently ignored, so refuse (R-RIGHTS-1).
    #[error("allowed redistribution must not carry terms; use Restricted")]
    AllowedWithTerms,
    /// Attribution empty or containing control characters.
    #[error("attribution malformed")]
    BadAttribution,
}

impl DataRights {
    /// Rights for public blockchain data.
    pub fn public_chain(source: SourceId) -> Self {
        DataRights {
            license: "onyx:public-chain-data".into(),
            redistribution: Redistribution::Allowed,
            source,
            attribution: None,
            terms: String::new(),
        }
    }

    /// Gate for every ingest path. Hard limit: never ingest forbidden data.
    ///
    /// This checks form only. Rights are self-declared until the reviewed source
    /// registry (M3) supplies them per `SourceId`; see THREATS R-RIGHTS-1.
    pub fn check_ingest(&self) -> Result<(), RightsError> {
        if !is_license_id(&self.license) {
            return Err(RightsError::BadLicense);
        }
        if !is_dotted_id(&self.source.0) {
            return Err(RightsError::BadSource);
        }
        if let Some(a) = &self.attribution {
            // Display-only credit line: visible ASCII and spaces, no zero-width or
            // bidi characters, and it never carries terms (R-RIGHTS-3).
            if a.trim().is_empty()
                || a.len() > 256
                || !a.bytes().all(|c| c.is_ascii_graphic() || c == b' ')
            {
                return Err(RightsError::BadAttribution);
            }
        }
        match self.redistribution {
            Redistribution::Forbidden => {
                Err(RightsError::RedistributionForbidden(self.source.clone()))
            }
            Redistribution::Restricted if !is_dotted_id(&self.terms) => {
                Err(RightsError::RestrictedWithoutTerms)
            }
            Redistribution::Allowed if !self.terms.is_empty() => Err(RightsError::AllowedWithTerms),
            _ => Ok(()),
        }
    }

    /// Keccak-256 of the canonical encoding; bound into every commitment.
    pub fn hash(&self) -> Hash32 {
        keccak256(&borsh::to_vec(self).unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rights(r: Redistribution, terms: &str) -> DataRights {
        DataRights {
            license: "LicenseRef-vendor".into(),
            redistribution: r,
            source: SourceId("vendor.feed".into()),
            attribution: None,
            terms: terms.into(),
        }
    }

    #[test]
    fn forbidden_is_never_ingested() {
        assert!(matches!(
            rights(Redistribution::Forbidden, "").check_ingest(),
            Err(RightsError::RedistributionForbidden(_))
        ));
    }

    #[test]
    fn restricted_needs_terms_id() {
        for bad in ["", " ", ".", "no redistribution"] {
            assert_eq!(
                rights(Redistribution::Restricted, bad).check_ingest(),
                Err(RightsError::RestrictedWithoutTerms),
                "{bad:?}"
            );
        }
        assert!(rights(Redistribution::Restricted, "terms.vendor.nc-v1")
            .check_ingest()
            .is_ok());
    }

    #[test]
    fn allowed_with_terms_rejected() {
        assert_eq!(
            rights(Redistribution::Allowed, "no redistribution").check_ingest(),
            Err(RightsError::AllowedWithTerms)
        );
    }

    #[test]
    fn invisible_license_rejected() {
        let mut r = rights(Redistribution::Allowed, "");
        r.license = "\u{200B}".into();
        assert_eq!(r.check_ingest(), Err(RightsError::BadLicense));
        r.license = " ".into();
        assert_eq!(r.check_ingest(), Err(RightsError::BadLicense));
    }

    #[test]
    fn attribution_is_plain_ascii() {
        let mut r = rights(Redistribution::Allowed, "");
        r.attribution = Some("Data: Example Org".into());
        assert_eq!(r.check_ingest(), Ok(()));
        // At most 256 bytes (mutation L4).
        r.attribution = Some("a".repeat(256));
        assert_eq!(r.check_ingest(), Ok(()));
        r.attribution = Some("a".repeat(257));
        assert_eq!(r.check_ingest(), Err(RightsError::BadAttribution));
        r.attribution = Some("a".repeat(256));
        for bad in ["\u{202E}gro elpmaxE", "Example\u{200B}", " ", "Ex\nample"] {
            r.attribution = Some(bad.into());
            assert_eq!(
                r.check_ingest(),
                Err(RightsError::BadAttribution),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn public_chain_ok() {
        assert!(DataRights::public_chain(SourceId("chain.btc".into()))
            .check_ingest()
            .is_ok());
    }
}
