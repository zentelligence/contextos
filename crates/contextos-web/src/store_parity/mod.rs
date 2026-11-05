//! One contract suite for the two package stores.
//!
//! Every case is written once against a world trait and run twice, as `host::<case>` and `vault::<case>`, over the real
//! adapters: the host store over a temporary folder, and a vault's store over a temporary vault read through a real
//! in-process server. Where the stores are documented to differ, the difference is pinned by a test of its own in
//! `divergences` rather than skipped.

mod crash_cases;
mod divergences;
mod faulty_io;
mod host_world;
mod install_cases;
mod io_cases;
#[cfg(unix)]
mod io_links;
mod io_support;
mod removal_cases;
mod store_support;
mod vault_world;
mod worlds;

/// Stamps each named case as a test that runs it for `$world`.
macro_rules! stamp {
    ($cases:ident, $world:ty; $($case:ident),+ $(,)?) => {
        $(
            #[tokio::test]
            async fn $case() -> Result<(), $crate::store_parity::worlds::BoxError> {
                $crate::store_parity::$cases::$case::<$world>().await
            }
        )+
    };
}

/// Stamps the full list of I/O cases for `$world`.
macro_rules! stamp_io_cases {
    ($world:ty) => {
        stamp!(
            io_cases, $world;
            a_listing_reports_files_with_their_sizes_and_folders,
            a_listing_of_a_store_or_folder_that_does_not_exist_is_not_found,
            a_tree_listing_reports_every_entry_below_with_paths_relative_to_the_folder,
            a_tree_listing_stops_rather_than_return_more_than_the_limit,
            a_read_returns_the_exact_bytes_of_text_and_of_binary_files,
            a_read_past_the_limit_is_refused_before_the_content_is_returned,
            a_read_of_a_folder_is_refused_and_of_a_missing_file_is_not_found,
            a_new_file_is_written_with_exactly_the_bytes_given_and_leaves_no_temporary_file,
            a_new_file_never_replaces_an_existing_entry,
            a_new_file_in_a_folder_that_does_not_exist_is_not_found_and_creates_nothing,
            replace_creates_an_absent_file_and_swaps_a_present_one_whole,
            replace_creates_the_store_folder_when_the_store_does_not_exist_yet,
            creating_folders_makes_the_missing_ones_and_accepts_an_existing_one,
            creating_a_folder_where_a_file_stands_is_refused_as_already_existing,
            a_whole_folder_is_renamed_with_its_contents_in_one_step,
            a_rename_never_replaces_the_destination_and_leaves_the_source_in_place,
            renaming_something_that_is_not_there_is_not_found,
            removing_a_tree_deletes_it_and_everything_below_and_reports_whether_it_was_there,
            removing_a_tree_that_is_not_there_reports_nothing_removed_even_below_a_missing_folder,
            the_store_lock_is_exclusive_released_when_dropped_and_is_the_lock_file_inside_the_store,
            two_handles_on_one_store_contend_for_the_one_lock_and_so_does_a_holder_outside_the_adapter,
            the_store_lock_does_not_gate_the_changes_the_lock_holder_makes,
        );
        #[cfg(unix)]
        stamp!(
            io_links, $world;
            nothing_is_read_written_renamed_removed_or_created_through_a_link_on_the_way,
            a_file_that_is_a_link_is_neither_read_nor_replaced,
            a_link_inside_a_tree_being_removed_is_deleted_without_touching_what_it_points_to,
        );
    };
}

macro_rules! stamp_install_cases {
    ($world:ty) => {
        stamp!(
            install_cases, $world;
            a_new_release_is_installed_recorded_and_left_with_no_staging_folder,
            installing_the_same_release_again_changes_nothing,
            a_version_offered_again_with_another_digest_is_refused_and_the_installed_release_is_kept,
            a_request_whose_files_the_manifest_does_not_describe_is_refused_and_writes_nothing,
            installing_a_recorded_release_that_is_removed_incomplete_or_damaged_repairs_it,
            a_release_that_matches_its_manifest_reads_as_verified_and_a_folder_that_is_gone_as_removed,
            a_release_missing_a_listed_file_an_unlisted_entry_or_its_manifest_reads_as_incomplete,
            a_release_whose_file_no_longer_hashes_to_its_manifest_reads_as_damaged,
            a_release_that_exceeds_the_read_limits_reads_as_unverifiable_never_as_removed,
            an_empty_store_has_no_ledger_and_lists_nothing,
            a_store_with_releases_and_no_ledger_is_unusable_and_refuses_every_change,
            a_ledger_that_cannot_be_read_is_unusable_and_refuses_every_change_and_is_left_as_found,
            a_recorded_release_whose_folder_is_gone_is_read_as_removed_with_no_record,
            a_release_folder_the_ledger_does_not_record_blocks_the_install_of_that_version,
            a_missing_ledger_is_rebuilt_from_the_releases_present_and_changes_work_again,
            a_ledger_that_is_present_is_not_rebuilt_whether_or_not_it_can_be_read,
            a_ledger_is_not_rebuilt_while_a_release_does_not_read_whole,
            a_ledger_rebuild_records_a_release_whose_content_is_damaged_but_whose_manifest_is_sound,
            a_ledger_entry_records_the_interface_the_release_declares,
        );
    };
}

macro_rules! stamp_removal_cases {
    ($world:ty) => {
        stamp!(
            removal_cases, $world;
            the_store_lists_its_releases_and_what_keeps_them,
            a_release_no_rule_keeps_is_removed_and_recorded_by_request,
            a_release_a_job_file_names_is_refused_as_in_use_and_nothing_changes,
            the_current_release_is_refused_as_in_use_unless_forced,
            a_forced_removal_of_a_release_a_job_file_names_is_recorded_as_forced,
            a_state_file_that_is_not_valid_is_refused_with_its_own_code_for_a_listing_and_a_removal,
            a_store_another_operation_holds_refuses_a_removal_as_store_locked,
            a_release_that_is_not_there_or_a_name_or_version_that_is_not_usable_is_refused_by_its_own_code,
            removing_the_last_release_of_a_package_removes_its_folder,
            removing_a_future_release_rewrites_the_recorded_future_to_the_newest_release_left,
            a_removal_runs_retention_and_reports_the_releases_only_the_removed_one_kept,
            retention_removes_what_nothing_keeps_and_records_the_cause_as_retention,
            retention_keeps_a_release_a_job_file_names,
            retention_keeps_every_release_of_a_workflow_whose_job_file_cannot_be_read,
            a_job_file_that_names_no_exact_release_keeps_nothing,
            a_store_with_releases_and_no_state_file_runs_no_retention_and_creates_no_state_file,
            retention_never_touches_a_release_that_does_not_read_whole,
            a_store_another_operation_holds_is_not_changed_by_retention_and_the_run_fails,
            retention_repairs_a_recorded_future_that_names_no_whole_release_even_when_it_removes_nothing,
            retention_repairs_a_recorded_future_that_names_a_damaged_release,
            retention_rewrites_the_future_a_removal_changed,
            releases_set_aside_leave_their_names_at_once_with_nothing_recorded_until_settled,
            settling_a_batch_renames_back_what_is_kept_and_records_and_deletes_the_rest,
            settling_a_batch_for_a_requested_removal_records_the_cause_the_request_has,
            setting_aside_a_release_the_ledger_does_not_record_is_refused_before_any_release_moves,
            a_removal_is_renamed_aside_before_it_is_recorded_and_deleted_after,
            recovery_deletes_a_staging_folder_and_leaves_every_release_alone,
            recovery_renames_back_a_removal_folder_the_ledger_records_no_removal_for,
            recovery_deletes_a_removal_folder_the_ledger_records_a_removal_for,
            recovery_deletes_a_removal_folder_whose_release_has_been_put_back_in_place,
            recovery_leaves_a_removal_folder_whose_manifest_cannot_be_read_untouched,
            entries_that_are_not_releases_are_reported_as_unrecognised_and_recovery_leaves_them,
            the_ledger_and_the_folders_agree_after_an_install_a_removal_and_a_retention,
        );
    };
}

/// Stamps one crash loop per scenario for `$world`.
macro_rules! stamp_crash_loops {
    ($world:ty) => {
        use crate::store_parity::crash_cases::{Scenario, crash_at_every_step};

        #[tokio::test]
        async fn a_crash_at_any_step_of_the_first_install_into_a_new_store_leaves_no_partial_release()
        -> Result<(), $crate::store_parity::worlds::BoxError> {
            crash_at_every_step::<$world>(Scenario::InstallIntoEmptyStore).await.map(|_| ())
        }

        #[tokio::test]
        async fn a_crash_at_any_step_of_an_install_beside_another_release_leaves_no_partial_release()
        -> Result<(), $crate::store_parity::worlds::BoxError> {
            crash_at_every_step::<$world>(Scenario::InstallBesideAnother).await.map(|_| ())
        }

        #[tokio::test]
        async fn a_crash_at_any_step_of_repairing_a_removed_release_leaves_no_partial_release()
        -> Result<(), $crate::store_parity::worlds::BoxError> {
            crash_at_every_step::<$world>(Scenario::RepairRemovedRelease).await.map(|_| ())
        }

        #[tokio::test]
        async fn a_crash_at_any_step_of_repairing_an_incomplete_release_leaves_no_partial_release()
        -> Result<(), $crate::store_parity::worlds::BoxError> {
            crash_at_every_step::<$world>(Scenario::RepairIncompleteRelease).await.map(|_| ())
        }

        #[tokio::test]
        async fn a_crash_at_any_step_of_repairing_a_damaged_release_leaves_no_partial_release()
        -> Result<(), $crate::store_parity::worlds::BoxError> {
            crash_at_every_step::<$world>(Scenario::RepairDamagedRelease).await.map(|_| ())
        }

        #[tokio::test]
        async fn a_crash_at_any_step_of_a_removal_leaves_the_release_whole_or_recorded_removed()
        -> Result<(), $crate::store_parity::worlds::BoxError> {
            crash_at_every_step::<$world>(Scenario::RemoveByRequest).await.map(|_| ())
        }

        #[tokio::test]
        async fn a_crash_at_any_step_of_setting_aside_and_settling_a_retention_batch_leaves_each_release_whole_or_recorded_removed()
        -> Result<(), $crate::store_parity::worlds::BoxError> {
            crash_at_every_step::<$world>(Scenario::SetAsideAndSettle).await.map(|_| ())
        }

        #[tokio::test]
        async fn a_crash_at_any_step_of_a_retention_pass_leaves_each_release_whole_or_recorded_removed()
        -> Result<(), $crate::store_parity::worlds::BoxError> {
            crash_at_every_step::<$world>(Scenario::RetentionPass).await.map(|_| ())
        }
    };
}

mod host {
    stamp_io_cases!(crate::store_parity::host_world::HostWorld);
    stamp_install_cases!(crate::store_parity::host_world::HostWorld);
    stamp_removal_cases!(crate::store_parity::host_world::HostWorld);
    stamp_crash_loops!(crate::store_parity::host_world::HostWorld);
}

mod vault {
    stamp_io_cases!(crate::store_parity::vault_world::VaultWorld);
    stamp_install_cases!(crate::store_parity::vault_world::VaultWorld);
    stamp_removal_cases!(crate::store_parity::vault_world::VaultWorld);
    stamp_crash_loops!(crate::store_parity::vault_world::VaultWorld);
}
