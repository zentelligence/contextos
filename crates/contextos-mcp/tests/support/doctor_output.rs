//! Reading what `contextos doctor --resolve` prints, shared by the test binaries that run it.
//!
//! A binary that includes this file without using every item would otherwise warn, which is expected rather than
//! genuine dead code.
#![allow(dead_code)]

/// The subjects of every outcome `contextos doctor --resolve` printed, with or without `--dry-run`, in order.
pub fn resolution_subjects(stdout: &str) -> Vec<&str> {
    stdout
        .lines()
        .filter_map(|line| {
            line.strip_prefix("Would resolve: ")
                .or_else(|| line.strip_prefix("Resolved: "))
                .or_else(|| line.strip_prefix("Not resolved: "))
        })
        .filter_map(|rest| rest.split(" (").next())
        .collect()
}
