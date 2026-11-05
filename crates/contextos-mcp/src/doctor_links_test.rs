use crate::doctor::DoctorStatus;
use crate::doctor::fixture::check_over;

fn link_check(files: &[(&str, &str)]) -> Result<crate::doctor::DoctorCheck, Box<dyn std::error::Error>> {
    check_over("Link integrity", files)
}

#[test]
fn a_vault_whose_links_all_resolve_passes_with_the_number_of_links_checked() -> Result<(), Box<dyn std::error::Error>> {
    let check = link_check(&[("a.md", "[[b]] [c](b.md) [[{{slug}}]]\n"), ("b.md", "x\n")])?;

    assert_eq!(check.status, DoctorStatus::Pass);
    assert_eq!(check.message, "2 links checked");
    Ok(())
}

#[test]
fn broken_and_ambiguous_links_and_unreadable_notes_fail_with_counts_and_the_first_listed()
-> Result<(), Box<dyn std::error::Error>> {
    let check = link_check(&[
        ("a.md", "ok\n[[gone]]\n[[status]]\n[x](nowhere.md)\n"),
        ("bad.md", "text\n[[unclosed\n"),
        ("one/status.md", "x\n"),
        ("two/status.md", "x\n"),
    ])?;

    assert_eq!(check.status, DoctorStatus::Fail);
    assert!(!check.auto_fixable);
    assert!(check.remediation.is_none());
    assert!(check.message.contains("2 not-found link(s)"), "{}", check.message);
    assert!(check.message.contains("1 ambiguous link(s)"), "{}", check.message);
    assert!(check.message.contains("1 unreadable note(s)"), "{}", check.message);
    assert!(check.message.contains("2 note(s)"), "{}", check.message);
    assert!(
        check.message.contains("a.md:2 -> gone (not-found)"),
        "{}",
        check.message
    );
    assert!(
        check.message.contains("a.md:3 -> status (ambiguous)"),
        "{}",
        check.message
    );
    assert!(
        check.message.contains("a.md:4 -> nowhere.md (not-found)"),
        "{}",
        check.message
    );
    assert!(check.message.contains("bad.md (unreadable)"), "{}", check.message);
    let action = check.action.unwrap_or_default();
    assert!(action.contains("query_graph"), "{action}");
    assert!(action.contains("unresolved"), "{action}");
    Ok(())
}

#[test]
fn only_the_first_broken_links_are_listed_and_the_rest_are_counted() -> Result<(), Box<dyn std::error::Error>> {
    let mut body = String::new();
    for index in 0..15 {
        std::fmt::Write::write_fmt(&mut body, format_args!("[[gone-{index:02}]]\n"))?;
    }

    let check = link_check(&[("a.md", body.as_str())])?;

    assert!(check.message.contains("15 not-found link(s)"), "{}", check.message);
    assert!(
        check.message.contains("a.md:10 -> gone-09 (not-found)"),
        "{}",
        check.message
    );
    assert!(!check.message.contains("gone-10"), "{}", check.message);
    assert!(check.message.contains("and 5 more"), "{}", check.message);
    Ok(())
}

#[test]
fn links_in_excluded_source_folders_and_in_code_are_not_checked() -> Result<(), Box<dyn std::error::Error>> {
    let check = link_check(&[
        ("templates/t.md", "[[nowhere]]\n"),
        ("a.md", "`[[inline]]`\n```\n[[fenced]]\n```\n%% [[commented]] %%\n"),
    ])?;

    assert_eq!(check.status, DoctorStatus::Pass, "{}", check.message);
    assert_eq!(check.message, "0 links checked");
    Ok(())
}
