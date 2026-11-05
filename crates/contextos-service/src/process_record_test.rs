use std::time::{SystemTime, UNIX_EPOCH};

use tempfile::tempdir;

use super::*;

fn now_ms() -> Result<u64, Box<dyn std::error::Error>> {
    Ok(u64::try_from(
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis(),
    )?)
}

#[test]
fn each_config_has_its_own_record_beside_it() {
    assert_eq!(
        path_for(Path::new("/home/op/.config/contextos/web.toml")),
        PathBuf::from("/home/op/.config/contextos/web.toml.process.json")
    );
    assert_ne!(path_for(Path::new("/a/web.toml")), path_for(Path::new("/a/other.toml")));
}

#[test]
fn a_claimed_record_names_this_process_and_when_it_wrote_it_until_it_is_dropped()
-> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let path = path_for(&dir.path().join("web.toml"));
    let before = now_ms()?;

    let guard = ProcessRecordGuard::claim(path.clone(), "contextos-web")?;

    let record = read(&path, "contextos-web")?.ok_or("a record is written")?;
    assert_eq!(record.pid, std::process::id());
    assert_eq!(record.exe, std::env::current_exe()?);
    assert!((before..=now_ms()?).contains(&record.written_at_ms), "{record:?}");
    drop(guard);
    assert_eq!(read(&path, "contextos-web")?, None, "a clean stop leaves no record");
    Ok(())
}

#[test]
fn dropping_a_guard_leaves_a_record_another_process_has_since_written() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let path = dir.path().join("web.toml.process.json");
    let guard = ProcessRecordGuard::claim(path.clone(), "contextos-web")?;
    let newer = ProcessRecord {
        pid: std::process::id().wrapping_add(1),
        exe: PathBuf::from("/opt/contextos-web"),
        written_at_ms: now_ms()?,
    };
    std::fs::write(&path, serde_json::to_string(&newer)?)?;

    drop(guard);

    assert_eq!(
        read(&path, "contextos-web")?,
        Some(newer),
        "only this process's own record is removed"
    );
    Ok(())
}

#[test]
fn no_record_reads_as_none() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    assert_eq!(read(&dir.path().join("absent.json"), "contextos-web")?, None);
    Ok(())
}

#[test]
fn a_malformed_record_is_refused_naming_its_path_not_read_as_none() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let path = dir.path().join("web.toml.process.json");
    std::fs::write(&path, r#"{"pid": "not a number"}"#)?;

    let result = read(&path, "contextos-web");

    assert!(
        matches!(&result, Err(ProcessRecordError::Malformed { path: named, .. }) if *named == path),
        "{result:?}"
    );
    Ok(())
}

#[test]
fn removing_an_absent_record_succeeds() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    remove(&dir.path().join("absent.json"))?;
    Ok(())
}

#[test]
fn a_malformed_record_message_names_the_program_that_wrote_it() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let path = dir.path().join("config.toml.process.json");
    std::fs::write(&path, "not json")?;

    let message = read(&path, "contextos")
        .err()
        .ok_or("a malformed record is refused")?
        .to_string();

    assert!(message.contains("not one contextos wrote"), "{message}");
    assert!(message.contains("stop any running contextos server"), "{message}");
    assert!(!message.contains("contextos-web"), "{message}");
    Ok(())
}

#[test]
fn the_record_for_config_toml_is_named_for_it() {
    assert_eq!(
        path_for(Path::new("/home/op/.config/contextos/config.toml")),
        PathBuf::from("/home/op/.config/contextos/config.toml.process.json")
    );
}
