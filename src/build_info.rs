//! Build provenance embedded at compile time; never probes Git or Core at runtime.
mod validation;

#[derive(Debug, serde::Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BuildInfo {
    pub version: &'static str,
    pub revision: Option<&'static str>,
    pub commit_date: Option<&'static str>,
    pub dirty: Option<bool>,
}

impl BuildInfo {
    pub fn current() -> Self {
        Self::from_compile_time(
            option_env!("HERDR_BUILD_REVISION"),
            option_env!("HERDR_BUILD_COMMIT_DATE"),
            option_env!("HERDR_BUILD_DIRTY"),
        )
    }

    fn from_compile_time(
        revision: Option<&'static str>,
        date: Option<&'static str>,
        dirty: Option<&str>,
    ) -> Self {
        let revision = revision.filter(|r| validation::revision(r));
        Self {
            version: env!("CARGO_PKG_VERSION"),
            revision,
            commit_date: revision.and(date.filter(|d| validation::commit_date(d))),
            dirty: revision.and(match dirty {
                Some("true") => Some(true),
                Some("false") => Some(false),
                _ => None,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

    #[test]
    fn legacy_and_invalid_provenance_is_unknown() {
        for revision in [
            None,
            Some(""),
            Some("main"),
            Some("0123456"),
            Some("zz23456789abcdef0123456789abcdef01234567"),
        ] {
            let value = serde_json::to_value(BuildInfo::from_compile_time(
                revision,
                Some("2026-02-18T12:34:56Z"),
                Some("true"),
            ))
            .unwrap();
            assert_eq!(
                value,
                serde_json::json!({"version":env!("CARGO_PKG_VERSION"),"revision":null,"commitDate":null,"dirty":null})
            );
        }
    }
    #[test]
    fn explicit_sources_and_nullable_fields() {
        let info = BuildInfo::from_compile_time(
            Some(SHA),
            Some("2024-02-29T12:34:56.123+01:00"),
            Some("false"),
        );
        assert_eq!(info.revision, Some(SHA));
        assert_eq!(info.commit_date, Some("2024-02-29T12:34:56.123+01:00"));
        assert_eq!(info.dirty, Some(false));
        assert_eq!(
            BuildInfo::from_compile_time(Some(SHA), Some("today"), Some("1")).commit_date,
            None
        );
        assert_eq!(
            BuildInfo::from_compile_time(Some(SHA), None, Some("true")).dirty,
            Some(true)
        );
    }
    #[test]
    fn calendar_timestamp_validation_rejects_malformed_and_injection() {
        for bad in [
            "",
            "2025-02-29T12:00:00Z",
            "2024-00-01T12:00:00Z",
            "2024-01-00T12:00:00Z",
            "2024-01-01T24:00:00Z",
            "2024-01-01T12:60:00Z",
            "2024-01-01T12:00:00",
            "2024-01-01T12:00:00.Z",
            "2024-01-01T12:00:00+24:00",
            "2024-01-01T12:00:00Z\ncargo:warning=bad",
            "éééééééééé",
        ] {
            assert!(!validation::commit_date(bad), "accepted {bad:?}");
        }
        for valid in ["2024-02-29T12:00:00Z", "2026-01-01T12:00:00-05:30"] {
            assert!(validation::commit_date(valid));
        }
    }
}
