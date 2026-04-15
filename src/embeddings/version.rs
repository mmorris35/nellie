//! ONNX Runtime version validation.
//!
//! Ensures the loaded ONNX Runtime library is compatible with the `ort` crate
//! version compiled into this binary.  The `check_ort_version()` function
//! should be called early in startup — before any `Session` is created — so
//! that a version mismatch produces a clear, actionable error instead of a
//! cryptic crash-loop.
//!
//! The `MIN_ORT_VERSION` constant must stay in sync with the version pinned in
//! `packaging/install-universal.sh` (`ORT_VERSION`).

/// Minimum supported ONNX Runtime version.
///
/// The `ort` crate at 2.0.0-rc.11 links against `ORT_API_VERSION = 23`,
/// which maps to ONNX Runtime >= 1.23.x.
pub const MIN_ORT_VERSION: &str = "1.23.0";

/// Recommended (pinned) ORT version shipped by `packaging/install-universal.sh`.
pub const RECOMMENDED_ORT_VERSION: &str = "1.24.4";

/// Parse a dotted version string such as `"1.24.4"` into `(major, minor, patch)`.
///
/// A two-component string like `"2.0"` is accepted — `patch` defaults to `0`.
/// Returns `None` on any parse failure.
#[must_use]
pub fn parse_version(s: &str) -> Option<(u32, u32, u32)> {
    let mut parts = s.trim().split('.').map(str::parse::<u32>);
    let major = parts.next()?.ok()?;
    let minor = parts.next()?.ok()?;
    let patch = parts.next().and_then(Result::ok).unwrap_or(0);
    Some((major, minor, patch))
}

/// Return `true` when `found >= MIN_ORT_VERSION`.
///
/// # Panics
///
/// Panics if `MIN_ORT_VERSION` cannot be parsed (compile-time constant, so
/// this should never happen in practice).
#[must_use]
pub fn is_compatible(found: (u32, u32, u32)) -> bool {
    let min = parse_version(MIN_ORT_VERSION).expect("MIN_ORT_VERSION is a valid semver triple");
    found >= min
}

/// Compare two version tuples: `a >= b`.
#[must_use]
pub fn version_ge(a: (u32, u32, u32), b: (u32, u32, u32)) -> bool {
    a >= b
}

/// Format a version tuple as a dotted string.
#[must_use]
pub fn format_version(v: (u32, u32, u32)) -> String {
    format!("{}.{}.{}", v.0, v.1, v.2)
}

/// Eagerly load the ONNX Runtime dynamic library and validate its version.
///
/// On success the runtime build-info string is returned (e.g.
/// `"ORT Build Info: git-branch=rel-1.24.4, ..."`).
///
/// On failure a human-readable error string is returned that includes:
/// - the minimum required version,
/// - the recommended (pinned) version,
/// - concrete remediation steps.
///
/// This function calls `ort::init_from` when `ORT_DYLIB_PATH` is set, or
/// falls back to `ort::init().commit()` (which triggers lazy loading via
/// platform default search paths).  Either way, the ONNX Runtime library is
/// loaded and its version is validated by the `ort` crate itself before this
/// function returns.
pub fn check_ort_version() -> Result<String, String> {
    // Attempt to load the dynamic library eagerly so we can surface version
    // mismatches *before* any Session is constructed.
    let load_result = match std::env::var("ORT_DYLIB_PATH") {
        Ok(ref p) if !p.is_empty() => {
            // init_from both loads the library and returns Result
            ort::init_from(p).map(|builder| {
                builder.commit();
            })
        }
        _ => {
            // No explicit path — trigger the default search.
            // init() cannot fail, but the actual library load happens lazily
            // when api() is first called.  Force it now.
            ort::init().commit();
            // Trigger the lazy API initialisation which loads the dylib.
            // ort::api() panics on failure; catch that with catch_unwind.
            std::panic::catch_unwind(ort::api)
                .map(|_| ())
                .map_err(|panic_payload| {
                    let msg = panic_payload
                        .downcast_ref::<String>()
                        .cloned()
                        .or_else(|| {
                            panic_payload
                                .downcast_ref::<&str>()
                                .map(ToString::to_string)
                        })
                        .unwrap_or_else(|| "unknown panic while loading ONNX Runtime".to_string());
                    ort::Error::new(msg)
                })
        }
    };

    match load_result {
        Ok(()) => {
            // Library loaded successfully — grab the build info string.
            let info = ort::info().to_string();
            Ok(info)
        }
        Err(e) => {
            let err_msg = e.to_string();
            Err(format!(
                "Failed to load ONNX Runtime: {err_msg}\n\
                 \n\
                 Nellie requires ONNX Runtime >= {MIN_ORT_VERSION} \
                 (recommended: {RECOMMENDED_ORT_VERSION}).\n\
                 \n\
                 To fix:\n  \
                 1. Run `nellie setup` to download the correct version, or\n  \
                 2. Set ORT_DYLIB_PATH to a compatible libonnxruntime.{{so,dylib}}, or\n  \
                 3. Re-run `bash packaging/install-universal.sh`."
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── parse_version ────────────────────────────────────────────────

    #[test]
    fn parse_three_components() {
        assert_eq!(parse_version("1.24.4"), Some((1, 24, 4)));
        assert_eq!(parse_version("1.23.0"), Some((1, 23, 0)));
        assert_eq!(parse_version("0.0.0"), Some((0, 0, 0)));
    }

    #[test]
    fn parse_two_components_defaults_patch() {
        assert_eq!(parse_version("2.0"), Some((2, 0, 0)));
        assert_eq!(parse_version("1.23"), Some((1, 23, 0)));
    }

    #[test]
    fn parse_leading_trailing_whitespace() {
        assert_eq!(parse_version("  1.24.4  "), Some((1, 24, 4)));
    }

    #[test]
    fn parse_invalid_returns_none() {
        assert_eq!(parse_version("abc"), None);
        assert_eq!(parse_version(""), None);
        assert_eq!(parse_version("1"), None);
        assert_eq!(parse_version(".."), None);
        assert_eq!(parse_version("1.x.3"), None);
    }

    // ── is_compatible ────────────────────────────────────────────────

    #[test]
    fn compatible_versions() {
        assert!(is_compatible((1, 24, 4))); // recommended
        assert!(is_compatible((1, 23, 0))); // exact minimum
        assert!(is_compatible((1, 23, 1))); // patch above minimum
        assert!(is_compatible((2, 0, 0))); // major above
    }

    #[test]
    fn incompatible_versions() {
        assert!(!is_compatible((1, 22, 9)));
        assert!(!is_compatible((1, 20, 1)));
        assert!(!is_compatible((0, 99, 99)));
    }

    // ── version_ge ───────────────────────────────────────────────────

    #[test]
    fn version_ge_equal() {
        assert!(version_ge((1, 23, 0), (1, 23, 0)));
    }

    #[test]
    fn version_ge_greater() {
        assert!(version_ge((1, 24, 0), (1, 23, 0)));
        assert!(version_ge((2, 0, 0), (1, 99, 99)));
        assert!(version_ge((1, 23, 1), (1, 23, 0)));
    }

    #[test]
    fn version_ge_less() {
        assert!(!version_ge((1, 22, 0), (1, 23, 0)));
        assert!(!version_ge((0, 99, 0), (1, 0, 0)));
    }

    // ── format_version ───────────────────────────────────────────────

    #[test]
    fn format_round_trip() {
        let v = (1, 24, 4);
        assert_eq!(parse_version(&format_version(v)), Some(v));
    }

    // ── constants ────────────────────────────────────────────────────

    #[test]
    fn constants_are_parseable() {
        assert!(parse_version(MIN_ORT_VERSION).is_some());
        assert!(parse_version(RECOMMENDED_ORT_VERSION).is_some());
    }

    #[test]
    fn recommended_is_at_least_minimum() {
        let min = parse_version(MIN_ORT_VERSION).expect("valid");
        let rec = parse_version(RECOMMENDED_ORT_VERSION).expect("valid");
        assert!(version_ge(rec, min));
    }
}
