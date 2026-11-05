use super::*;

#[test]
fn a_report_of_nothing_found_is_unchanged() {
    assert!(GraphReconcileReport::default().is_unchanged());
    assert!(
        !GraphReconcileReport {
            notes_read: 1,
            ..GraphReconcileReport::default()
        }
        .is_unchanged()
    );
}
