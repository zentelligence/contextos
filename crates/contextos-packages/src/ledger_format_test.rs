use proptest::prelude::*;
use time::macros::datetime;

use super::*;
use crate::ledger::{EntryProvenance, LedgerEntry, LedgerRemoval, RecordedCause, ReleaseOrigin};
use crate::test_support::{Must, fail};
use crate::test_support::{TestInterface, release};

fn installed(name: &str, version: &str, fill: char, origin: ReleaseOrigin) -> LedgerEntry<TestInterface> {
    LedgerEntry {
        release: release(name, version, fill),
        interface: TestInterface {
            inputs: vec!["day".to_owned()],
            note: None,
        },
        provenance: EntryProvenance::Installed {
            origin,
            installed_at: datetime!(2026-11-03 09:30:15 +11:00),
        },
        removal: None,
    }
}

fn text(ledger: &Ledger<TestInterface>) -> String {
    LedgerText::try_from(ledger).must("renders").0
}

fn read(text: &str) -> Result<Ledger<TestInterface>, LedgerProblem> {
    Ledger::<TestInterface>::try_from(text)
}

fn sample() -> Ledger<TestInterface> {
    let mut removed = installed("report", "1.0.0", 'a', ReleaseOrigin::Bundle);
    removed.removal = Some(LedgerRemoval {
        removed_at: datetime!(2026-11-04 01:00:00 UTC),
        cause: RecordedCause::Retention,
    });
    let mut forced = installed("report", "1.1.0", 'b', ReleaseOrigin::Source);
    forced.removal = Some(LedgerRemoval {
        removed_at: datetime!(2026-11-05 01:00:00 UTC),
        cause: RecordedCause::Forced,
    });
    let mut by_request = installed("other", "2.0.0", 'c', ReleaseOrigin::Source);
    by_request.removal = Some(LedgerRemoval {
        removed_at: datetime!(2026-11-05 02:00:00 UTC),
        cause: RecordedCause::ByRequest,
    });
    let rebuilt = LedgerEntry {
        release: release("rebuilt-one", "1.0.0", 'd'),
        interface: TestInterface {
            inputs: Vec::new(),
            note: Some("n".to_owned()),
        },
        provenance: EntryProvenance::Rebuilt,
        removal: None,
    };
    Ledger {
        entries: vec![
            removed,
            forced,
            by_request,
            installed("report", "1.2.0", 'e', ReleaseOrigin::Source),
            rebuilt,
        ],
        rebuilt_at: Some(datetime!(2026-11-06 00:00:00 UTC)),
    }
}

#[test]
fn a_ledger_reads_back_as_written_with_every_kind_of_entry() {
    let ledger = sample();
    assert_eq!(read(&text(&ledger)).must("reads"), ledger);
}

#[test]
fn an_empty_ledger_reads_back_empty() {
    let ledger = Ledger::<TestInterface> {
        entries: Vec::new(),
        rebuilt_at: None,
    };
    assert_eq!(read(&text(&ledger)).must("reads"), ledger);
}

#[test]
fn the_written_ledger_is_plain_toml_an_operator_can_read() {
    let written = text(&sample());
    assert!(written.contains("schema = 1"), "{written}");
    assert!(written.contains("[[release]]"), "{written}");
    assert!(written.contains("package = \"report\""), "{written}");
    assert!(written.contains("origin = \"bundle\""), "{written}");
    assert!(written.contains("removal-cause = \"by-request\""), "{written}");
    assert!(written.contains("[release.interface]"), "{written}");
}

fn unreadable(text: &str) -> String {
    match read(text) {
        Err(LedgerProblem::Unreadable(reason)) => reason,
        other => fail(format!("expected an unreadable ledger, got {other:?}")),
    }
}

fn entry_toml(extra: &str) -> String {
    format!(
        "schema = 1\n[[release]]\npackage = \"report\"\nversion = \"1.0.0\"\ndigest = \"sha256:{}\"\n{extra}\n[release.interface]\ninputs = []\n",
        "a".repeat(64)
    )
}

const INSTALLED: &str = "provenance = \"installed\"\norigin = \"source\"\ninstalled-at = \"2026-11-03T09:30:15+11:00\"";

#[test]
fn a_minimal_valid_entry_reads() {
    assert_eq!(read(&entry_toml(INSTALLED)).must("reads").entries.len(), 1);
}

#[test]
fn text_that_is_not_toml_or_breaks_the_schema_is_unreadable() {
    for bad in [
        "not toml [",
        "",
        "schema = 2\n",
        "schema = \"1\"\n",
        "schema = 1\nunknown = 1\n",
        "schema = 1\nrebuilt-at = \"yesterday\"\n",
    ] {
        unreadable(bad);
    }
}

#[test]
fn an_entry_that_breaks_its_schema_is_unreadable_and_named() {
    let removed = format!("{INSTALLED}\nremoved-at = \"2026-11-04T01:00:00Z\"");
    let cases: Vec<(String, &str)> = vec![
        (entry_toml(&format!("{INSTALLED}\nsurprise = 1")), "surprise"),
        (
            entry_toml("provenance = \"installed\"\ninstalled-at = \"2026-11-03T09:30:15Z\""),
            "origin",
        ),
        (
            entry_toml("provenance = \"installed\"\norigin = \"source\""),
            "installed-at",
        ),
        (entry_toml("provenance = \"rebuilt\"\norigin = \"source\""), "origin"),
        (
            entry_toml("provenance = \"rebuilt\"\ninstalled-at = \"2026-11-03T09:30:15Z\""),
            "installed-at",
        ),
        (entry_toml("provenance = \"made-up\""), "made-up"),
        (
            entry_toml("provenance = \"installed\"\norigin = \"tarball\"\ninstalled-at = \"2026-11-03T09:30:15Z\""),
            "tarball",
        ),
        (
            entry_toml("provenance = \"installed\"\norigin = \"source\"\ninstalled-at = \"soon\""),
            "installed-at",
        ),
        (entry_toml(&removed), "removal"),
        (
            entry_toml(&format!("{INSTALLED}\nremoval-cause = \"retention\"")),
            "removal",
        ),
        (
            entry_toml(&format!("{removed}\nremoval-cause = \"unrecorded\"")),
            "unrecorded",
        ),
        (
            entry_toml(&format!(
                "{INSTALLED}\nremoved-at = \"tomorrow\"\nremoval-cause = \"forced\""
            )),
            "removed-at",
        ),
        (entry_toml(INSTALLED).replace("report", "Report"), "Report"),
        (entry_toml(INSTALLED).replace("1.0.0", "1.0"), "1.0"),
        (entry_toml(INSTALLED).replace("1.0.0", "1.0.0-rc.1"), "1.0.0-rc.1"),
        (entry_toml(INSTALLED).replace(&"a".repeat(64), "abc"), "digest"),
        (entry_toml(INSTALLED).replace("inputs = []", "inputs = 7"), "report"),
    ];
    for (bad, mentions) in cases {
        let reason = unreadable(&bad);
        assert!(reason.contains(mentions), "{mentions} not in {reason:?} for {bad}");
    }
}

#[test]
fn one_version_recorded_twice_is_unreadable_even_with_the_same_digest() {
    let one = entry_toml(INSTALLED);
    let body = &one["schema = 1\n".len()..];
    let same = format!("{one}{body}");
    assert!(unreadable(&same).contains("report 1.0.0"));
    let other = format!("{one}{}", body.replace(&"a".repeat(64), &"b".repeat(64)));
    assert!(unreadable(&other).contains("two digests"), "{}", unreadable(&other));
}

#[test]
fn the_reason_is_one_line_naming_the_line_and_never_quoting_the_file() {
    let reason = unreadable("schema = 1\n\n[[release]]\npackage = 5\n");
    assert!(!reason.contains('\n') && !reason.contains(" | "), "{reason}");
    assert!(reason.contains("line 4"), "{reason}");
}

fn arbitrary_entry() -> impl Strategy<Value = LedgerEntry<TestInterface>> {
    (
        "[a-z][a-z0-9-]{0,10}",
        (0u8..5, 0u8..5, 0u8..5),
        prop::sample::select(vec!['a', 'b', 'c', '0', 'f']),
        prop::option::of(prop::sample::select(vec![
            RecordedCause::Retention,
            RecordedCause::Forced,
            RecordedCause::ByRequest,
        ])),
        prop::bool::ANY,
        prop::collection::vec("[a-z]{1,5}", 0..3),
        prop::option::of("[ -~]{0,12}"),
        0i64..2_000_000_000,
    )
        .prop_filter_map(
            "valid name",
            |(name, (a, b, c), fill, cause, source, inputs, note, seconds)| {
                // The pattern also draws names the name check refuses, such as `prn`, which Windows reserves for a
                // device; those are skipped, since this property is about the ledger, not about names.
                crate::ReleaseName::try_from(name.as_str()).ok()?;
                let at = time::OffsetDateTime::from_unix_timestamp(seconds).ok()?;
                Some(LedgerEntry {
                    release: release(&name, &format!("{a}.{b}.{c}"), fill),
                    interface: TestInterface { inputs, note },
                    provenance: if source {
                        EntryProvenance::Installed {
                            origin: ReleaseOrigin::Source,
                            installed_at: at,
                        }
                    } else {
                        EntryProvenance::Rebuilt
                    },
                    removal: cause.map(|cause| LedgerRemoval { removed_at: at, cause }),
                })
            },
        )
}

proptest! {
    #[test]
    fn any_ledger_of_distinct_releases_reads_back_as_written(
        entries in prop::collection::vec(arbitrary_entry(), 0..6),
        rebuilt in prop::bool::ANY
    ) {
        let mut seen = std::collections::BTreeSet::new();
        let entries: Vec<_> = entries.into_iter().filter(|entry| seen.insert(entry.release.key.clone())).collect();
        let ledger = Ledger {
            entries,
            rebuilt_at: rebuilt.then_some(datetime!(2026-11-06 00:00:00 UTC)),
        };
        prop_assert_eq!(read(&text(&ledger)).must("reads"), ledger);
    }
}
