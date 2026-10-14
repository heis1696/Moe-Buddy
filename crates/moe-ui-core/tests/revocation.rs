use std::cell::Cell;
use std::convert::Infallible;
use std::error::Error;
use std::fmt;
use std::rc::Rc;

use moe_ui_core::{
    CloseDecision, EventSink, NavSink, Navigate, PanelEntry, PanelHandle, PanelKind, PanelScope,
    ReleaseCause, ScopeIdentity, ScopeLevel, UiConfig, UiController, UiError, UiManager, UiPanel,
    UiView,
};

#[derive(Default)]
struct ReleaseState {
    fail_unmount: Cell<bool>,
    revocations: Cell<usize>,
    releases: Cell<usize>,
}

struct SensitivePayload {
    body: String,
    drops: Rc<Cell<usize>>,
}

impl Drop for SensitivePayload {
    fn drop(&mut self) {
        self.drops.set(self.drops.get() + 1);
    }
}

struct Controller {
    model: String,
    state: Rc<ReleaseState>,
}

impl UiController for Controller {
    type OpenArgs = Rc<ReleaseState>;
    type Model = String;
    type Event = SensitivePayload;
    type Error = Infallible;

    fn model(&self) -> &Self::Model {
        &self.model
    }

    fn apply(
        &mut self,
        event: Self::Event,
        _navigation: &mut NavSink<'_>,
    ) -> Result<(), Infallible> {
        self.model.clone_from(&event.body);
        Ok(())
    }

    fn revise(
        &mut self,
        _args: &Self::OpenArgs,
        _navigation: &mut NavSink<'_>,
    ) -> Result<(), Infallible> {
        Ok(())
    }

    fn refresh(&mut self) -> Result<(), Infallible> {
        Ok(())
    }

    fn suspend(&mut self) -> Result<(), Infallible> {
        Ok(())
    }

    fn close_request(&mut self) -> CloseDecision {
        CloseDecision::Allow
    }

    fn revoke(&mut self) {
        self.model.clear();
        self.state.revocations.set(self.state.revocations.get() + 1);
    }

    fn release(&mut self) -> Result<(), Infallible> {
        self.state.releases.set(self.state.releases.get() + 1);
        Ok(())
    }
}

#[derive(Debug)]
struct UnmountError;

impl fmt::Display for UnmountError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("synthetic unmount failure")
    }
}

impl Error for UnmountError {}

struct View(Rc<ReleaseState>);

impl UiView for View {
    type Model = String;
    type Event = SensitivePayload;
    type Context = ();
    type Error = UnmountError;

    fn mount(&mut self, _context: &mut ()) -> Result<(), UnmountError> {
        Ok(())
    }

    fn render(
        &mut self,
        _model: &String,
        _context: &mut (),
        _events: &mut EventSink<'_, SensitivePayload>,
    ) -> Result<(), UnmountError> {
        Ok(())
    }

    fn unmount(&mut self, _context: &mut ()) -> Result<(), UnmountError> {
        if self.0.fail_unmount.get() {
            Err(UnmountError)
        } else {
            Ok(())
        }
    }
}

struct Fixture {
    manager: UiManager<()>,
    entry: PanelEntry<Controller, View>,
    handle: PanelHandle,
    scope: PanelScope,
    state: Rc<ReleaseState>,
}

fn fixture() -> Fixture {
    let state = Rc::new(ReleaseState::default());
    let mut manager = UiManager::new(UiConfig::new(1, 1, 4, 2, 4, 2).unwrap()).unwrap();
    let entry = manager
        .register(
            PanelKind::new("sensitive").unwrap(),
            ScopeLevel::Pet,
            |_key, state: &Rc<ReleaseState>| {
                Ok(UiPanel::new(
                    Controller {
                        model: String::new(),
                        state: state.clone(),
                    },
                    View(state.clone()),
                ))
            },
        )
        .unwrap();
    let scope = PanelScope::Pet(ScopeIdentity::new("pet-a").unwrap());
    let handle = manager
        .open(&mut (), &entry, scope.clone(), Some(&state))
        .unwrap()
        .handle();
    Fixture {
        manager,
        entry,
        handle,
        scope,
        state,
    }
}

fn sensitive_payload(drops: &Rc<Cell<usize>>) -> SensitivePayload {
    SensitivePayload {
        body: "synthetic private result".to_owned(),
        drops: drops.clone(),
    }
}

#[test]
fn scope_revocation_drops_queued_payloads_in_existing_failed_release() {
    let mut setup = fixture();
    let drops = Rc::new(Cell::new(0));
    let ticket = setup
        .manager
        .begin_load(&setup.entry, &setup.handle)
        .unwrap();
    assert!(
        setup
            .manager
            .submit(&setup.entry, &ticket, sensitive_payload(&drops))
            .is_ok()
    );
    assert!(
        setup
            .manager
            .send(&setup.entry, &setup.handle, sensitive_payload(&drops))
            .is_ok()
    );
    setup.state.fail_unmount.set(true);
    let record = match setup.manager.hide(&mut (), &setup.handle) {
        Err(UiError::ReleaseIncomplete { record, .. }) => record,
        other => panic!("expected failed release, got {other:?}"),
    };
    assert_eq!(drops.get(), 0);
    assert!(setup.manager.retry_release(&mut (), record).is_err());
    assert_eq!(drops.get(), 0);
    let report =
        setup
            .manager
            .force_release_scope(&mut (), &setup.scope, ReleaseCause::ContextRevoked);
    assert_eq!(report.failed().len(), 1);
    assert_eq!(drops.get(), 2);
    assert_eq!(setup.state.revocations.get(), 1);
    assert_eq!(setup.state.releases.get(), 1);
    assert_eq!(setup.manager.pending_releases().len(), 1);
    assert!(setup.manager.retry_release(&mut (), record).is_err());
    assert_eq!(drops.get(), 2);
    assert_eq!(setup.state.revocations.get(), 1);
    assert_eq!(setup.state.releases.get(), 1);
    setup.state.fail_unmount.set(false);
    setup.manager.retry_release(&mut (), record).unwrap();
    assert!(setup.manager.pending_releases().is_empty());
    assert_eq!(setup.state.releases.get(), 1);
}

#[test]
fn live_force_release_drops_payload_before_failed_unmount() {
    let mut setup = fixture();
    let drops = Rc::new(Cell::new(0));
    let ticket = setup
        .manager
        .begin_load(&setup.entry, &setup.handle)
        .unwrap();
    assert!(
        setup
            .manager
            .submit(&setup.entry, &ticket, sensitive_payload(&drops))
            .is_ok()
    );
    setup.state.fail_unmount.set(true);
    assert!(
        setup
            .manager
            .force_release(&mut (), &setup.handle, ReleaseCause::ContextRevoked)
            .is_err()
    );
    assert_eq!(drops.get(), 1);
    assert_eq!(setup.state.revocations.get(), 1);
    assert!(!setup.manager.contains(&setup.handle));
    assert_eq!(setup.manager.pending_releases().len(), 1);
}

#[test]
fn ordinary_hide_keeps_accepted_plain_payload_until_restored() {
    let mut setup = fixture();
    let drops = Rc::new(Cell::new(0));
    assert!(
        setup
            .manager
            .send(&setup.entry, &setup.handle, sensitive_payload(&drops))
            .is_ok()
    );
    setup.manager.hide(&mut (), &setup.handle).unwrap();
    setup.manager.drive(&mut ());
    assert_eq!(drops.get(), 0);
    setup.manager.show(&mut (), &setup.handle).unwrap();
    assert_eq!(setup.manager.drive(&mut ()).inputs_delivered(), 1);
    assert_eq!(drops.get(), 1);
    assert_eq!(setup.state.revocations.get(), 0);
}

fn assert_failed_navigation_reports_focus_clear(command: fn(PanelHandle) -> Navigate) {
    let mut setup = fixture();
    setup.manager.focus(&setup.handle).unwrap();
    setup.state.fail_unmount.set(true);
    setup
        .manager
        .requeue_navigation(command(setup.handle))
        .unwrap();
    let report = setup.manager.drive(&mut ());
    assert_eq!(report.navigation_faults().len(), 1);
    assert!(report.focus_cleared());
    assert_eq!(setup.manager.focused(), None);
    assert!(!setup.manager.contains(&setup.handle));
}

#[test]
fn failed_close_navigation_reports_focus_clear() {
    assert_failed_navigation_reports_focus_clear(Navigate::Close);
}

#[test]
fn failed_hide_navigation_reports_focus_clear() {
    assert_failed_navigation_reports_focus_clear(Navigate::Hide);
}
