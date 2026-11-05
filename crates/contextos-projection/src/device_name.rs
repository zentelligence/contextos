//! The one rule for Windows device names, shared by every name the projection writes.

/// Windows device names, reserved with or without an extension, in any case.
const RESERVED_STEMS: [&str; 4] = ["con", "prn", "aux", "nul"];

/// Whether Windows would open a device for `component`: its stem (before the first dot, ignoring trailing spaces) is
/// a reserved name, case-insensitively. `COM` and `LPT` with superscript digits are reserved too.
pub(crate) fn is_reserved_device(component: &str) -> bool {
    let stem = component.split('.').next().unwrap_or_default().trim_end_matches(' ');
    let lower = stem.to_lowercase();
    if RESERVED_STEMS.contains(&lower.as_str()) {
        return true;
    }
    ["com", "lpt"].iter().any(|prefix| {
        lower.strip_prefix(prefix).is_some_and(|rest| {
            matches!(
                rest,
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "\u{b9}" | "\u{b2}" | "\u{b3}"
            )
        })
    })
}
