use super::*;

const TASKS_TOML: &str = "\
[[stage]]
state = \"backlog\"
label = \"Backlog\"
icon = \"inbox\"
bucket = \"backlog\"

[[stage]]
state = \"doing\"
label = \"In progress\"
bucket = \"now\"

[hierarchy]
depth_labels = [\"Initiative\", \"Action\"]
";

#[test]
fn a_configured_file_parses_every_field_including_the_optional_ones() -> Result<(), Box<dyn std::error::Error>> {
    let config: TasksConfig = toml::from_str(TASKS_TOML)?;
    assert_eq!(config.stage.len(), 2);
    let backlog = &config.stage[0];
    assert_eq!(backlog.state, "backlog");
    assert_eq!(backlog.display_label(), "Backlog");
    assert_eq!(backlog.icon.as_deref(), Some("inbox"));
    assert_eq!(backlog.bucket, Some(Bucket::Backlog));
    assert_eq!(config.hierarchy.label_for_depth(0), Some("Initiative"));
    assert_eq!(config.hierarchy.label_for_depth(1), Some("Action"));
    assert_eq!(config.hierarchy.label_for_depth(2), None);
    Ok(())
}

#[test]
fn a_stage_with_no_label_displays_its_own_state_verbatim() -> Result<(), Box<dyn std::error::Error>> {
    let config: TasksConfig = toml::from_str(TASKS_TOML)?;
    let doing = &config.stage[1];
    assert_eq!(doing.label.as_deref(), Some("In progress"));
    let unlabelled: TasksConfig = toml::from_str("[[stage]]\nstate = \"waiting\"\n")?;
    assert_eq!(unlabelled.stage[0].display_label(), "waiting");
    Ok(())
}

#[test]
fn a_stage_with_no_configured_bucket_has_none() -> Result<(), Box<dyn std::error::Error>> {
    let config: TasksConfig = toml::from_str("[[stage]]\nstate = \"waiting\"\n")?;
    assert_eq!(config.stage[0].bucket, None);
    Ok(())
}

#[test]
fn an_unknown_bucket_value_fails_to_parse_rather_than_guess() {
    let result: Result<TasksConfig, _> = toml::from_str("[[stage]]\nstate = \"waiting\"\nbucket = \"urgent\"\n");
    assert!(result.is_err());
}

fn states(stages: &[StageEntry]) -> Vec<&str> {
    stages.iter().map(|stage| stage.state.as_str()).collect()
}

#[test]
fn an_empty_file_defaults_to_the_seven_action_states_in_lifecycle_order() -> Result<(), Box<dyn std::error::Error>> {
    let config: TasksConfig = toml::from_str("")?;
    assert_eq!(
        states(&config.stage),
        vec!["backlog", "next", "doing", "waiting", "review", "done", "dropped"]
    );
    assert!(
        config.hierarchy.depth_labels.is_empty(),
        "`config.hierarchy.depth_labels` should be empty, got {:?}",
        config.hierarchy.depth_labels
    );
    Ok(())
}

#[test]
fn the_shipped_decision_and_question_types_each_have_their_own_lifecycle() {
    let config = TasksConfig::default();
    assert_eq!(
        states(config.lifecycle("action")),
        ["backlog", "next", "doing", "waiting", "review", "done", "dropped"]
    );
    assert_eq!(states(config.lifecycle("decision")), ["proposed", "adopted", "dropped"]);
    assert_eq!(states(config.lifecycle("question")), ["open", "answered", "dropped"]);
}

#[test]
fn a_type_is_found_whatever_its_case_and_an_undeclared_or_missing_type_uses_the_shared_stages() {
    let config = TasksConfig::default();
    assert_eq!(states(config.lifecycle("Decision")), ["proposed", "adopted", "dropped"]);
    assert_eq!(states(config.lifecycle("chore")), states(&config.stage));
    assert_eq!(states(config.lifecycle("")), states(&config.stage));
}

#[test]
fn a_type_that_declares_no_stages_of_its_own_uses_the_shared_stages() -> Result<(), Box<dyn std::error::Error>> {
    let config: TasksConfig = toml::from_str(
        "[[stage]]\nstate = \"todo\"\n\n[[stage]]\nstate = \"done\"\n\n\
         [[task_type]]\nvalue = \"action\"\n\n\
         [[task_type]]\nvalue = \"question\"\n[[task_type.stage]]\nstate = \"open\"\n[[task_type.stage]]\nstate = \"answered\"\n",
    )?;
    assert_eq!(states(config.lifecycle("action")), ["todo", "done"]);
    assert_eq!(states(config.lifecycle("question")), ["open", "answered"]);
    Ok(())
}

#[test]
fn the_shipped_focus_buckets_are_per_type_and_dropped_is_in_none() {
    let config = TasksConfig::default();
    let keys = |bucket| -> Vec<String> {
        config
            .stage_keys_in_bucket(bucket)
            .iter()
            .map(ToString::to_string)
            .collect()
    };
    assert_eq!(keys(Bucket::Backlog), ["action:backlog", "action:next"]);
    assert_eq!(keys(Bucket::Now), ["action:doing"]);
    assert_eq!(
        keys(Bucket::NeedsCall),
        ["action:waiting", "action:review", "decision:proposed", "question:open"]
    );
    assert_eq!(
        keys(Bucket::Done),
        ["action:done", "decision:adopted", "question:answered"]
    );
    for task_type in ["action", "decision", "question"] {
        assert_eq!(config.bucket_of(task_type, "dropped"), None, "{task_type}");
    }
}

#[test]
fn a_stage_with_no_bucket_is_in_no_bucket() -> Result<(), Box<dyn std::error::Error>> {
    let config: TasksConfig = toml::from_str("[[stage]]\nstate = \"someday\"\n\n[[task_type]]\nvalue = \"action\"\n")?;
    for bucket in [Bucket::Now, Bucket::Backlog, Bucket::NeedsCall, Bucket::Done] {
        assert!(config.stage_keys_in_bucket(bucket).is_empty(), "{bucket:?}");
    }
    Ok(())
}

#[test]
fn a_stage_key_reads_back_from_its_text_and_a_bare_state_matches_every_type() {
    let config = TasksConfig::default();
    let decision = StageKey::from("decision:dropped");
    assert_eq!(decision.to_string(), "decision:dropped");
    assert!(config.in_stage(&decision, "decision", "dropped"));
    assert!(config.in_stage(&decision, "Decision", "dropped"));
    assert!(!config.in_stage(&decision, "action", "dropped"));
    let bare = StageKey::from("dropped");
    assert_eq!(bare.to_string(), "dropped");
    assert!(config.in_stage(&bare, "action", "dropped") && config.in_stage(&bare, "question", "dropped"));
    assert!(!config.in_stage(&bare, "action", "done"));
}

#[test]
fn a_task_with_no_type_or_an_undeclared_one_follows_the_shared_stages_and_has_its_own_stage_key() {
    let config = TasksConfig::default();
    assert_eq!(config.bucket_of("", "doing"), Some(Bucket::Now));
    assert_eq!(config.bucket_of("bug", "waiting"), Some(Bucket::NeedsCall));
    assert!(config.is_open("bug", "doing"));
    assert!(!config.is_declared("bug") && !config.is_declared("") && config.is_declared("Action"));

    let other = StageKey::from(":doing");
    assert_eq!(other, StageKey::other("doing"));
    assert_eq!(other.to_string(), ":doing");
    assert!(config.in_stage(&other, "", "doing"));
    assert!(config.in_stage(&other, "bug", "doing"));
    assert!(
        !config.in_stage(&other, "action", "doing"),
        "a declared type is not one of the others"
    );
    let keys: Vec<String> = config
        .stage_keys_in_bucket(Bucket::Now)
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(
        keys,
        ["action:doing"],
        "the others' keys are added only where such tasks exist"
    );
}

#[test]
fn label_for_state_reads_the_tasks_own_type_and_falls_back_to_the_raw_state() {
    let config = TasksConfig::default();
    assert_eq!(config.label_for_state("action", "doing"), "Doing");
    assert_eq!(config.label_for_state("decision", "adopted"), "Adopted");
    assert_eq!(config.label_for_state("question", "answered"), "Answered");
    assert_eq!(config.label_for_state("decision", "doing"), "doing");
    assert_eq!(config.label_for_state("action", "someday"), "someday");
}

#[test]
fn a_file_with_a_hierarchy_but_no_stage_entries_still_defaults_its_stages() -> Result<(), Box<dyn std::error::Error>> {
    let config: TasksConfig = toml::from_str("[hierarchy]\ndepth_labels = [\"Theme\"]\n")?;
    assert_eq!(config.stage.len(), 7);
    assert_eq!(config.hierarchy.label_for_depth(0), Some("Theme"));
    Ok(())
}

#[test]
fn malformed_toml_falls_back_to_the_default_config() {
    let config: TasksConfig = toml::from_str("stage = \"not a table\"").unwrap_or_default();
    assert_eq!(config, TasksConfig::default());
}

#[test]
fn default_matches_the_toml_parsed_default() -> Result<(), Box<dyn std::error::Error>> {
    let from_empty: TasksConfig = toml::from_str("")?;
    assert_eq!(from_empty, TasksConfig::default());
    Ok(())
}

#[test]
fn a_task_is_open_in_any_state_of_its_type_that_does_not_close_it() {
    let config = TasksConfig::default();
    for (task_type, state) in [
        ("action", "backlog"),
        ("action", "next"),
        ("action", "doing"),
        ("action", "waiting"),
        ("action", "review"),
        ("decision", "proposed"),
        ("question", "open"),
    ] {
        assert!(config.is_open(task_type, state), "{task_type} {state}");
    }
}

#[test]
fn a_task_is_not_open_when_closed_or_in_a_state_its_type_does_not_declare() {
    let config = TasksConfig::default();
    for (task_type, state) in [
        ("action", "done"),
        ("action", "dropped"),
        ("decision", "adopted"),
        ("decision", "dropped"),
        ("question", "answered"),
        ("question", "dropped"),
        ("decision", "pending"),
        ("question", "doing"),
        ("action", "made-up"),
        ("action", ""),
    ] {
        assert!(!config.is_open(task_type, state), "{task_type} {state}");
    }
}

#[test]
fn a_closing_state_says_which_date_it_stamps() {
    let config = TasksConfig::default();
    assert_eq!(config.closing("action", "done"), Some(Closes::Completed));
    assert_eq!(config.closing("decision", "adopted"), Some(Closes::Completed));
    assert_eq!(config.closing("question", "answered"), Some(Closes::Completed));
    for task_type in ["action", "decision", "question"] {
        assert_eq!(
            config.closing(task_type, "dropped"),
            Some(Closes::Cancelled),
            "{task_type}"
        );
    }
    assert_eq!(config.closing("action", "doing"), Some(Closes::Never));
    assert_eq!(
        config.closing("decision", "done"),
        None,
        "a state the type does not declare"
    );
}

#[test]
fn a_stage_configured_without_closes_takes_the_shipped_one_for_its_state_and_can_be_kept_open()
-> Result<(), Box<dyn std::error::Error>> {
    let config: TasksConfig = toml::from_str(
        "[[stage]]\nstate = \"done\"\n\n[[stage]]\nstate = \"dropped\"\n\n[[stage]]\nstate = \"shipped\"\ncloses = \"completed\"\n\n\
         [[stage]]\nstate = \"answered\"\ncloses = \"never\"\n\n[[stage]]\nstate = \"someday\"\n",
    )?;
    assert_eq!(config.closing("action", "done"), Some(Closes::Completed));
    assert_eq!(config.closing("action", "dropped"), Some(Closes::Cancelled));
    assert_eq!(config.closing("action", "shipped"), Some(Closes::Completed));
    assert_eq!(config.closing("action", "answered"), Some(Closes::Never));
    assert!(
        config.is_open("action", "someday"),
        "a state with no bucket that does not close is open"
    );
    Ok(())
}

#[test]
fn a_new_task_starts_in_its_types_initial_state_else_its_first_open_one() -> Result<(), Box<dyn std::error::Error>> {
    let config = TasksConfig::default();
    assert_eq!(config.initial_state("action"), Some("next"));
    assert_eq!(config.initial_state("decision"), Some("proposed"));
    assert_eq!(config.initial_state("question"), Some("open"));
    assert_eq!(
        config.initial_state("chore"),
        Some("backlog"),
        "an undeclared type uses the shared stages"
    );

    let unset: TasksConfig = toml::from_str(
        "[[stage]]\nstate = \"done\"\n\n[[stage]]\nstate = \"todo\"\n\n[[task_type]]\nvalue = \"action\"\n",
    )?;
    assert_eq!(unset.initial_state("action"), Some("todo"));
    Ok(())
}

#[test]
fn an_absent_vocabulary_falls_back_to_the_shipped_task_types_and_priorities() -> Result<(), Box<dyn std::error::Error>>
{
    let config = toml::from_str::<TasksConfig>(TASKS_TOML)?;
    let types: Vec<(&str, Option<&str>)> = config
        .task_type
        .iter()
        .map(|entry| (entry.value.as_str(), entry.prefix.as_deref()))
        .collect();
    assert_eq!(
        types,
        [("action", Some("A")), ("decision", Some("D")), ("question", Some("Q"))]
    );
    let decision = config.task_type("decision").ok_or("no decision type")?;
    assert_eq!(decision.initial.as_deref(), Some("proposed"));
    let priorities: Vec<&str> = config.priority.iter().map(|entry| entry.value.as_str()).collect();
    assert_eq!(priorities, ["highest", "high", "medium", "normal", "low"]);
    Ok(())
}

#[test]
fn a_declared_vocabulary_replaces_the_shipped_one() -> Result<(), Box<dyn std::error::Error>> {
    let config = toml::from_str::<TasksConfig>(
        "[[task_type]]\nvalue = \"chore\"\nlabel = \"Chore\"\nprefix = \"C\"\n\n[[priority]]\nvalue = \"urgent\"\n\n[[priority]]\nvalue = \"later\"\n",
    )?;
    assert_eq!(config.task_type.len(), 1);
    assert_eq!(config.task_type[0].display_label(), "Chore");
    assert_eq!(config.priority[1].display_label(), "later");
    Ok(())
}

#[test]
fn priority_ranks_follow_the_declared_order_with_undeclared_and_blank_values_last() {
    let config = TasksConfig::default();
    let mut values = vec!["", "low", "normal", "someday", "highest", "high", "medium"];
    values.sort_by_key(|value| config.priority_sort_key(value));
    assert_eq!(values, ["highest", "high", "medium", "normal", "low", "someday", ""]);
}

#[test]
fn a_valid_configuration_passes_validation() {
    assert_eq!(TasksConfig::default().validate(), Ok(()));
}

#[test]
fn validation_names_the_first_problem_in_each_list() {
    let mut config = TasksConfig::default();
    config.stage.push(config.stage[0].clone());
    assert_eq!(
        config.validate(),
        Err(TasksConfigError::DuplicateStage {
            lifecycle: "the shared stages".to_owned(),
            state: "backlog".to_owned()
        })
    );

    let mut config = TasksConfig::default();
    config.stage.clear();
    assert_eq!(config.validate(), Err(TasksConfigError::NoStages("action".to_owned())));

    let mut config = TasksConfig::default();
    config.task_type[1].initial = Some("parked".to_owned());
    assert_eq!(
        config.validate(),
        Err(TasksConfigError::InvalidInitialState {
            task_type: "decision".to_owned(),
            state: "parked".to_owned()
        })
    );

    let mut config = TasksConfig::default();
    config.task_type[1].initial = Some("adopted".to_owned());
    assert_eq!(
        config.validate(),
        Err(TasksConfigError::InvalidInitialState {
            task_type: "decision".to_owned(),
            state: "adopted".to_owned()
        }),
        "a new task cannot start closed"
    );

    let mut config = TasksConfig::default();
    let repeated = config.task_type[2].stage[0].clone();
    config.task_type[2].stage.push(repeated);
    assert_eq!(
        config.validate(),
        Err(TasksConfigError::DuplicateStage {
            lifecycle: "the question stages".to_owned(),
            state: "open".to_owned()
        })
    );

    let mut config = TasksConfig::default();
    config.task_type[1].stage.retain(|stage| stage.state != "proposed");
    config.task_type[1].initial = None;
    assert_eq!(
        config.validate(),
        Err(TasksConfigError::NoOpenState("the decision stages".to_owned()))
    );

    let mut config = TasksConfig::default();
    config.task_type[1].stage[0].state = "Pro posed".to_owned();
    assert_eq!(
        config.validate(),
        Err(TasksConfigError::InvalidValue("Pro posed".to_owned()))
    );

    let mut config = TasksConfig::default();
    config.task_type[2].prefix = Some("q-1".to_owned());
    assert_eq!(
        config.validate(),
        Err(TasksConfigError::InvalidPrefix("q-1".to_owned()))
    );

    let mut config = TasksConfig::default();
    config.task_type[2].prefix = Some("A".to_owned());
    assert_eq!(
        config.validate(),
        Err(TasksConfigError::DuplicatePrefix("A".to_owned()))
    );

    let mut config = TasksConfig::default();
    config.priority.push(config.priority[0].clone());
    assert_eq!(
        config.validate(),
        Err(TasksConfigError::DuplicatePriority("highest".to_owned()))
    );

    let mut config = TasksConfig::default();
    config.stage[0].state = "Back log".to_owned();
    assert_eq!(
        config.validate(),
        Err(TasksConfigError::InvalidValue("Back log".to_owned()))
    );
}

#[test]
fn a_closes_value_outside_the_three_is_refused_by_name() {
    assert_eq!(
        Closes::try_from("sometimes"),
        Err(TasksConfigError::UnknownCloses("sometimes".to_owned()))
    );
    assert_eq!(
        TasksConfigError::UnknownCloses("sometimes".to_owned()).to_string(),
        "`sometimes` is not a close: choose never, completed, or cancelled"
    );
}

#[test]
fn a_blank_type_is_reported_as_a_blank_value_before_any_missing_stages() {
    let mut config = TasksConfig::default();
    config.stage.clear();
    config.task_type[0].value = String::new();
    assert_eq!(config.validate(), Err(TasksConfigError::InvalidValue(String::new())));
}

#[test]
fn the_shared_stages_may_be_empty_when_every_type_has_its_own() {
    let mut config = TasksConfig::default();
    config.stage.clear();
    config.task_type.retain(|entry| !entry.stage.is_empty());
    assert_eq!(config.validate(), Ok(()));
}

#[test]
fn writing_the_configuration_keeps_the_files_own_comments() -> Result<(), Box<dyn std::error::Error>> {
    let source = "# Shared with the team.\n# Edited through Vault Settings.\n\n[[stage]]\nstate = \"old\"\n";
    let mut config = TasksConfig::default();
    config.hierarchy.depth_labels = vec!["Initiative".to_owned(), "Action".to_owned()];
    let written = config.written_over(source)?;
    assert!(written.contains("# Shared with the team."), "{written}");
    assert!(written.contains("# Edited through Vault Settings."), "{written}");
    assert!(!written.contains("\"old\""), "{written}");
    let reread = toml::from_str::<TasksConfig>(&written)?;
    assert_eq!(reread, config);
    Ok(())
}

#[test]
fn a_comment_written_above_a_replaced_section_is_kept() -> Result<(), Box<dyn std::error::Error>> {
    let source =
        "# The stages we use.\n[[stage]]\nstate = \"old\"\n\n# Most urgent first.\n[[priority]]\nvalue = \"x\"\n";
    let written = TasksConfig::default().written_over(source)?;
    assert!(written.contains("# The stages we use.\n[[stage]]"), "{written}");
    assert!(written.contains("# Most urgent first.\n[[priority]]"), "{written}");
    Ok(())
}

#[test]
fn an_unknown_key_anywhere_is_rejected_rather_than_dropped_on_the_next_save() {
    for source in [
        "[[stage]]\nstate = \"next\"\nshade = \"red\"\n",
        "[[task_type]]\nvalue = \"action\"\nicon = \"x\"\n",
        "[[priority]]\nvalue = \"high\"\nweight = 3\n",
        "[hierarchy]\ndepth_labels = []\ncollapsed = true\n",
        "stages = []\n",
        "[[task_type]]\nvalue = \"decision\"\nholding_state = \"pending\"\n",
        "[[task_type]]\nvalue = \"decision\"\nreview = \"weekly\"\n",
        "[[task_type]]\nvalue = \"decision\"\n[[task_type.stage]]\nstate = \"proposed\"\nshade = \"red\"\n",
        "[[stage]]\nstate = \"done\"\ncloses = \"finished\"\n",
    ] {
        assert!(toml::from_str::<TasksConfig>(source).is_err(), "{source}");
    }
}

#[test]
fn a_stage_takes_the_colour_it_is_configured_with() -> Result<(), Box<dyn std::error::Error>> {
    let config: TasksConfig = toml::from_str("[[stage]]\nstate = \"review\"\ncolour = \"red\"\n")?;

    assert_eq!(config.stage[0].colour, Some(StageColour::Red));
    assert_eq!(config.stage_colour("action", "review"), StageColour::Red);
    Ok(())
}

#[test]
fn the_shipped_stages_each_have_a_colour_and_review_is_not_left_plain() {
    let config = TasksConfig::default();

    assert_eq!(config.stage_colour("action", "backlog"), StageColour::Grey);
    assert_eq!(config.stage_colour("action", "next"), StageColour::Blue);
    assert_eq!(config.stage_colour("action", "doing"), StageColour::Green);
    assert_eq!(config.stage_colour("action", "waiting"), StageColour::Amber);
    assert_eq!(config.stage_colour("action", "review"), StageColour::Purple);
    assert_eq!(config.stage_colour("action", "done"), StageColour::Grey);
    assert_eq!(config.stage_colour("action", "dropped"), StageColour::Faint);
    assert_eq!(config.stage_colour("decision", "proposed"), StageColour::Teal);
    assert_eq!(config.stage_colour("decision", "adopted"), StageColour::Grey);
    assert_eq!(config.stage_colour("question", "open"), StageColour::Amber);
    assert_eq!(config.stage_colour("question", "answered"), StageColour::Grey);
}

#[test]
fn a_stage_configured_without_a_colour_takes_the_shipped_one_for_its_state_else_grey()
-> Result<(), Box<dyn std::error::Error>> {
    let config: TasksConfig =
        toml::from_str("[[stage]]\nstate = \"review\"\n\n[[stage]]\nstate = \"triage\"\nlabel = \"Triage\"\n")?;

    assert_eq!(config.stage_colour("action", "review"), StageColour::Purple);
    assert_eq!(config.stage_colour("action", "triage"), StageColour::Grey);
    assert_eq!(config.stage_colour("action", "not-a-stage"), StageColour::Grey);
    Ok(())
}

#[test]
fn an_unknown_colour_fails_to_parse_rather_than_guess() {
    assert!(toml::from_str::<TasksConfig>("[[stage]]\nstate = \"review\"\ncolour = \"chartreuse\"\n").is_err());
}

#[test]
fn every_colour_has_a_stable_name_and_a_label() {
    for colour in StageColour::ALL {
        assert!(!colour.label().is_empty(), "{colour:?} has no label");
        assert_eq!(StageColour::try_from(colour.slug()), Ok(colour));
    }
    assert!(StageColour::try_from("chartreuse").is_err());
}
