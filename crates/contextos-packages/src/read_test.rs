use super::*;
use crate::check::{DamageFinding, IncompleteFinding, ReadCheck, UnverifiableReason};
use crate::io::StoreFailureKind;
use crate::test_support::{Built, FakeIo, Node, TestEngine, TestManifest, engine_over, key};
use crate::test_support::{Must, fail};

fn sample() -> Built {
    Built::new(
        "report",
        "1.0.0",
        &[
            ("main.rhai", b"let x = 1;"),
            ("templates/a.html", b"<p>"),
            ("templates/b/c.txt", b"c"),
        ],
    )
}

fn engine_with(release: &Built) -> (TestEngine, FakeIo) {
    let io = FakeIo::with_root();
    release.put_into(&io);
    (engine_over(io.clone()), io)
}

async fn check(engine: &TestEngine) -> ReadCheck<TestManifest> {
    engine.check_release(&key("report", "1.0.0")).await
}

#[tokio::test]
async fn a_whole_release_is_verified_and_carries_the_bytes_it_was_verified_with() {
    let release = sample();
    let (engine, _) = engine_with(&release);
    let ReadCheck::Verified(verified) = check(&engine).await else {
        fail("expected a verified release")
    };
    assert_eq!(verified.release, release.id());
    assert_eq!(verified.manifest_text, release.manifest_text());
    assert_eq!(verified.files, release.release_files());
}

#[tokio::test]
async fn a_release_with_no_folder_is_removed() {
    let engine = engine_over(FakeIo::with_root());
    assert_eq!(check(&engine).await, ReadCheck::Removed);
    let rootless = engine_over(FakeIo::default());
    assert_eq!(check(&rootless).await, ReadCheck::Removed);
}

#[tokio::test]
async fn a_folder_with_no_manifest_is_incomplete() {
    let (engine, io) = engine_with(&sample());
    io.delete("report/1.0.0/manifest.toml");
    assert_eq!(
        check(&engine).await,
        ReadCheck::Incomplete(vec![IncompleteFinding::ManifestMissing])
    );
    let empty = FakeIo::with_root();
    empty.put_dir("report/1.0.0");
    assert_eq!(
        check(&engine_over(empty)).await,
        ReadCheck::Incomplete(vec![IncompleteFinding::ManifestMissing])
    );
}

#[tokio::test]
async fn a_listed_file_that_is_absent_makes_the_release_incomplete() {
    let (engine, io) = engine_with(&sample());
    io.delete("report/1.0.0/templates/a.html");
    let ReadCheck::Incomplete(findings) = check(&engine).await else {
        fail("expected incomplete")
    };
    assert_eq!(
        findings,
        vec![IncompleteFinding::ListedFileAbsent(
            ReleaseFilePath::try_from("templates/a.html").must("path")
        )]
    );
}

#[tokio::test]
async fn an_entry_the_manifest_does_not_list_makes_the_release_incomplete() {
    let (engine, io) = engine_with(&sample());
    io.put_file("report/1.0.0/extra.txt", b"x");
    io.put_dir("report/1.0.0/empty-folder");
    io.put_file("report/1.0.0/templates/b/d.txt", b"d");
    let ReadCheck::Incomplete(findings) = check(&engine).await else {
        fail("expected incomplete")
    };
    let unlisted: Vec<_> = findings
        .iter()
        .map(|finding| match finding {
            IncompleteFinding::UnlistedEntry(path) => path.as_str(),
            other => fail(format!("unexpected {other:?}")),
        })
        .collect();
    assert_eq!(unlisted, ["extra.txt", "templates/b/d.txt", "empty-folder"]);
}

#[tokio::test]
async fn every_incomplete_finding_is_reported_together() {
    let (engine, io) = engine_with(&sample());
    io.delete("report/1.0.0/main.rhai");
    io.put_file("report/1.0.0/extra.txt", b"x");
    let ReadCheck::Incomplete(findings) = check(&engine).await else {
        fail("expected incomplete")
    };
    assert_eq!(findings.len(), 2, "{findings:?}");
}

#[tokio::test]
async fn a_changed_file_is_damage_and_so_is_the_digest_it_no_longer_matches() {
    let (engine, io) = engine_with(&sample());
    io.put_file("report/1.0.0/main.rhai", b"let x = 2;");
    let ReadCheck::Damaged(findings) = check(&engine).await else {
        fail("expected damage")
    };
    assert!(findings.contains(&DamageFinding::FileHashMismatch(
        ReleaseFilePath::try_from("main.rhai").must("path")
    )));
    assert!(
        findings
            .iter()
            .any(|finding| matches!(finding, DamageFinding::DigestMismatch { .. })),
        "{findings:?}"
    );
}

#[tokio::test]
async fn files_that_match_their_hashes_but_not_the_declared_digest_are_damage() {
    let mut release = sample();
    release.declared_digest = Some(format!("sha256:{}", "0".repeat(64)));
    let (engine, _) = engine_with(&release);
    let ReadCheck::Damaged(findings) = check(&engine).await else {
        fail("expected damage")
    };
    assert_eq!(findings.len(), 1, "{findings:?}");
    assert!(
        matches!(&findings[0], DamageFinding::DigestMismatch { declared, computed }
        if declared.as_str() == "sha256:".to_owned() + &"0".repeat(64) && *computed == release.digest())
    );
}

#[tokio::test]
async fn a_manifest_that_does_not_parse_or_breaks_the_model_is_damage() {
    let (engine, io) = engine_with(&sample());
    for text in [&b"not toml ["[..], b"name = \"report\"\n", &[0xff, 0xfe][..]] {
        io.put_file("report/1.0.0/manifest.toml", text);
        let ReadCheck::Damaged(findings) = check(&engine).await else {
            fail("expected damage")
        };
        assert!(
            matches!(findings.as_slice(), [DamageFinding::ManifestInvalid(_)]),
            "{findings:?}"
        );
    }
}

#[tokio::test]
async fn a_manifest_for_another_release_is_damage() {
    let io = FakeIo::with_root();
    let other = Built::new("report", "2.0.0", &[("main.rhai", b"let x = 1;")]);
    other.put_under(&io, "report/1.0.0");
    let engine = engine_over(io);
    let ReadCheck::Damaged(findings) = check(&engine).await else {
        fail("expected damage")
    };
    assert_eq!(
        findings,
        vec![DamageFinding::ManifestForAnotherRelease {
            folder: key("report", "1.0.0"),
            manifest: key("report", "2.0.0")
        }]
    );
    let io = FakeIo::with_root();
    Built::new("other", "1.0.0", &[]).put_under(&io, "report/1.0.0");
    assert!(matches!(check(&engine_over(io)).await, ReadCheck::Damaged(_)));
}

#[tokio::test]
async fn a_link_or_special_entry_in_the_release_is_damage_and_is_never_read() {
    let (engine, io) = engine_with(&sample());
    io.put_node("report/1.0.0/templates/b/link", Node::Link);
    io.put_node("report/1.0.0/pipe", Node::Other);
    let ReadCheck::Damaged(findings) = check(&engine).await else {
        fail("expected damage")
    };
    assert_eq!(
        findings,
        vec![
            DamageFinding::NotARegularFile("pipe".to_owned()),
            DamageFinding::LinkInRelease("templates/b/link".to_owned()),
        ]
    );
    let (engine, io) = engine_with(&sample());
    io.put_node("report/1.0.0/main.rhai", Node::Link);
    assert!(
        matches!(check(&engine).await, ReadCheck::Damaged(findings) if findings == vec![DamageFinding::LinkInRelease("main.rhai".to_owned())])
    );
    let (engine, io) = engine_with(&sample());
    io.put_node("report/1.0.0/manifest.toml", Node::Link);
    assert!(
        matches!(check(&engine).await, ReadCheck::Damaged(findings) if findings == vec![DamageFinding::LinkInRelease("manifest.toml".to_owned())])
    );
}

#[tokio::test]
async fn an_unreachable_store_is_unverifiable_never_removed() {
    let (engine, io) = engine_with(&sample());
    io.set_unavailable(true);
    let ReadCheck::Unverifiable(UnverifiableReason::StoreUnavailable(failure)) = check(&engine).await else {
        fail("expected unverifiable")
    };
    assert_eq!(failure.kind, StoreFailureKind::Unavailable);
}

#[tokio::test]
async fn an_outage_part_way_through_a_read_is_unverifiable() {
    let (engine, io) = engine_with(&sample());
    io.before_read(1, |state| state.unavailable = true);
    assert!(matches!(
        check(&engine).await,
        ReadCheck::Unverifiable(UnverifiableReason::StoreUnavailable(_))
    ));
}

#[tokio::test]
async fn a_release_that_changes_while_it_is_read_is_unverifiable() {
    // Folder listed again after the files are read: an entry appears.
    let (engine, io) = engine_with(&sample());
    io.before_list_tree(1, |state| {
        state.nodes.insert(
            vec!["report".into(), "1.0.0".into(), "late.txt".into()],
            Node::File(b"x".to_vec()),
        );
    });
    assert_eq!(
        check(&engine).await,
        ReadCheck::Unverifiable(UnverifiableReason::ChangedWhileRead)
    );

    // The manifest is rewritten between its first and second reads, to the same release with other text.
    let release = sample();
    let (engine, io) = engine_with(&release);
    let last_read = 1 + release.files.len();
    let changed = format!("{}\n# changed\n", release.manifest_text());
    io.before_read(last_read, move |state| {
        state.nodes.insert(
            vec!["report".into(), "1.0.0".into(), "manifest.toml".into()],
            Node::File(changed.into_bytes()),
        );
    });
    assert_eq!(
        check(&engine).await,
        ReadCheck::Unverifiable(UnverifiableReason::ChangedWhileRead)
    );

    // A listed file vanishes between the listing and its read.
    let (engine, io) = engine_with(&sample());
    io.before_read(1, |state| {
        state
            .nodes
            .remove(&vec!["report".into(), "1.0.0".into(), "main.rhai".into()]);
    });
    assert_eq!(
        check(&engine).await,
        ReadCheck::Unverifiable(UnverifiableReason::ChangedWhileRead)
    );
}

#[tokio::test]
async fn a_release_over_the_read_limits_is_unverifiable_before_its_content_is_read() {
    let (mut engine, io) = engine_with(&sample());
    engine.limits.max_entries = 2;
    let ReadCheck::Unverifiable(UnverifiableReason::LimitExceeded(limits)) = check(&engine).await else {
        fail("expected a limit")
    };
    assert_eq!(limits, engine.limits);

    let (mut engine, io2) = engine_with(&sample());
    engine.limits.max_bytes = 10;
    let before = io2.state().read_calls;
    assert!(matches!(
        check(&engine).await,
        ReadCheck::Unverifiable(UnverifiableReason::LimitExceeded(_))
    ));
    assert_eq!(io2.state().read_calls, before, "no content was read");
    drop(io);
}

#[tokio::test]
async fn a_manifest_listing_a_file_twice_is_damage() {
    let io = FakeIo::with_root();
    let release = sample();
    let text = release.manifest_text();
    let doubled = format!(
        "{text}\n[[files]]\npath = \"main.rhai\"\nhash = \"{}\"\n",
        "0".repeat(64)
    );
    release.put_into(&io);
    io.put_file("report/1.0.0/manifest.toml", doubled.as_bytes());
    let ReadCheck::Damaged(findings) = check(&engine_over(io)).await else {
        fail("expected damage")
    };
    assert!(
        matches!(findings.as_slice(), [DamageFinding::ManifestInvalid(_)]),
        "{findings:?}"
    );
}
