use super::*;

#[test]
fn a_well_formed_entry_splits_into_its_five_fields() {
    let line = "11:52:05 | base_record_create | create | Created memory/tasks/a-031.md (5173 bytes) | files: memory/tasks/a-031.md, memory/tasks.md";
    let entry = parse_line(line);
    assert_eq!(
        entry,
        LogLine::Entry(LogEntry {
            time: "11:52:05".to_owned(),
            origin: "base_record_create".to_owned(),
            operation: "create".to_owned(),
            description: "Created memory/tasks/a-031.md (5173 bytes)".to_owned(),
            files: vec!["memory/tasks/a-031.md".to_owned(), "memory/tasks.md".to_owned()],
        })
    );
}

#[test]
fn an_escaped_separator_stays_inside_the_description() -> Result<(), Box<dyn std::error::Error>> {
    let line = "21:08:36 | manual | log | check-ai-vendor-updates \\| 11 findings \\| files: [journal/2026/10/2026-10-07.md] | files: journal/2026/10/2026-10-07.md";
    let LogLine::Entry(entry) = parse_line(line) else {
        return Err("expected an entry".into());
    };
    assert_eq!(
        entry.description,
        "check-ai-vendor-updates | 11 findings | files: [journal/2026/10/2026-10-07.md]"
    );
    assert_eq!(entry.files, ["journal/2026/10/2026-10-07.md"]);
    Ok(())
}

#[test]
fn a_bracketed_file_list_and_minute_precision_time_are_accepted() -> Result<(), Box<dyn std::error::Error>> {
    let LogLine::Entry(entry) = parse_line("09:15 | session | modify | Tidied notes | files: [a.md, b.md]") else {
        return Err("expected an entry".into());
    };
    assert_eq!(entry.time, "09:15");
    assert_eq!(entry.files, ["a.md", "b.md"]);
    Ok(())
}

#[test]
fn an_entry_without_a_files_field_has_no_files() -> Result<(), Box<dyn std::error::Error>> {
    let LogLine::Entry(entry) = parse_line("09:15 | session | log | Planned the day") else {
        return Err("expected an entry".into());
    };
    assert_eq!(entry.description, "Planned the day");
    assert_eq!(entry.files, Vec::<String>::new());
    Ok(())
}

#[test]
fn headings_and_blank_lines_are_not_entries_and_anything_else_is_kept_as_written() {
    assert_eq!(
        parse_line("# 2026-10-07: Operation Log"),
        LogLine::Heading("2026-10-07: Operation Log".to_owned())
    );
    assert_eq!(parse_line("   "), LogLine::Blank);
    assert_eq!(
        parse_line("free text note"),
        LogLine::Other("free text note".to_owned())
    );
    assert_eq!(
        parse_line("25:00 | x | y | z"),
        LogLine::Other("25:00 | x | y | z".to_owned())
    );
}

#[test]
fn a_day_summary_counts_entries_operations_and_distinct_files() {
    let source = "# Day\n\n10:00 | a | create | one | files: x.md\n10:01 | b | modify | two | files: x.md, y.md\n10:02 | c | modify | three\nstray line\n";
    let day = parse_day(source);
    assert_eq!(day.entries().count(), 3);
    assert_eq!(day.distinct_files(), 2);
    assert_eq!(
        day.operation_counts(),
        [("modify".to_owned(), 2), ("create".to_owned(), 1)]
    );
}

#[test]
fn a_session_file_name_reads_as_a_date_time_and_title() {
    assert_eq!(
        session_label("2026-05-01-0054-claude-md-split-and-skill-flatten.md"),
        SessionLabel {
            date: Some("2026-05-01".to_owned()),
            time: Some("00:54".to_owned()),
            title: "Claude md split and skill flatten".to_owned(),
        }
    );
    assert_eq!(
        session_label("2026-05-08-lint.md"),
        SessionLabel {
            date: Some("2026-05-08".to_owned()),
            time: None,
            title: "Lint".to_owned(),
        }
    );
    assert_eq!(
        session_label("notes.md"),
        SessionLabel {
            date: None,
            time: None,
            title: "Notes".to_owned(),
        }
    );
}

#[test]
fn a_month_folder_beneath_a_year_reads_as_the_month_name() {
    assert_eq!(folder_label("10", Some("2026")), "October");
    assert_eq!(folder_label("13", Some("2026")), "13");
    assert_eq!(folder_label("10", None), "10");
    assert_eq!(folder_label("2026-05", Some("2026")), "2026-05");
}
