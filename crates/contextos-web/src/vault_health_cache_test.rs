use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError};

use super::*;
use crate::vault_health::{DoctorResult, HealthSummary};

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// A one-check report that passes or fails.
fn report(status: &str) -> Result<HealthReport, Box<dyn std::error::Error>> {
    let wire = serde_json::json!({
        "checks": [{"kind": "managed_indexes", "status": status, "message": "m", "auto_fixable": status == "fail"}]
    });
    Ok(HealthReport::try_from(serde_json::from_value::<DoctorResult>(wire)?)?)
}

/// A source that answers from a script and counts how often it was asked, optionally pausing each answer so a test
/// can hold a fetch in flight.
struct ScriptedSource {
    answers: Mutex<VecDeque<Result<HealthReport, HealthError>>>,
    calls: AtomicUsize,
    delay: Duration,
}

impl ScriptedSource {
    fn new(answers: Vec<Result<HealthReport, HealthError>>) -> Self {
        Self {
            answers: Mutex::new(answers.into()),
            calls: AtomicUsize::new(0),
            delay: Duration::ZERO,
        }
    }

    fn slow(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }
}

impl HealthSource for ScriptedSource {
    async fn assess(&self, _vault: &str) -> Result<HealthReport, HealthError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        tokio::time::sleep(self.delay).await;
        self.answers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop_front()
            .unwrap_or(Err(HealthError::Unreachable))
    }
}

const TTL: Duration = Duration::from_secs(60);
const TIMEOUT: Duration = Duration::from_secs(30);

#[tokio::test(start_paused = true)]
async fn a_second_read_inside_the_ttl_reuses_the_first_assessment() -> TestResult {
    let cache = HealthCache::new(ScriptedSource::new(vec![Ok(report("pass")?)]), TTL, TIMEOUT);

    let first = cache.current("v").await?;
    tokio::time::advance(Duration::from_secs(59)).await;
    let second = cache.current("v").await?;

    assert_eq!(cache.source.calls.load(Ordering::SeqCst), 1);
    assert!(Arc::ptr_eq(&first.report, &second.report));
    assert_eq!(second.age, Duration::from_secs(59));
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn a_read_after_the_ttl_assesses_again() -> TestResult {
    let cache = HealthCache::new(
        ScriptedSource::new(vec![Ok(report("pass")?), Ok(report("fail")?)]),
        TTL,
        TIMEOUT,
    );

    cache.current("v").await?;
    tokio::time::advance(TTL).await;
    let second = cache.current("v").await?;

    assert_eq!(cache.source.calls.load(Ordering::SeqCst), 2);
    assert_ne!(second.report.summary(), HealthSummary::Healthy);
    assert_eq!(second.age, Duration::ZERO);
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn refresh_assesses_again_even_inside_the_ttl() -> TestResult {
    let cache = HealthCache::new(
        ScriptedSource::new(vec![Ok(report("pass")?), Ok(report("fail")?)]),
        TTL,
        TIMEOUT,
    );

    cache.current("v").await?;
    let refreshed = cache.refresh("v").await?;

    assert_eq!(cache.source.calls.load(Ordering::SeqCst), 2);
    assert_ne!(refreshed.report.summary(), HealthSummary::Healthy);
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn invalidate_forces_the_next_read_to_assess_again() -> TestResult {
    let cache = HealthCache::new(
        ScriptedSource::new(vec![Ok(report("fail")?), Ok(report("pass")?)]),
        TTL,
        TIMEOUT,
    );

    cache.current("v").await?;
    cache.invalidate("v").await;
    let after = cache.current("v").await?;

    assert_eq!(after.report.summary(), HealthSummary::Healthy);
    assert_eq!(cache.source.calls.load(Ordering::SeqCst), 2);
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn concurrent_reads_of_a_cold_vault_share_one_assessment() -> TestResult {
    let source = ScriptedSource::new(vec![Ok(report("pass")?)]).slow(Duration::from_secs(5));
    let cache = HealthCache::new(source, TTL, TIMEOUT);

    let (a, b, c) = tokio::join!(cache.current("v"), cache.current("v"), cache.current("v"));

    assert!(a.is_ok() && b.is_ok() && c.is_ok());
    assert_eq!(cache.source.calls.load(Ordering::SeqCst), 1);
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn a_refresh_is_not_satisfied_by_an_assessment_that_began_before_it_was_asked_for() -> TestResult {
    // The first assessment starts at once and takes five seconds. The refresh is asked for one second in, so it must
    // not be answered from a scan that may predate whatever the operator has just changed.
    let source = ScriptedSource::new(vec![Ok(report("pass")?), Ok(report("fail")?)]).slow(Duration::from_secs(5));
    let cache = HealthCache::new(source, TTL, TIMEOUT);

    let (first, refreshed) = tokio::join!(cache.current("v"), async {
        tokio::time::sleep(Duration::from_secs(1)).await;
        cache.refresh("v").await
    });

    assert_eq!(first?.report.summary(), HealthSummary::Healthy);
    assert_ne!(refreshed?.report.summary(), HealthSummary::Healthy);
    assert_eq!(cache.source.calls.load(Ordering::SeqCst), 2);
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn a_refresh_asked_while_another_refresh_runs_assesses_for_itself() -> TestResult {
    let source = ScriptedSource::new(vec![Ok(report("pass")?), Ok(report("pass")?)]).slow(Duration::from_secs(5));
    let cache = HealthCache::new(source, TTL, TIMEOUT);

    let (first, second) = tokio::join!(cache.refresh("v"), cache.refresh("v"));

    assert!(first.is_ok() && second.is_ok());
    assert_eq!(cache.source.calls.load(Ordering::SeqCst), 2);
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn readers_waiting_on_a_failing_assessment_share_its_error_instead_of_repeating_it() {
    let source = ScriptedSource::new(vec![Err(HealthError::Unreachable)]).slow(Duration::from_secs(5));
    let cache = HealthCache::new(source, TTL, TIMEOUT);

    let (a, b, c) = tokio::join!(cache.current("v"), cache.current("v"), cache.current("v"));

    assert!(matches!(a, Err(HealthError::Unreachable)));
    assert!(matches!(b, Err(HealthError::Unreachable)));
    assert!(matches!(c, Err(HealthError::Unreachable)));
    assert_eq!(cache.source.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn an_assessment_that_outlives_the_timeout_is_abandoned_and_is_not_remembered() -> TestResult {
    let source = ScriptedSource::new(vec![Ok(report("pass")?)]).slow(Duration::from_secs(60));
    let cache = HealthCache::new(source, TTL, TIMEOUT);

    let first = cache.current("v").await;
    let second = cache.current("v").await;

    assert!(matches!(first, Err(HealthError::TimedOut(_))));
    assert!(matches!(second, Err(HealthError::TimedOut(_))));
    assert_eq!(
        cache.source.calls.load(Ordering::SeqCst),
        2,
        "a timeout is never cached, so the next read tries again"
    );
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn vaults_are_cached_independently() -> TestResult {
    let cache = HealthCache::new(
        ScriptedSource::new(vec![Ok(report("pass")?), Ok(report("fail")?)]),
        TTL,
        TIMEOUT,
    );

    let alpha = cache.current("alpha").await?;
    let beta = cache.current("beta").await?;

    assert_eq!(alpha.report.summary(), HealthSummary::Healthy);
    assert_ne!(beta.report.summary(), HealthSummary::Healthy);
    assert_eq!(cache.source.calls.load(Ordering::SeqCst), 2);
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn a_failed_assessment_is_reported_and_never_cached() -> TestResult {
    let cache = HealthCache::new(
        ScriptedSource::new(vec![Err(HealthError::Unreachable), Ok(report("pass")?)]),
        TTL,
        TIMEOUT,
    );

    let failed = cache.current("v").await;
    let recovered = cache.current("v").await?;

    assert!(matches!(failed, Err(HealthError::Unreachable)));
    assert_eq!(recovered.report.summary(), HealthSummary::Healthy);
    assert_eq!(cache.source.calls.load(Ordering::SeqCst), 2);
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn a_failed_refresh_does_not_hide_behind_the_earlier_good_assessment() -> TestResult {
    let cache = HealthCache::new(
        ScriptedSource::new(vec![Ok(report("pass")?), Err(HealthError::Unreachable)]),
        TTL,
        TIMEOUT,
    );

    cache.current("v").await?;
    let refreshed = cache.refresh("v").await;

    assert!(matches!(refreshed, Err(HealthError::Unreachable)));
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn a_stored_assessment_answers_reads_without_assessing() -> TestResult {
    let cache = HealthCache::new(ScriptedSource::new(vec![Ok(report("fail")?)]), TTL, TIMEOUT);
    cache.current("v").await?;

    cache.store("v", report("pass")?).await;
    let after = cache.current("v").await?;

    assert_eq!(after.report.summary(), HealthSummary::Healthy);
    assert_eq!(after.age, Duration::ZERO);
    assert_eq!(cache.source.calls.load(Ordering::SeqCst), 1);
    Ok(())
}
