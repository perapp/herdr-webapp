//! Build provenance embedded at compile time; never probes Git or Core at runtime.
#[allow(dead_code)] // Build-time formatting helpers share validation with this module.
mod validation;

pub const VERSION: &str = env!("HERDR_DISPLAY_VERSION");
/// Empty for untagged/unknown builds; only release management may use this.
pub const RELEASE_VERSION: &str = env!("HERDR_RELEASE_VERSION");

#[derive(Debug, serde::Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BuildInfo {
    pub version: &'static str,
    pub release_version: Option<&'static str>,
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
            version: VERSION,
            release_version: validation::release_tag(RELEASE_VERSION),
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
                serde_json::json!({"version":VERSION,"releaseVersion":validation::release_tag(RELEASE_VERSION),"revision":null,"commitDate":null,"dirty":null})
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
    fn versions_use_exact_tags_or_commit_local_time() {
        assert_eq!(
            validation::display_version(Some(SHA), Some("2026-06-03T14:05:06+02:00"), None)
                .as_deref(),
            Some("260603140506+0200-0123456")
        );
        assert_eq!(
            validation::display_version(Some(SHA), Some("2026-01-02T03:04:05-05:30"), None)
                .as_deref(),
            Some("260102030405-0530-0123456")
        );
        assert_eq!(
            validation::display_version(Some(SHA), Some("2026-01-02T03:04:05Z"), None).as_deref(),
            Some("260102030405+0000-0123456")
        );
        assert_eq!(
            validation::display_version(Some(SHA), None, Some("v1.2.3")).as_deref(),
            Some("1.2.3")
        );
        assert_eq!(validation::display_version(Some(SHA), None, None), None);
        assert_eq!(validation::display_version(None, None, None), None);
        for tag in ["v1.2.3", "1.2.3-rc.1+build"] {
            assert!(validation::release_tag(tag).is_some());
        }
        for tag in ["1.02.3", "1.2.3-01", "1.2.3+", "1.2.3-", "1.2.3\n", "main"] {
            assert!(validation::release_tag(tag).is_none(), "accepted {tag}");
        }
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
