use super::*;

#[test]
fn only_the_last_components_are_shown() {
    let path = Path::new("/home/someone/.local/share/contextos/state/shared-jobs.toml");

    assert_eq!(Trailing { path, count: 2 }.to_string(), "state/shared-jobs.toml");
    assert_eq!(Trailing { path, count: 1 }.to_string(), "shared-jobs.toml");
}

#[test]
fn a_path_shorter_than_the_count_is_shown_whole_without_its_root() {
    let path = Path::new("/a/b");

    assert_eq!(Trailing { path, count: 5 }.to_string(), "a/b");
}
