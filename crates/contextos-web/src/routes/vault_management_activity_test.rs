use super::*;

fn file(path: &str) -> VaultFile {
    VaultFile {
        name: path.rsplit('/').next().unwrap_or(path).to_owned(),
        relative_path: path.to_owned(),
    }
}

fn log_tree() -> VaultFolder {
    VaultFolder {
        name: "log".to_owned(),
        relative_path: String::new(),
        folders: vec![VaultFolder {
            name: "2026".to_owned(),
            relative_path: "2026".to_owned(),
            folders: vec![
                VaultFolder {
                    name: "09".to_owned(),
                    relative_path: "2026/09".to_owned(),
                    folders: Vec::new(),
                    files: vec![file("2026/09/2026-09-30.md")],
                },
                VaultFolder {
                    name: "10".to_owned(),
                    relative_path: "2026/10".to_owned(),
                    folders: Vec::new(),
                    files: vec![file("2026/10/2026-10-01.md"), file("2026/10/index.md")],
                },
            ],
            files: Vec::new(),
        }],
        files: vec![file("index.md")],
    }
}

#[test]
fn activity_reads_newest_first_with_months_named_and_indexes_left_out() {
    let arranged = arranged(&log_tree(), None);
    let year = &arranged.folders[0];
    let months: Vec<&str> = year.folders.iter().map(|month| month.name.as_str()).collect();
    assert_eq!(months, ["October", "September"]);
    assert!(year.folders[0].files.iter().all(|file| !file.is_index()));
    assert_eq!(arranged.files, Vec::<VaultFile>::new());
}

#[test]
fn neighbours_follow_the_calendar_across_folders() {
    let tree = log_tree();
    let files = chronological(&tree);
    let names: Vec<&str> = files.iter().map(|file| file.name.as_str()).collect();
    assert_eq!(names, ["2026-09-30.md", "2026-10-01.md"]);
}
