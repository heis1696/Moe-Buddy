use moe_agent_runtime::{
    BackgroundCode, CaptureStrategy, Category, CategorySet, ClockReading, DetailLevel,
    DiagnosticKind, DiagnosticsConfig, DiagnosticsError, DiagnosticsService, EmitOutcome,
    ErrorCode, EventCode, EventInput, Gap, HardBudgets, KnownFailure, LogFilter, OperationState,
    PageDirection, RunObservation, Severity, Source, StableDomain, Step, TransportState,
    ViewerGrant, ViewerSubject,
};

fn budgets(events_per_partition: usize, aliases_per_partition: usize) -> HardBudgets {
    HardBudgets {
        max_domains: 2,
        max_grants_per_domain: 2,
        events_per_partition,
        runs_per_partition: 4,
        aliases_per_partition,
        cursors_per_grant: 2,
        max_page_size: 2,
        max_query_work: 20_000,
        max_summaries_per_query: 8,
        max_health_slots: 18,
        max_concurrent_queries: 1,
        max_event_bytes: 2_048,
        max_allocated_bytes: 2_000_000,
    }
}

fn clock(monotonic_ms: u64) -> ClockReading {
    ClockReading {
        monotonic_ms,
        wall_clock_ms: Some(monotonic_ms + 10_000),
    }
}

fn domain(value: &str) -> StableDomain {
    StableDomain::new(value).unwrap()
}
fn subject(value: &str) -> ViewerSubject {
    ViewerSubject::new(value).unwrap()
}

fn grant(
    subject: ViewerSubject,
    version: u64,
    categories: CategorySet,
    maximum_detail: DetailLevel,
) -> ViewerGrant {
    ViewerGrant::new(subject, version, 10_000, categories, maximum_detail)
}

fn configure(
    service: &mut DiagnosticsService,
    budgets: HardBudgets,
    domain_id: StableDomain,
    capture: CaptureStrategy,
    grant: ViewerGrant,
) {
    let mut config = DiagnosticsConfig::new(1, budgets).unwrap();
    config.add_domain(domain_id, capture, &[grant]).unwrap();
    service.enable(config).unwrap();
}

fn event(kind: DiagnosticKind, detail: DetailLevel) -> EventInput {
    EventInput {
        source: Source::Agent,
        detail,
        elapsed_ms: Some(7),
        kind,
    }
}

fn visible_filter() -> LogFilter {
    LogFilter::all().categories(CategorySet::only(Category::Background))
}

#[test]
fn diagnostics_are_closed_by_default_and_denial_is_generic() {
    let mut service = DiagnosticsService::default();
    assert!(matches!(
        service.authorize(domain("pet-private-a"), subject("viewer-a"), 1, clock(0)),
        Err(DiagnosticsError::Unavailable)
    ));
    assert_eq!(
        service
            .create_emitter(domain("pet-private-a"), None)
            .unwrap_err(),
        DiagnosticsError::Unavailable
    );
}

#[test]
fn handles_are_bound_to_service_domain_version_and_expiry() {
    let pet_a = domain("pet-private-a");
    let pet_b = domain("pet-private-b");
    let viewer = subject("viewer-private");
    let mut service = DiagnosticsService::default();
    let domain_grant = ViewerGrant::new(viewer, 1, 100, CategorySet::all(), DetailLevel::Verbose);
    let mut config = DiagnosticsConfig::new(1, budgets(4, 32)).unwrap();
    config
        .add_domain(
            pet_a,
            CaptureStrategy::all_through(DetailLevel::Verbose),
            &[domain_grant],
        )
        .unwrap();
    config
        .add_domain(
            pet_b,
            CaptureStrategy::all_through(DetailLevel::Verbose),
            &[domain_grant],
        )
        .unwrap();
    service.enable(config).unwrap();
    let access = service.authorize(pet_a, viewer, 1, clock(1)).unwrap();
    let run = service.create_run(pet_a).unwrap();
    assert_eq!(
        service.create_emitter(pet_b, Some(&run)).unwrap_err(),
        DiagnosticsError::Unauthorized
    );
    let emitter = service.create_emitter(pet_a, Some(&run)).unwrap();
    let mut other_service = DiagnosticsService::default();
    configure(
        &mut other_service,
        budgets(4, 32),
        pet_a,
        CaptureStrategy::all_through(DetailLevel::Verbose),
        grant(viewer, 1, CategorySet::all(), DetailLevel::Verbose),
    );
    assert_eq!(
        other_service.emit(
            &emitter,
            event(
                DiagnosticKind::Background(BackgroundCode::Waiting),
                DetailLevel::Essential
            ),
            clock(2)
        ),
        Err(DiagnosticsError::StaleHandle)
    );
    assert_eq!(
        service.health_into(&access, clock(100), &mut [None; 9]),
        Err(DiagnosticsError::Unauthorized)
    );
    assert!(matches!(
        service.authorize(pet_a, viewer, 1, clock(100)),
        Err(DiagnosticsError::Unauthorized)
    ));
}

#[test]
fn access_clock_regression_and_expiration_cannot_revive_grants() {
    let pet = domain(stringify!(pet_private_a));
    let viewer = subject(stringify!(viewer_a));
    let categories = CategorySet::only(Category::Background);
    let capture = CaptureStrategy::all_through(DetailLevel::Essential);
    let mut service = DiagnosticsService::default();
    let mut config = DiagnosticsConfig::new(1, budgets(2, 8)).unwrap();
    config
        .add_domain(
            pet,
            capture,
            &[ViewerGrant::new(
                viewer,
                1,
                20,
                categories,
                DetailLevel::Essential,
            )],
        )
        .unwrap();
    service.enable(config).unwrap();
    let access = service.authorize(pet, viewer, 1, clock(1)).unwrap();
    let mut health = [None; 1];
    service
        .health_into(&access, clock(10), &mut health)
        .unwrap();
    assert_eq!(
        service.health_into(&access, clock(9), &mut health),
        Err(DiagnosticsError::ClockWentBackwards)
    );
    assert_eq!(
        service.health_into(&access, clock(11), &mut health),
        Err(DiagnosticsError::ClockWentBackwards)
    );

    let mut expired_service = DiagnosticsService::default();
    let mut expired_config = DiagnosticsConfig::new(1, budgets(2, 8)).unwrap();
    expired_config
        .add_domain(
            pet,
            capture,
            &[ViewerGrant::new(
                viewer,
                1,
                20,
                categories,
                DetailLevel::Essential,
            )],
        )
        .unwrap();
    expired_service.enable(expired_config).unwrap();
    let expired_access = expired_service.authorize(pet, viewer, 1, clock(1)).unwrap();
    assert_eq!(
        expired_service.health_into(&expired_access, clock(20), &mut health),
        Err(DiagnosticsError::Unauthorized)
    );
    assert_eq!(
        expired_service.health_into(&expired_access, clock(5), &mut health),
        Err(DiagnosticsError::Unauthorized)
    );
}

#[test]
fn disabled_capture_and_invisible_partitions_do_not_change_visible_results() {
    let pet = domain("pet-private-a");
    let viewer = subject("viewer-a");
    let mut baseline = DiagnosticsService::default();
    let mut noisy = DiagnosticsService::default();
    let capture = CaptureStrategy::all_through(DetailLevel::Verbose);
    let visible = CategorySet::only(Category::Background);
    configure(
        &mut baseline,
        budgets(2, 32),
        pet,
        capture,
        grant(viewer, 1, visible, DetailLevel::Essential),
    );
    configure(
        &mut noisy,
        budgets(2, 32),
        pet,
        capture,
        grant(viewer, 1, visible, DetailLevel::Essential),
    );
    let access_a = baseline.authorize(pet, viewer, 1, clock(0)).unwrap();
    let access_b = noisy.authorize(pet, viewer, 1, clock(0)).unwrap();
    let emitter_a = baseline.create_emitter(pet, None).unwrap();
    let emitter_b = noisy.create_emitter(pet, None).unwrap();
    for tick in 1..=3 {
        let visible_event = event(
            DiagnosticKind::Background(BackgroundCode::Waiting),
            DetailLevel::Essential,
        );
        assert!(matches!(
            baseline.emit(&emitter_a, visible_event, clock(tick)),
            Ok(EmitOutcome::Accepted | EmitOutcome::AcceptedWithEviction)
        ));
        assert!(matches!(
            noisy.emit(&emitter_b, visible_event, clock(tick)),
            Ok(EmitOutcome::Accepted | EmitOutcome::AcceptedWithEviction)
        ));
    }
    let mut page_b_before = [None; 1];
    let first_b_before = noisy
        .query_into(
            &access_b,
            visible_filter(),
            None,
            clock(20),
            &mut page_b_before,
        )
        .unwrap();
    let visible_alias_before = page_b_before[0].unwrap().event_alias();
    let mut health_b_before = [None; 1];
    noisy
        .health_into(&access_b, clock(20), &mut health_b_before)
        .unwrap();
    if let Some(cursor) = first_b_before.next_cursor {
        noisy.close_cursor(cursor).unwrap();
    }
    for tick in 4..=9 {
        noisy
            .emit(
                &emitter_b,
                event(
                    DiagnosticKind::Error {
                        code: ErrorCode::Unclassified,
                        stage: Step::Infer,
                    },
                    DetailLevel::Verbose,
                ),
                clock(tick),
            )
            .unwrap();
        noisy
            .emit(
                &emitter_b,
                event(
                    DiagnosticKind::Background(BackgroundCode::Initialized),
                    DetailLevel::Verbose,
                ),
                clock(tick),
            )
            .unwrap();
    }
    let mut page_a = [None; 1];
    let mut page_b = [None; 1];
    let first_a = baseline
        .query_into(&access_a, visible_filter(), None, clock(20), &mut page_a)
        .unwrap();
    let first_b = noisy
        .query_into(&access_b, visible_filter(), None, clock(20), &mut page_b)
        .unwrap();
    assert_eq!(first_a.returned, first_b.returned);
    assert_eq!(first_a.gap, first_b.gap);
    assert_eq!(
        page_a[0].unwrap().occurred_at_ms(),
        page_b[0].unwrap().occurred_at_ms()
    );
    assert_eq!(page_b[0].unwrap().event_alias(), visible_alias_before);
    assert_eq!(first_b.gap, first_b_before.gap);
    let mut health_a = [None; 1];
    let mut health_b = [None; 1];
    assert_eq!(
        baseline
            .health_into(&access_a, clock(20), &mut health_a)
            .unwrap(),
        1
    );
    assert_eq!(
        noisy
            .health_into(&access_b, clock(20), &mut health_b)
            .unwrap(),
        1
    );
    assert_eq!(health_b, health_b_before);
    assert_eq!(health_a, health_b);
}

#[test]
fn pagination_uses_snapshot_and_reports_actual_eviction_as_gap() {
    let pet = domain("pet-private-a");
    let viewer = subject("viewer-a");
    let mut service = DiagnosticsService::default();
    configure(
        &mut service,
        budgets(2, 32),
        pet,
        CaptureStrategy::all_through(DetailLevel::Essential),
        grant(
            viewer,
            1,
            CategorySet::only(Category::Background),
            DetailLevel::Essential,
        ),
    );
    let access = service.authorize(pet, viewer, 1, clock(0)).unwrap();
    let emitter = service.create_emitter(pet, None).unwrap();
    for tick in 1..=3 {
        service
            .emit(
                &emitter,
                event(
                    DiagnosticKind::Background(BackgroundCode::Waiting),
                    DetailLevel::Essential,
                ),
                clock(tick),
            )
            .unwrap();
    }
    let mut output = [None; 1];
    let first = service
        .query_into(&access, visible_filter(), None, clock(4), &mut output)
        .unwrap();
    assert_eq!(first.gap, Gap::Possible);
    assert!(first.has_more);
    assert_eq!(output[0].unwrap().occurred_at_ms(), Some(10_002));
    let stale = first.next_cursor.as_ref().unwrap().clone();
    let second = service
        .query_into(
            &access,
            visible_filter(),
            first.next_cursor.as_ref(),
            clock(5),
            &mut output,
        )
        .unwrap();
    assert_eq!(output[0].unwrap().occurred_at_ms(), Some(10_003));
    assert!(!second.has_more);
    assert!(matches!(
        service.query_into(
            &access,
            visible_filter(),
            Some(&stale),
            clock(6),
            &mut output
        ),
        Err(DiagnosticsError::StaleHandle)
    ));
}

#[test]
fn pagination_snapshot_ignores_appends_for_results_and_query_budget() {
    let pet = domain(stringify!(pet - private - a));
    let viewer = subject(stringify!(viewer - a));
    let mut limits = budgets(4, 32);
    limits.max_query_work = 3;
    let mut service = DiagnosticsService::default();
    configure(
        &mut service,
        limits,
        pet,
        CaptureStrategy::all_through(DetailLevel::Essential),
        grant(
            viewer,
            1,
            CategorySet::only(Category::Background),
            DetailLevel::Essential,
        ),
    );
    let access = service.authorize(pet, viewer, 1, clock(0)).unwrap();
    let emitter = service.create_emitter(pet, None).unwrap();
    for tick in 1..=2 {
        service
            .emit(
                &emitter,
                event(
                    DiagnosticKind::Background(BackgroundCode::Waiting),
                    DetailLevel::Essential,
                ),
                clock(tick),
            )
            .unwrap();
    }

    let mut output = [None; 1];
    let first = service
        .query_into(&access, visible_filter(), None, clock(3), &mut output)
        .unwrap();
    assert!(first.has_more);
    assert_eq!(output[0].unwrap().occurred_at_ms(), Some(10_001));

    service
        .emit(
            &emitter,
            event(
                DiagnosticKind::Background(BackgroundCode::Initialized),
                DetailLevel::Essential,
            ),
            clock(4),
        )
        .unwrap();
    let second = service
        .query_into(
            &access,
            visible_filter(),
            first.next_cursor.as_ref(),
            clock(5),
            &mut output,
        )
        .unwrap();
    assert!(!second.has_more);
    assert_eq!(output[0].unwrap().occurred_at_ms(), Some(10_002));
}

#[test]
fn policy_replacement_revokes_access_emitters_and_cursors() {
    let pet = domain("pet-private-a");
    let viewer = subject("viewer-a");
    let mut service = DiagnosticsService::default();
    let capture = CaptureStrategy::all_through(DetailLevel::Essential);
    let categories = CategorySet::only(Category::Background);
    configure(
        &mut service,
        budgets(4, 32),
        pet,
        capture,
        grant(viewer, 1, categories, DetailLevel::Essential),
    );
    let old_access = service.authorize(pet, viewer, 1, clock(0)).unwrap();
    let old_emitter = service.create_emitter(pet, None).unwrap();
    for tick in 1..=3 {
        service
            .emit(
                &old_emitter,
                event(
                    DiagnosticKind::Background(BackgroundCode::Waiting),
                    DetailLevel::Essential,
                ),
                clock(tick),
            )
            .unwrap();
    }
    let mut output = [None; 1];
    let first = service
        .query_into(&old_access, visible_filter(), None, clock(4), &mut output)
        .unwrap();
    let old_cursor = first.next_cursor.unwrap();
    configure(
        &mut service,
        budgets(4, 32),
        pet,
        capture,
        grant(viewer, 2, categories, DetailLevel::Essential),
    );
    assert!(matches!(
        service.emit(
            &old_emitter,
            event(
                DiagnosticKind::Background(BackgroundCode::Waiting),
                DetailLevel::Essential
            ),
            clock(5)
        ),
        Err(DiagnosticsError::StaleHandle)
    ));
    assert!(matches!(
        service.query_into(
            &old_access,
            visible_filter(),
            Some(&old_cursor),
            clock(5),
            &mut output
        ),
        Err(DiagnosticsError::StaleHandle)
    ));
    assert!(matches!(
        service.authorize(pet, viewer, 1, clock(5)),
        Err(DiagnosticsError::Unauthorized)
    ));
    assert!(service.authorize(pet, viewer, 2, clock(5)).is_ok());
}

#[test]
fn operation_and_transport_states_need_independent_evidence() {
    let pet = domain("pet-private-a");
    let viewer = subject("viewer-a");
    let mut service = DiagnosticsService::default();
    let capture = CaptureStrategy::all_through(DetailLevel::Essential);
    configure(
        &mut service,
        budgets(8, 32),
        pet,
        capture,
        grant(
            viewer,
            1,
            CategorySet::only(Category::Run),
            DetailLevel::Essential,
        ),
    );
    let access = service.authorize(pet, viewer, 1, clock(0)).unwrap();
    let completed_run = service.create_run(pet).unwrap();
    let incomplete_run = service.create_run(pet).unwrap();
    let completed_emitter = service.create_emitter(pet, Some(&completed_run)).unwrap();
    let incomplete_emitter = service.create_emitter(pet, Some(&incomplete_run)).unwrap();
    service
        .emit(
            &completed_emitter,
            event(
                DiagnosticKind::Run(RunObservation::StageReturned(Step::Prepare)),
                DetailLevel::Essential,
            ),
            clock(1),
        )
        .unwrap();
    service
        .emit(
            &completed_emitter,
            event(
                DiagnosticKind::Run(RunObservation::Operation(OperationState::Completed)),
                DetailLevel::Essential,
            ),
            clock(2),
        )
        .unwrap();
    service
        .emit(
            &incomplete_emitter,
            event(
                DiagnosticKind::Run(RunObservation::StageReturned(Step::Prepare)),
                DetailLevel::Essential,
            ),
            clock(3),
        )
        .unwrap();
    let mut summaries = [const { None }; 8];
    let listing = service
        .list_runs_into(&access, clock(4), &mut summaries)
        .unwrap();
    assert_eq!(listing.count, 2);
    let completed = summaries
        .iter()
        .flatten()
        .find(|summary| summary.operation_state() == OperationState::Completed)
        .unwrap();
    let completed_alias = completed.alias().unwrap();
    let unknown = summaries
        .iter()
        .flatten()
        .find(|summary| summary.operation_state() == OperationState::Unknown)
        .unwrap();
    assert_eq!(completed.transport_state(), TransportState::Unknown);
    assert_eq!(unknown.transport_state(), TransportState::Unknown);
    assert!(completed.evidence_complete());
    service
        .emit(
            &completed_emitter,
            event(
                DiagnosticKind::Run(RunObservation::Transport(TransportState::Written)),
                DetailLevel::Essential,
            ),
            clock(5),
        )
        .unwrap();
    let updated = service
        .list_runs_into(&access, clock(6), &mut summaries)
        .unwrap();
    assert_eq!(updated.count, 2);
    assert!(
        summaries
            .iter()
            .flatten()
            .any(|summary| summary.alias() == Some(completed_alias)
                && summary.operation_state() == OperationState::Completed
                && summary.transport_state() == TransportState::Written)
    );
}

#[test]
fn disabled_capture_and_allocation_budgets_are_explicit() {
    let pet = domain("pet-private-a");
    let viewer = subject("viewer-a");
    let mut service = DiagnosticsService::default();
    configure(
        &mut service,
        budgets(2, 8),
        pet,
        CaptureStrategy::disabled(),
        grant(
            viewer,
            1,
            CategorySet::only(Category::Background),
            DetailLevel::Essential,
        ),
    );
    let access = service.authorize(pet, viewer, 1, clock(0)).unwrap();
    let emitter = service.create_emitter(pet, None).unwrap();
    assert_eq!(
        service
            .emit(
                &emitter,
                event(
                    DiagnosticKind::Background(BackgroundCode::Waiting),
                    DetailLevel::Essential
                ),
                clock(1)
            )
            .unwrap(),
        EmitOutcome::Omitted
    );
    let mut health = [None; 1];
    assert_eq!(
        service.health_into(&access, clock(2), &mut health).unwrap(),
        1
    );
    assert!(!health[0].unwrap().capture_enabled);
    let mut tiny = budgets(2, 8);
    tiny.max_allocated_bytes = 1;
    assert!(matches!(
        DiagnosticsConfig::new(1, tiny),
        Err(DiagnosticsError::CapacityExceeded)
    ));
}

#[test]
fn safe_debug_and_error_mapping_do_not_accept_untrusted_text() {
    let private_id = "private-pet-id-chat-secret";
    let private_subject = "viewer-secret-token";
    let pet = domain(private_id);
    let viewer = subject(private_subject);
    let mut service = DiagnosticsService::default();
    configure(
        &mut service,
        budgets(2, 8),
        pet,
        CaptureStrategy::all_through(DetailLevel::Essential),
        grant(
            viewer,
            1,
            CategorySet::only(Category::Error),
            DetailLevel::Essential,
        ),
    );
    let access = service.authorize(pet, viewer, 1, clock(0)).unwrap();
    let emitter = service.create_emitter(pet, None).unwrap();
    let unclassified = ErrorCode::from(KnownFailure::Other);
    assert_eq!(unclassified, ErrorCode::Unclassified);
    service
        .emit(
            &emitter,
            event(
                DiagnosticKind::Error {
                    code: unclassified,
                    stage: Step::Infer,
                },
                DetailLevel::Essential,
            ),
            clock(1),
        )
        .unwrap();
    let mut entries = [None; 1];
    service
        .query_into(
            &access,
            LogFilter::all().categories(CategorySet::only(Category::Error)),
            None,
            clock(2),
            &mut entries,
        )
        .unwrap();
    let debug = format!(
        "{:?} {:?} {:?} {:?}",
        pet,
        viewer,
        access,
        entries[0].unwrap()
    );
    assert!(!debug.contains(private_id));
    assert!(!debug.contains(private_subject));
    assert!(!debug.contains("chat-secret"));
    assert!(!debug.contains("token"));
    assert_eq!(
        entries[0].unwrap().retry_hint(),
        Some(moe_agent_runtime::RetryHint::ManualConfirmation)
    );
    assert_eq!(entries[0].unwrap().severity(), Severity::Warn);
}

#[test]
fn clock_regression_and_run_budget_exhaustion_are_partition_local() {
    let pet = domain("pet-private-a");
    let viewer = subject("viewer-a");
    let mut service = DiagnosticsService::default();
    configure(
        &mut service,
        budgets(8, 32),
        pet,
        CaptureStrategy::all_through(DetailLevel::Verbose),
        grant(viewer, 1, CategorySet::all(), DetailLevel::Verbose),
    );
    let access = service.authorize(pet, viewer, 1, clock(0)).unwrap();
    let background = service.create_emitter(pet, None).unwrap();
    let regression = EventInput {
        source: Source::Agent,
        detail: DetailLevel::Verbose,
        elapsed_ms: None,
        kind: DiagnosticKind::Background(BackgroundCode::Waiting),
    };
    service.emit(&background, regression, clock(20)).unwrap();
    assert_eq!(
        service.emit(&background, regression, clock(19)),
        Err(DiagnosticsError::ClockWentBackwards)
    );
    let error = EventInput {
        source: Source::Agent,
        detail: DetailLevel::Essential,
        elapsed_ms: None,
        kind: DiagnosticKind::Background(BackgroundCode::Initialized),
    };
    assert_eq!(
        service.emit(&background, error, clock(5)).unwrap(),
        EmitOutcome::Accepted
    );
    let mut health = [None; 9];
    let count = service
        .health_into(&access, clock(21), &mut health)
        .unwrap();
    let essential = health[..count]
        .iter()
        .flatten()
        .find(|entry| {
            entry.category == Category::Background && entry.detail == DetailLevel::Essential
        })
        .unwrap();
    let verbose = health[..count]
        .iter()
        .flatten()
        .find(|entry| {
            entry.category == Category::Background && entry.detail == DetailLevel::Verbose
        })
        .unwrap();
    assert_eq!(essential.accepted, 1);
    assert!(!essential.clock_regressed);
    assert_eq!(verbose.rejected, 1);
    assert!(verbose.clock_regressed);

    let mut run_service = DiagnosticsService::default();
    configure(
        &mut run_service,
        budgets(8, 32),
        pet,
        CaptureStrategy::all_through(DetailLevel::Essential),
        grant(
            viewer,
            1,
            CategorySet::only(Category::Run),
            DetailLevel::Essential,
        ),
    );
    let run_emitter = (0..5)
        .map(|_| {
            let run = run_service.create_run(pet).unwrap();
            run_service.create_emitter(pet, Some(&run)).unwrap()
        })
        .collect::<Vec<_>>();
    for emitter in run_emitter.iter().take(4) {
        run_service
            .emit(
                emitter,
                event(
                    DiagnosticKind::Run(RunObservation::StageStarted(Step::Infer)),
                    DetailLevel::Essential,
                ),
                clock(1),
            )
            .unwrap();
    }
    assert_eq!(
        run_service.emit(
            &run_emitter[4],
            event(
                DiagnosticKind::Run(RunObservation::StageStarted(Step::Infer)),
                DetailLevel::Essential,
            ),
            clock(1),
        ),
        Err(DiagnosticsError::RunBudgetExceeded)
    );
}

#[test]
fn expired_access_clears_caller_buffers_before_denial() {
    let pet = domain("pet-private-a");
    let viewer = subject("viewer-a");
    let mut service = DiagnosticsService::default();
    configure(
        &mut service,
        budgets(2, 8),
        pet,
        CaptureStrategy::all_through(DetailLevel::Essential),
        grant(
            viewer,
            1,
            CategorySet::only(Category::Background),
            DetailLevel::Essential,
        ),
    );
    let access = service.authorize(pet, viewer, 1, clock(0)).unwrap();
    let emitter = service.create_emitter(pet, None).unwrap();
    service
        .emit(
            &emitter,
            event(
                DiagnosticKind::Background(BackgroundCode::Waiting),
                DetailLevel::Essential,
            ),
            clock(1),
        )
        .unwrap();
    let mut output = [None; 1];
    service
        .query_into(&access, visible_filter(), None, clock(2), &mut output)
        .unwrap();
    assert!(output[0].is_some());
    assert!(matches!(
        service.query_into(&access, visible_filter(), None, clock(10_000), &mut output),
        Err(DiagnosticsError::Unauthorized)
    ));
    assert!(output[0].is_none());
}

#[test]
fn event_aliases_are_partition_local_and_cursor_capacity_is_hard_bounded() {
    let pet = domain("pet-private-a");
    let viewer = subject("viewer-a");
    let mut service = DiagnosticsService::default();
    let mut limits = budgets(4, 1);
    limits.cursors_per_grant = 1;
    configure(
        &mut service,
        limits,
        pet,
        CaptureStrategy::all_through(DetailLevel::Essential),
        grant(viewer, 1, CategorySet::all(), DetailLevel::Essential),
    );
    let access = service.authorize(pet, viewer, 1, clock(0)).unwrap();
    let emitter = service.create_emitter(pet, None).unwrap();
    for tick in 1..=2 {
        service
            .emit(
                &emitter,
                event(
                    DiagnosticKind::Background(BackgroundCode::Waiting),
                    DetailLevel::Essential,
                ),
                clock(tick),
            )
            .unwrap();
    }
    service
        .emit(
            &emitter,
            event(
                DiagnosticKind::Error {
                    code: ErrorCode::Unclassified,
                    stage: Step::Infer,
                },
                DetailLevel::Essential,
            ),
            clock(3),
        )
        .unwrap();
    let mut page = [None; 1];
    let first = service
        .query_into(&access, visible_filter(), None, clock(4), &mut page)
        .unwrap();
    assert!(first.has_more);
    assert!(matches!(
        service.query_into(&access, visible_filter(), None, clock(4), &mut page),
        Err(DiagnosticsError::CursorBudgetExceeded)
    ));
    assert!(page[0].is_none());
    assert!(matches!(
        service.query_into(
            &access,
            visible_filter(),
            first.next_cursor.as_ref(),
            clock(5),
            &mut page
        ),
        Err(DiagnosticsError::AliasBudgetExceeded)
    ));
    assert!(page[0].is_none());
    let error_filter = LogFilter::all().categories(CategorySet::only(Category::Error));
    let error_page = service
        .query_into(&access, error_filter, None, clock(6), &mut page)
        .unwrap();
    assert_eq!(error_page.returned, 1);
    assert_eq!(page[0].unwrap().category(), Category::Error);
}

#[test]
fn run_summary_tracks_business_and_transport_evidence_across_detail_partitions() {
    let pet = domain("pet-private-a");
    let viewer = subject("viewer-a");
    let mut service = DiagnosticsService::default();
    configure(
        &mut service,
        budgets(8, 32),
        pet,
        CaptureStrategy::all_through(DetailLevel::Standard),
        grant(
            viewer,
            1,
            CategorySet::only(Category::Run),
            DetailLevel::Standard,
        ),
    );
    let access = service.authorize(pet, viewer, 1, clock(0)).unwrap();
    let completed_run = service.create_run(pet).unwrap();
    let failed_run = service.create_run(pet).unwrap();
    let completed_emitter = service.create_emitter(pet, Some(&completed_run)).unwrap();
    let failed_emitter = service.create_emitter(pet, Some(&failed_run)).unwrap();
    service
        .emit(
            &completed_emitter,
            event(
                DiagnosticKind::Run(RunObservation::Operation(OperationState::Completed)),
                DetailLevel::Standard,
            ),
            clock(1),
        )
        .unwrap();
    service
        .emit(
            &completed_emitter,
            event(
                DiagnosticKind::Run(RunObservation::Transport(TransportState::Written)),
                DetailLevel::Essential,
            ),
            clock(2),
        )
        .unwrap();
    service
        .emit(
            &failed_emitter,
            event(
                DiagnosticKind::Run(RunObservation::Operation(OperationState::Completed)),
                DetailLevel::Standard,
            ),
            clock(3),
        )
        .unwrap();
    service
        .emit(
            &failed_emitter,
            event(
                DiagnosticKind::Run(RunObservation::Transport(TransportState::Failed)),
                DetailLevel::Essential,
            ),
            clock(4),
        )
        .unwrap();
    service
        .emit(
            &failed_emitter,
            event(
                DiagnosticKind::Run(RunObservation::Operation(OperationState::Failed)),
                DetailLevel::Standard,
            ),
            clock(5),
        )
        .unwrap();

    let mut summaries = [None; 8];
    let listing = service
        .list_runs_into(&access, clock(6), &mut summaries)
        .unwrap();
    assert_eq!(listing.count, 2);
    assert!(summaries.iter().flatten().any(|summary| {
        summary.operation_state() == OperationState::Completed
            && summary.transport_state() == TransportState::Written
    }));
    assert!(summaries.iter().flatten().any(|summary| {
        summary.operation_state() == OperationState::Failed
            && summary.transport_state() == TransportState::Failed
    }));
}

#[test]
fn process_crashed_projects_as_error_severity() {
    let pet = domain("pet-private-a");
    let viewer = subject("viewer-a");
    let mut service = DiagnosticsService::default();
    configure(
        &mut service,
        budgets(4, 8),
        pet,
        CaptureStrategy::all_through(DetailLevel::Essential),
        grant(
            viewer,
            1,
            CategorySet::only(Category::Background),
            DetailLevel::Essential,
        ),
    );
    let access = service.authorize(pet, viewer, 1, clock(0)).unwrap();
    let emitter = service.create_emitter(pet, None).unwrap();
    service
        .emit(
            &emitter,
            event(
                DiagnosticKind::Background(BackgroundCode::ProcessCrashed),
                DetailLevel::Essential,
            ),
            clock(1),
        )
        .unwrap();
    let filter = LogFilter::all()
        .categories(CategorySet::only(Category::Background))
        .severity(Severity::Error);
    let mut output = [None; 1];
    let page = service
        .query_into(&access, filter, None, clock(2), &mut output)
        .unwrap();
    assert_eq!(page.returned, 1);
    assert_eq!(output[0].unwrap().severity(), Severity::Error);
}

#[test]
fn bounded_safe_search_direction_and_cursor_binding_work() {
    let pet = domain("pet-private-a");
    let viewer = subject("viewer-a");
    let mut service = DiagnosticsService::default();
    configure(
        &mut service,
        budgets(8, 16),
        pet,
        CaptureStrategy::all_through(DetailLevel::Essential),
        grant(
            viewer,
            1,
            CategorySet::only(Category::Background),
            DetailLevel::Essential,
        ),
    );
    let access = service.authorize(pet, viewer, 1, clock(0)).unwrap();
    let emitter = service.create_emitter(pet, None).unwrap();
    for (tick, code) in [
        (1, BackgroundCode::Waiting),
        (2, BackgroundCode::ProcessCrashed),
        (3, BackgroundCode::Waiting),
        (4, BackgroundCode::Initialized),
    ] {
        service
            .emit(
                &emitter,
                event(DiagnosticKind::Background(code), DetailLevel::Essential),
                clock(tick),
            )
            .unwrap();
    }
    let filter = LogFilter::all()
        .categories(CategorySet::only(Category::Background))
        .event_code(EventCode::Background(BackgroundCode::Waiting))
        .time_range(1, 4)
        .unwrap()
        .search("background.waiting")
        .unwrap()
        .direction(PageDirection::NewestFirst);
    assert!(!format!("{filter:?}").contains("background.waiting"));
    let mut output = [None; 1];
    let first = service
        .query_into(&access, filter, None, clock(5), &mut output)
        .unwrap();
    assert!(first.has_more);
    assert_eq!(output[0].unwrap().occurred_at_ms(), Some(10_003));
    let cursor = first.next_cursor.unwrap();
    assert!(matches!(
        service.query_into(
            &access,
            filter.direction(PageDirection::OldestFirst),
            Some(&cursor),
            clock(6),
            &mut output,
        ),
        Err(DiagnosticsError::InvalidCursor)
    ));
    assert!(output[0].is_none());
    let changed = filter.search("process.crashed").unwrap();
    assert!(matches!(
        service.query_into(&access, changed, Some(&cursor), clock(7), &mut output),
        Err(DiagnosticsError::InvalidCursor)
    ));
    assert!(output[0].is_none());
    let changed_range = LogFilter::all()
        .categories(CategorySet::only(Category::Background))
        .event_code(EventCode::Background(BackgroundCode::Waiting))
        .time_range(0, 4)
        .unwrap()
        .search("background.waiting")
        .unwrap()
        .direction(PageDirection::NewestFirst);
    assert!(matches!(
        service.query_into(&access, changed_range, Some(&cursor), clock(8), &mut output),
        Err(DiagnosticsError::InvalidCursor)
    ));
    assert!(output[0].is_none());
    let second = service
        .query_into(&access, filter, Some(&cursor), clock(9), &mut output)
        .unwrap();
    assert!(!second.has_more);
    assert_eq!(output[0].unwrap().occurred_at_ms(), Some(10_001));
    assert!(LogFilter::all().search("sensitive\nterm").is_err());
    assert!(LogFilter::all().search(&"x".repeat(97)).is_err());
    assert!(LogFilter::all().time_range(4, 4).is_err());
    let bounded = LogFilter::all()
        .categories(CategorySet::only(Category::Background))
        .time_range(2, 3)
        .unwrap()
        .search("process.crashed")
        .unwrap();
    let bounded_page = service
        .query_into(&access, bounded, None, clock(10), &mut output)
        .unwrap();
    assert_eq!(bounded_page.returned, 1);
    assert_eq!(output[0].unwrap().occurred_at_ms(), Some(10_002));
}

#[test]
fn run_alias_is_stable_across_partitions_and_can_be_paged_safely() {
    let pet = domain("pet-private-a");
    let viewer = subject("viewer-a");
    let mut service = DiagnosticsService::default();
    configure(
        &mut service,
        budgets(8, 32),
        pet,
        CaptureStrategy::all_through(DetailLevel::Essential),
        grant(viewer, 1, CategorySet::all(), DetailLevel::Essential),
    );
    let access = service.authorize(pet, viewer, 1, clock(0)).unwrap();
    let run = service.create_run(pet).unwrap();
    let emitter = service.create_emitter(pet, Some(&run)).unwrap();
    service
        .emit(
            &emitter,
            event(
                DiagnosticKind::Run(RunObservation::Operation(OperationState::Completed)),
                DetailLevel::Essential,
            ),
            clock(1),
        )
        .unwrap();
    service
        .emit(
            &emitter,
            event(
                DiagnosticKind::Error {
                    code: ErrorCode::Unclassified,
                    stage: Step::Infer,
                },
                DetailLevel::Essential,
            ),
            clock(2),
        )
        .unwrap();
    let mut summaries = [None; 8];
    service
        .list_runs_into(&access, clock(3), &mut summaries)
        .unwrap();
    let alias = summaries[0].unwrap().alias().unwrap();
    let mut output = [None; 1];
    service
        .query_into(
            &access,
            LogFilter::all().categories(CategorySet::only(Category::Run)),
            None,
            clock(4),
            &mut output,
        )
        .unwrap();
    assert_eq!(output[0].unwrap().run_alias(), Some(alias));
    service
        .query_into(
            &access,
            LogFilter::all().categories(CategorySet::only(Category::Error)),
            None,
            clock(5),
            &mut output,
        )
        .unwrap();
    assert_eq!(output[0].unwrap().run_alias(), Some(alias));

    let filter = LogFilter::all().direction(PageDirection::NewestFirst);
    let first = service
        .inspect_run_alias_into(&access, alias, filter, None, clock(6), &mut output)
        .unwrap();
    assert!(first.has_more);
    assert_eq!(output[0].unwrap().category(), Category::Error);
    let second = service
        .inspect_run_alias_into(
            &access,
            alias,
            filter,
            first.next_cursor.as_ref(),
            clock(7),
            &mut output,
        )
        .unwrap();
    assert!(!second.has_more);
    assert_eq!(output[0].unwrap().category(), Category::Run);
    assert_eq!(output[0].unwrap().run_alias(), Some(alias));
}

#[test]
fn hidden_detail_partition_does_not_consume_visible_alias_capacity() {
    let pet = domain("pet-private-a");
    let viewer = subject("viewer-a");
    let mut service = DiagnosticsService::default();
    configure(
        &mut service,
        budgets(4, 1),
        pet,
        CaptureStrategy::all_through(DetailLevel::Verbose),
        grant(
            viewer,
            1,
            CategorySet::only(Category::Background),
            DetailLevel::Essential,
        ),
    );
    let access = service.authorize(pet, viewer, 1, clock(0)).unwrap();
    let emitter = service.create_emitter(pet, None).unwrap();
    service
        .emit(
            &emitter,
            event(
                DiagnosticKind::Background(BackgroundCode::Waiting),
                DetailLevel::Verbose,
            ),
            clock(1),
        )
        .unwrap();
    service
        .emit(
            &emitter,
            event(
                DiagnosticKind::Background(BackgroundCode::Waiting),
                DetailLevel::Essential,
            ),
            clock(2),
        )
        .unwrap();
    let mut output = [None; 1];
    let page = service
        .query_into(&access, visible_filter(), None, clock(3), &mut output)
        .unwrap();
    assert_eq!(page.returned, 1);
    assert_eq!(output[0].unwrap().detail(), DetailLevel::Essential);
}

#[test]
fn run_listing_clears_partial_summaries_on_alias_budget_error() {
    let pet = domain("pet-private-a");
    let viewer = subject("viewer-a");
    let mut service = DiagnosticsService::default();
    configure(
        &mut service,
        budgets(8, 1),
        pet,
        CaptureStrategy::all_through(DetailLevel::Essential),
        grant(
            viewer,
            1,
            CategorySet::only(Category::Run),
            DetailLevel::Essential,
        ),
    );
    let access = service.authorize(pet, viewer, 1, clock(0)).unwrap();
    for tick in 1..=2 {
        let run = service.create_run(pet).unwrap();
        let emitter = service.create_emitter(pet, Some(&run)).unwrap();
        service
            .emit(
                &emitter,
                event(
                    DiagnosticKind::Run(RunObservation::Operation(OperationState::Completed)),
                    DetailLevel::Essential,
                ),
                clock(tick),
            )
            .unwrap();
    }
    let mut output = [None; 2];
    assert!(matches!(
        service.list_runs_into(&access, clock(3), &mut output),
        Err(DiagnosticsError::AliasBudgetExceeded)
    ));
    assert!(output.iter().all(Option::is_none));
}

#[test]
fn equal_clock_cross_partition_events_keep_acceptance_order() {
    let pet = domain("ordering-pet");
    let viewer = subject("ordering-viewer");
    let mut service = DiagnosticsService::default();
    configure(
        &mut service,
        budgets(8, 32),
        pet,
        CaptureStrategy::all_through(DetailLevel::Verbose),
        grant(viewer, 1, CategorySet::all(), DetailLevel::Verbose),
    );
    let access = service.authorize(pet, viewer, 1, clock(0)).unwrap();
    let emitter = service.create_emitter(pet, None).unwrap();
    for (code, detail) in [
        (BackgroundCode::ServiceStarted, DetailLevel::Standard),
        (BackgroundCode::Initialized, DetailLevel::Essential),
    ] {
        service
            .emit(
                &emitter,
                event(DiagnosticKind::Background(code), detail),
                clock(1),
            )
            .unwrap();
    }
    let mut entries = [None; 2];
    service
        .query_into(&access, visible_filter(), None, clock(2), &mut entries)
        .unwrap();
    assert_eq!(
        entries[0].unwrap().event_code(),
        moe_agent_runtime::EventCode::Background(BackgroundCode::ServiceStarted)
    );
    assert_eq!(
        entries[1].unwrap().event_code(),
        moe_agent_runtime::EventCode::Background(BackgroundCode::Initialized)
    );
    let run = service.create_run(pet).unwrap();
    let run_emitter = service.create_emitter(pet, Some(&run)).unwrap();
    for (state, detail) in [
        (OperationState::Completed, DetailLevel::Standard),
        (OperationState::Cancelled, DetailLevel::Essential),
    ] {
        service
            .emit(
                &run_emitter,
                event(
                    DiagnosticKind::Run(RunObservation::Operation(state)),
                    detail,
                ),
                clock(3),
            )
            .unwrap();
    }
    let mut summaries = [None];
    service
        .list_runs_into(&access, clock(4), &mut summaries)
        .unwrap();
    assert_eq!(
        summaries[0].as_ref().unwrap().operation_state(),
        OperationState::Cancelled
    );
}

#[test]
fn rejected_visible_evidence_cannot_claim_complete_run_history() {
    let pet = domain("rejection-pet");
    let viewer = subject("rejection-viewer");
    let mut service = DiagnosticsService::default();
    configure(
        &mut service,
        budgets(4, 16),
        pet,
        CaptureStrategy::all_through(DetailLevel::Essential),
        grant(
            viewer,
            1,
            CategorySet::only(Category::Run),
            DetailLevel::Essential,
        ),
    );
    let access = service.authorize(pet, viewer, 1, clock(0)).unwrap();
    let run = service.create_run(pet).unwrap();
    let emitter = service.create_emitter(pet, Some(&run)).unwrap();
    service
        .emit(
            &emitter,
            event(
                DiagnosticKind::Run(RunObservation::Operation(OperationState::InProgress)),
                DetailLevel::Essential,
            ),
            clock(2),
        )
        .unwrap();
    assert_eq!(
        service.emit(
            &emitter,
            event(
                DiagnosticKind::Run(RunObservation::Operation(OperationState::Completed)),
                DetailLevel::Essential
            ),
            clock(1)
        ),
        Err(DiagnosticsError::ClockWentBackwards)
    );
    let mut summaries = [None];
    service
        .list_runs_into(&access, clock(3), &mut summaries)
        .unwrap();
    let summary = summaries[0].as_ref().unwrap();
    assert!(!summary.evidence_complete());
    assert_eq!(summary.operation_state(), OperationState::Unknown);
    assert_eq!(summary.transport_state(), TransportState::Unknown);
}

#[test]
fn aliases_remain_distinct_after_their_service_and_handles_are_dropped() {
    fn first_alias() -> moe_agent_runtime::OpaqueAlias {
        let pet = domain("alias-pet");
        let viewer = subject("alias-viewer");
        let mut service = DiagnosticsService::default();
        configure(
            &mut service,
            budgets(2, 8),
            pet,
            CaptureStrategy::all_through(DetailLevel::Essential),
            grant(
                viewer,
                1,
                CategorySet::only(Category::Background),
                DetailLevel::Essential,
            ),
        );
        let access = service.authorize(pet, viewer, 1, clock(0)).unwrap();
        let emitter = service.create_emitter(pet, None).unwrap();
        service
            .emit(
                &emitter,
                event(
                    DiagnosticKind::Background(BackgroundCode::Waiting),
                    DetailLevel::Essential,
                ),
                clock(1),
            )
            .unwrap();
        let mut entries = [None];
        service
            .query_into(&access, visible_filter(), None, clock(2), &mut entries)
            .unwrap();
        entries[0].unwrap().event_alias()
    }
    let original = first_alias();
    for _ in 0..100 {
        assert_ne!(first_alias(), original);
    }
}

#[test]
fn host_process_generation_must_be_explicit_and_nonzero() {
    assert!(matches!(
        DiagnosticsConfig::new(0, budgets(2, 8)),
        Err(DiagnosticsError::InvalidConfiguration)
    ));
}

#[test]
fn newest_first_reports_an_evicted_prefix_even_when_the_snapshot_fits_one_page() {
    let pet = domain("reverse-gap-pet");
    let viewer = subject("reverse-gap-viewer");
    let mut service = DiagnosticsService::default();
    configure(
        &mut service,
        budgets(2, 8),
        pet,
        CaptureStrategy::all_through(DetailLevel::Essential),
        grant(
            viewer,
            1,
            CategorySet::only(Category::Background),
            DetailLevel::Essential,
        ),
    );
    let access = service.authorize(pet, viewer, 1, clock(0)).unwrap();
    let emitter = service.create_emitter(pet, None).unwrap();
    for timestamp in 1..=3 {
        service
            .emit(
                &emitter,
                event(
                    DiagnosticKind::Background(BackgroundCode::Waiting),
                    DetailLevel::Essential,
                ),
                clock(timestamp),
            )
            .unwrap();
    }
    let filter = visible_filter().direction(moe_agent_runtime::PageDirection::NewestFirst);
    let mut entries = [None; 2];
    let page = service
        .query_into(&access, filter, None, clock(4), &mut entries)
        .unwrap();
    assert_eq!(page.gap, Gap::Possible);
    assert!(!page.has_more);
    assert_eq!(entries[0].unwrap().occurred_at_ms(), Some(10_003));
    assert_eq!(entries[1].unwrap().occurred_at_ms(), Some(10_002));
}
