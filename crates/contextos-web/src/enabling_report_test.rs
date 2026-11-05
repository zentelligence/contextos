use contextos_workflows::JobSchedule;
use semver::Version;

use super::*;

fn enabled(retired: Option<&str>, moved: Option<ReleaseMove>) -> Result<Enabled, Box<dyn std::error::Error>> {
    Ok(Enabled {
        schedule: JobSchedule::try_from("0 5 * * *")?,
        retired: retired.map(str::to_owned),
        moved,
    })
}

#[test]
fn a_report_names_the_retired_predecessor_and_how_current_and_previous_moved() -> Result<(), Box<dyn std::error::Error>>
{
    let moved = ReleaseMove {
        current_from: Some(Version::new(1, 0, 0)),
        current_to: Version::new(2, 0, 0),
        previous_from: None,
        previous_to: Some(Version::new(1, 0, 0)),
    };

    let report = EnablingReport::from(&enabled(Some("nightly"), Some(moved))?);

    assert_eq!(
        report.to_string(),
        "Its schedule is 0 5 * * *. Its predecessor nightly was retired. Current moved from 1.0.0 to 2.0.0; previous moved from none to 1.0.0."
    );
    assert_eq!(
        serde_json::to_value(&report)?,
        serde_json::json!({
            "schedule": "0 5 * * *",
            "retired": "nightly",
            "current": {"from": "1.0.0", "to": "2.0.0"},
            "previous": {"from": null, "to": "1.0.0"},
        })
    );
    Ok(())
}

#[test]
fn an_enabling_that_retired_nothing_and_moved_nothing_says_only_its_schedule() -> Result<(), Box<dyn std::error::Error>>
{
    let report = EnablingReport::from(&enabled(None, None)?);

    assert_eq!(report.to_string(), "Its schedule is 0 5 * * *.");
    assert_eq!(report.current, None);
    assert_eq!(report.previous, None);
    Ok(())
}
