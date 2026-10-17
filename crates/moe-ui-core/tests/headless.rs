//! U1 无头测试:注册、打开/复用/新入参、作用域隔离、隐藏恢复草稿、
//! 关闭确认、强制清理、焦点、旧句柄/迟到加载、容量、每轮预算与背压、
//! 单面板错误链,以及确认失效、隐藏保留输入、敏感缓存清除、逐项
//! 强制释放与渲染先行驱动顺序的针对性回归。测试视图只留在本目录,
//! 不导出为真实呈现。

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::error::Error;
use std::fmt;
use std::rc::Rc;

use moe_ui_core::{
    AggregateError, CloseDecision, CloseRequest, EventSink, NavSink, Navigate, OpenOutcome,
    PanelEntry, PanelHandle, PanelKey, PanelKind, PanelScope, RejectionReason, ReleaseCause,
    ScopeIdentity, ScopeLevel, Stage, UiConfig, UiController, UiError, UiManager, UiPanel, UiView,
    Visibility,
};

// ---- 测试夹具 ----

#[derive(Debug)]
struct TestError {
    stage: String,
    cause: Option<Box<dyn Error>>,
}

impl TestError {
    fn new(stage: &str) -> Self {
        Self {
            stage: stage.to_owned(),
            cause: None,
        }
    }

    fn chained(stage: &str) -> Self {
        Self {
            stage: stage.to_owned(),
            cause: Some(Box::new(RootCause("root-detail"))),
        }
    }
}

#[derive(Debug)]
struct RootCause(&'static str);

impl fmt::Display for RootCause {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "root cause: {}", self.0)
    }
}

impl Error for RootCause {}

impl fmt::Display for TestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "test error at {}", self.stage)
    }
}

impl Error for TestError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.cause.as_deref()
    }
}

#[derive(Default, Debug)]
struct TestModel {
    counter: u32,
    bound_key: Option<PanelKey>,
    close_requests: u32,
    draft: String,
    saved: Vec<String>,
    revisions: Vec<String>,
    refreshes: u32,
    suspends: u32,
    load_results: Vec<u32>,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum TestNav {
    CloseSelf,
    HideSelf,
}

#[derive(Debug, PartialEq, Eq, Clone)]
enum TestEvent {
    Add(u32),
    Draft(&'static str),
    Save,
    LoadResult(u32),
    Fail(&'static str),
    FailChained(&'static str),
    EmitNav(TestNav),
}

struct TestArgs {
    label: &'static str,
    render_failures: u32,
    mount_failures: u32,
    unmount_failures: u32,
    emits: Vec<Vec<TestEvent>>,
    revise_navs: Vec<TestNav>,
    repeat_emit: Option<TestEvent>,
}

impl TestArgs {
    fn plain(label: &'static str) -> Self {
        Self {
            label,
            render_failures: 0,
            mount_failures: 0,
            unmount_failures: 0,
            emits: Vec::new(),
            revise_navs: Vec::new(),
            repeat_emit: None,
        }
    }

    fn with_emits(label: &'static str, emits: Vec<Vec<TestEvent>>) -> Self {
        Self {
            label,
            render_failures: 0,
            mount_failures: 0,
            unmount_failures: 0,
            emits,
            revise_navs: Vec::new(),
            repeat_emit: None,
        }
    }
}

/// 控制器与测试共享的开关;测试运行中可切换。
struct Flags {
    factory_calls: Cell<u32>,
    close_confirm: Cell<bool>,
    revise_fails: Cell<bool>,
    fail_release: Cell<bool>,
    chain_release_errors: Cell<bool>,
    chain_unmount_errors: Cell<bool>,
    refresh_fails: Cell<bool>,
    view_mount_failures: Cell<u32>,
    view_unmount_failures: Cell<u32>,
    unmount_calls: Cell<u32>,
    mount_calls: Cell<u32>,
    release_calls: Cell<u32>,
    revocations: Cell<u32>,
    refused_nav: RefCell<Vec<Navigate>>,
}

impl Flags {
    fn new() -> Rc<Self> {
        Rc::new(Self {
            factory_calls: Cell::new(0),
            close_confirm: Cell::new(false),
            revise_fails: Cell::new(false),
            fail_release: Cell::new(false),
            chain_release_errors: Cell::new(false),
            chain_unmount_errors: Cell::new(false),
            refresh_fails: Cell::new(false),
            view_mount_failures: Cell::new(0),
            view_unmount_failures: Cell::new(0),
            unmount_calls: Cell::new(0),
            mount_calls: Cell::new(0),
            release_calls: Cell::new(0),
            revocations: Cell::new(0),
            refused_nav: RefCell::new(Vec::new()),
        })
    }
}

fn submit_nav(sink: &mut NavSink<'_>, nav: TestNav) -> Result<(), Navigate> {
    match nav {
        TestNav::CloseSelf => sink.close_self(),
        TestNav::HideSelf => sink.hide_self(),
    }
}

struct TestController {
    model: TestModel,
    flags: Rc<Flags>,
    log: Rc<RefCell<Vec<String>>>,
}

impl UiController for TestController {
    type OpenArgs = TestArgs;
    type Model = TestModel;
    type Event = TestEvent;
    type Error = TestError;

    fn model(&self) -> &TestModel {
        &self.model
    }

    fn apply(&mut self, event: TestEvent, sink: &mut NavSink<'_>) -> Result<(), TestError> {
        self.log.borrow_mut().push(format!("apply {event:?}"));
        match event {
            TestEvent::Add(count) => self.model.counter += count,
            TestEvent::Draft(text) => self.model.draft.push_str(text),
            TestEvent::Save => {
                let draft = std::mem::take(&mut self.model.draft);
                self.model.saved.push(draft);
            }
            TestEvent::LoadResult(value) => self.model.load_results.push(value),
            TestEvent::Fail(stage) => return Err(TestError::new(stage)),
            TestEvent::FailChained(stage) => return Err(TestError::chained(stage)),
            TestEvent::EmitNav(nav) => {
                if let Err(command) = submit_nav(sink, nav) {
                    self.flags.refused_nav.borrow_mut().push(command);
                }
            }
        }
        Ok(())
    }

    fn revise(&mut self, args: &TestArgs, sink: &mut NavSink<'_>) -> Result<(), TestError> {
        self.model.revisions.push(args.label.to_owned());
        for nav_request in &args.revise_navs {
            if let Err(command) = submit_nav(sink, *nav_request) {
                self.flags.refused_nav.borrow_mut().push(command);
            }
        }
        if self.flags.revise_fails.get() {
            return Err(TestError::new("revise"));
        }
        Ok(())
    }

    fn refresh(&mut self) -> Result<(), TestError> {
        if self.flags.refresh_fails.get() {
            return Err(TestError::new("refresh"));
        }
        self.model.refreshes += 1;
        Ok(())
    }

    fn suspend(&mut self) -> Result<(), TestError> {
        self.model.suspends += 1;
        Ok(())
    }

    fn close_request(&mut self) -> CloseDecision {
        self.model.close_requests += 1;
        if self.flags.close_confirm.get() && !self.model.draft.is_empty() {
            CloseDecision::RequireConfirmation
        } else {
            CloseDecision::Allow
        }
    }

    fn revoke(&mut self) {
        self.flags.revocations.set(self.flags.revocations.get() + 1);
    }

    fn release(&mut self) -> Result<(), TestError> {
        self.flags
            .release_calls
            .set(self.flags.release_calls.get() + 1);
        if self.flags.fail_release.get() {
            return Err(if self.flags.chain_release_errors.get() {
                TestError::chained("release")
            } else {
                TestError::new("release")
            });
        }
        Ok(())
    }
}

struct ViewScript {
    render_failures: Cell<u32>,
    mount_failures: Cell<u32>,
    unmount_failures: Cell<u32>,
    emits: RefCell<VecDeque<Vec<TestEvent>>>,
    repeat_emit: Option<TestEvent>,
}

impl ViewScript {
    fn from_args(args: &TestArgs) -> Self {
        Self {
            render_failures: Cell::new(args.render_failures),
            mount_failures: Cell::new(args.mount_failures),
            unmount_failures: Cell::new(args.unmount_failures),
            emits: RefCell::new(args.emits.iter().cloned().collect()),
            repeat_emit: args.repeat_emit.clone(),
        }
    }
}

struct TestView {
    script: ViewScript,
    flags: Rc<Flags>,
    log: Rc<RefCell<Vec<String>>>,
}

impl UiView for TestView {
    type Model = TestModel;
    type Event = TestEvent;
    type Context = ();
    type Error = TestError;

    fn mount(&mut self, _context: &mut ()) -> Result<(), TestError> {
        self.flags.mount_calls.set(self.flags.mount_calls.get() + 1);
        if self.flags.view_mount_failures.get() > 0 {
            self.flags
                .view_mount_failures
                .set(self.flags.view_mount_failures.get() - 1);
            return Err(TestError::new("mount"));
        }
        if self.script.mount_failures.get() > 0 {
            self.script
                .mount_failures
                .set(self.script.mount_failures.get() - 1);
            return Err(TestError::new("mount"));
        }
        Ok(())
    }

    fn render(
        &mut self,
        model: &TestModel,
        _context: &mut (),
        events: &mut EventSink<'_, TestEvent>,
    ) -> Result<(), TestError> {
        if self.script.render_failures.get() > 0 {
            self.script
                .render_failures
                .set(self.script.render_failures.get() - 1);
            return Err(TestError::chained("render"));
        }
        for event in self
            .script
            .emits
            .borrow_mut()
            .pop_front()
            .unwrap_or_default()
        {
            let _ = events.submit(event);
        }
        if let Some(event) = self.script.repeat_emit.clone() {
            let _ = events.submit(event);
        }
        self.log.borrow_mut().push(format!(
            "counter={} key={} closes={} draft={:?} saved={:?} revisions={:?} refreshes={} suspends={} load={:?}",
            model.counter,
            model.bound_key.as_ref().map(ToString::to_string).unwrap_or_default(),
            model.close_requests,
            model.draft,
            model.saved,
            model.revisions,
            model.refreshes,
            model.suspends,
            model.load_results
        ));
        Ok(())
    }

    fn unmount(&mut self, _context: &mut ()) -> Result<(), TestError> {
        self.flags
            .unmount_calls
            .set(self.flags.unmount_calls.get() + 1);
        if self.flags.view_unmount_failures.get() > 0 {
            self.flags
                .view_unmount_failures
                .set(self.flags.view_unmount_failures.get() - 1);
            return Err(if self.flags.chain_unmount_errors.get() {
                TestError::chained("unmount")
            } else {
                TestError::new("unmount")
            });
        }
        if self.script.unmount_failures.get() > 0 {
            self.script
                .unmount_failures
                .set(self.script.unmount_failures.get() - 1);
            return Err(if self.flags.chain_unmount_errors.get() {
                TestError::chained("unmount")
            } else {
                TestError::new("unmount")
            });
        }
        Ok(())
    }
}

struct Fixture {
    ui: UiManager<()>,
    entry: PanelEntry<TestController, TestView>,
    flags: Rc<Flags>,
    log: Rc<RefCell<Vec<String>>>,
}

fn config(
    registration: usize,
    live: usize,
    input: usize,
    nav_queue: usize,
    events: usize,
    navigations: usize,
) -> UiConfig {
    UiConfig::new(registration, live, input, nav_queue, events, navigations).unwrap()
}

fn fixture() -> Fixture {
    fixture_with(config(8, 8, 8, 8, 8, 8), ScopeLevel::Application)
}

fn fixture_with(setup: UiConfig, level: ScopeLevel) -> Fixture {
    let log = Rc::new(RefCell::new(Vec::new()));
    let flags = Flags::new();
    let mut ui = UiManager::new(setup).unwrap();
    let entry = {
        let log = log.clone();
        let flags = flags.clone();
        ui.register(
            PanelKind::new("probe").unwrap(),
            level,
            move |key: &PanelKey, args: &TestArgs| {
                flags.factory_calls.set(flags.factory_calls.get() + 1);
                Ok(UiPanel::new(
                    TestController {
                        model: TestModel {
                            bound_key: Some(key.clone()),
                            ..TestModel::default()
                        },
                        flags: flags.clone(),
                        log: log.clone(),
                    },
                    TestView {
                        script: ViewScript::from_args(args),
                        flags: flags.clone(),
                        log: log.clone(),
                    },
                ))
            },
        )
        .unwrap()
    };
    Fixture {
        ui,
        entry,
        flags,
        log,
    }
}

fn open_app(fixture: &mut Fixture, args: TestArgs) -> PanelHandle {
    match fixture
        .ui
        .open(
            &mut (),
            &fixture.entry,
            PanelScope::Application,
            Some(&args),
        )
        .unwrap()
    {
        OpenOutcome::Created(handle) => handle,
        outcome => panic!("expected Created, got {outcome:?}"),
    }
}

fn open_pet(fixture: &mut Fixture, identity: &ScopeIdentity, label: &'static str) -> PanelHandle {
    match fixture
        .ui
        .open(
            &mut (),
            &fixture.entry,
            PanelScope::Pet(identity.clone()),
            Some(&TestArgs::plain(label)),
        )
        .unwrap()
    {
        OpenOutcome::Created(handle) => handle,
        outcome => panic!("expected Created, got {outcome:?}"),
    }
}

fn log_contains(fixture: &Fixture, needle: &str) -> bool {
    fixture
        .log
        .borrow()
        .iter()
        .any(|line| line.contains(needle))
}

fn log_lines_with(fixture: &Fixture, needle: &str) -> usize {
    fixture
        .log
        .borrow()
        .iter()
        .filter(|line| line.contains(needle))
        .count()
}

fn register_second_kind(
    fixture: &mut Fixture,
    kind: &str,
    level: ScopeLevel,
) -> PanelEntry<TestController, TestView> {
    let flags = fixture.flags.clone();
    let log = fixture.log.clone();
    fixture
        .ui
        .register::<TestController, TestView>(
            PanelKind::new(kind).unwrap(),
            level,
            move |key: &PanelKey, args: &TestArgs| {
                Ok(UiPanel::new(
                    TestController {
                        model: TestModel {
                            bound_key: Some(key.clone()),
                            ..TestModel::default()
                        },
                        flags: flags.clone(),
                        log: log.clone(),
                    },
                    TestView {
                        script: ViewScript::from_args(args),
                        flags: flags.clone(),
                        log: log.clone(),
                    },
                ))
            },
        )
        .unwrap()
}

// ---- 注册与重复注册 ----

#[test]
fn duplicate_kind_registration_is_rejected() {
    let mut fixture = fixture();
    register_second_kind(&mut fixture, "other", ScopeLevel::Application);
    let error = fixture
        .ui
        .register::<TestController, TestView>(
            PanelKind::new("other").unwrap(),
            ScopeLevel::Application,
            move |_key: &PanelKey, _args: &TestArgs| {
                Ok(UiPanel::new(
                    TestController {
                        model: TestModel::default(),
                        flags: Flags::new(),
                        log: Rc::new(RefCell::new(Vec::new())),
                    },
                    TestView {
                        script: ViewScript::from_args(&TestArgs::plain("dup")),
                        flags: Flags::new(),
                        log: Rc::new(RefCell::new(Vec::new())),
                    },
                ))
            },
        )
        .unwrap_err();
    assert!(matches!(error, UiError::KindAlreadyRegistered(ref kind) if kind.as_str() == "other"));
}

#[test]
fn registration_capacity_is_enforced() {
    let mut fixture = fixture_with(config(1, 8, 8, 8, 8, 8), ScopeLevel::Application);
    let error = fixture
        .ui
        .register::<TestController, TestView>(
            PanelKind::new("other").unwrap(),
            ScopeLevel::Application,
            move |_key: &PanelKey, _args: &TestArgs| {
                Ok(UiPanel::new(
                    TestController {
                        model: TestModel::default(),
                        flags: Flags::new(),
                        log: Rc::new(RefCell::new(Vec::new())),
                    },
                    TestView {
                        script: ViewScript::from_args(&TestArgs::plain("other")),
                        flags: Flags::new(),
                        log: Rc::new(RefCell::new(Vec::new())),
                    },
                ))
            },
        )
        .unwrap_err();
    assert!(matches!(error, UiError::RegistrationCapacity));
}

// ---- 按需打开 / 重复打开 / 新入参 ----

#[test]
fn open_reuses_visible_instance_and_routes_new_args() {
    let mut fixture = fixture();
    let handle = open_app(&mut fixture, TestArgs::plain("first"));
    fixture
        .ui
        .send::<TestController, TestView>(&fixture.entry, &handle, TestEvent::Draft("hello"))
        .unwrap();
    fixture.ui.drive(&mut ());
    assert!(log_contains(&fixture, "draft=\"hello\""));

    // 重复打开复用:新入参交给原控制器,草稿不重置,不抢焦点。
    let reused = fixture
        .ui
        .open(
            &mut (),
            &fixture.entry,
            PanelScope::Application,
            Some(&TestArgs::plain("second")),
        )
        .unwrap();
    assert_eq!(reused, OpenOutcome::Reused(handle));
    assert_eq!(fixture.ui.focused(), None);
    fixture.ui.drive(&mut ());
    assert!(log_contains(&fixture, "revisions=[\"second\"]"));
    assert!(log_contains(&fixture, "draft=\"hello\""));
    assert_eq!(fixture.ui.list().len(), 1);

    // 无入参的普通重复打开保持原状态。
    let again = fixture
        .ui
        .open(&mut (), &fixture.entry, PanelScope::Application, None)
        .unwrap();
    assert_eq!(again, OpenOutcome::Reused(handle));
    fixture.ui.drive(&mut ());
    assert!(log_contains(&fixture, "revisions=[\"second\"]"));
}

#[test]
fn factory_binds_same_args_to_each_actual_panel_key() {
    let mut fixture = fixture_with(config(8, 8, 8, 8, 8, 8), ScopeLevel::Pet);
    let pet_a = ScopeIdentity::new("pet-a").unwrap();
    let pet_b = ScopeIdentity::new("pet-b").unwrap();
    let args = TestArgs::plain("same-args");
    let handle_a = open_pet(&mut fixture, &pet_a, args.label);
    let handle_b = open_pet(&mut fixture, &pet_b, args.label);
    assert_ne!(handle_a, handle_b);

    fixture.ui.drive(&mut ());
    assert!(log_contains(&fixture, "key=probe@pet:pet-a"));
    assert!(log_contains(&fixture, "key=probe@pet:pet-b"));
}

#[test]
fn first_open_requires_typed_args() {
    let mut fixture = fixture();
    let error = fixture
        .ui
        .open(&mut (), &fixture.entry, PanelScope::Application, None)
        .unwrap_err();
    assert!(matches!(error, UiError::MissingOpenArgs));
}

#[test]
fn first_open_verifies_via_refresh_and_restore_refreshes_again() {
    let mut fixture = fixture();
    // 核验失败:首次打开失败,不留实例也不留失败记录,也不触碰视图。
    fixture.flags.refresh_fails.set(true);
    let error = fixture
        .ui
        .open(
            &mut (),
            &fixture.entry,
            PanelScope::Application,
            Some(&TestArgs::plain("root")),
        )
        .unwrap_err();
    assert!(matches!(
        error,
        UiError::PanelFault {
            stage: Stage::Refresh,
            ..
        }
    ));
    assert!(fixture.ui.list().is_empty());
    assert!(fixture.ui.pending_releases().is_empty());
    assert_eq!(fixture.flags.unmount_calls.get(), 0);

    // 核验通过后创建;恢复重走核验/刷新边界。
    fixture.flags.refresh_fails.set(false);
    let handle = open_app(&mut fixture, TestArgs::plain("root"));
    fixture.ui.drive(&mut ());
    assert!(log_contains(&fixture, "refreshes=1"));
    fixture.ui.hide(&mut (), &handle).unwrap();
    fixture.ui.show(&mut (), &handle).unwrap();
    fixture.ui.drive(&mut ());
    assert!(log_contains(&fixture, "refreshes=2"));
}

// ---- 作用域隔离 ----

#[test]
fn scopes_are_isolated_and_level_checked() {
    let mut fixture = fixture_with(config(8, 8, 8, 8, 8, 8), ScopeLevel::Pet);
    let pet_a = ScopeIdentity::new("pet-a").unwrap();
    let pet_b = ScopeIdentity::new("pet-b").unwrap();
    let handle_a = open_pet(&mut fixture, &pet_a, "a");
    let handle_b = open_pet(&mut fixture, &pet_b, "b");
    assert_ne!(handle_a, handle_b);
    assert_eq!(fixture.ui.list().len(), 2);

    // 同键重复打开是复用,不是新实例。
    let reuse_a = fixture
        .ui
        .open(
            &mut (),
            &fixture.entry,
            PanelScope::Pet(pet_a.clone()),
            None,
        )
        .unwrap();
    assert_eq!(reuse_a, OpenOutcome::Reused(handle_a));

    // 作用域层级不匹配被拒绝。
    let error = fixture
        .ui
        .open(
            &mut (),
            &fixture.entry,
            PanelScope::Application,
            Some(&TestArgs::plain("app")),
        )
        .unwrap_err();
    assert!(matches!(error, UiError::ScopeLevelRejected { .. }));

    // 只影响 pet-a 的输入不会进入 pet-b。
    fixture
        .ui
        .send::<TestController, TestView>(&fixture.entry, &handle_a, TestEvent::Draft("from-a"))
        .unwrap();
    fixture.ui.drive(&mut ());
    assert_eq!(log_lines_with(&fixture, "draft=\"from-a\""), 1);
    assert_eq!(log_lines_with(&fixture, "draft=\"\""), 1);
}

// ---- 隐藏恢复不丢草稿 ----

#[test]
fn hide_and_restore_keep_drafts() {
    let mut fixture = fixture();
    let handle = open_app(&mut fixture, TestArgs::plain("root"));
    fixture
        .ui
        .send::<TestController, TestView>(&fixture.entry, &handle, TestEvent::Draft("persist"))
        .unwrap();
    fixture.ui.drive(&mut ());
    fixture.ui.hide(&mut (), &handle).unwrap();
    assert_eq!(
        fixture.ui.get(&handle).unwrap().visibility,
        Visibility::Hidden
    );
    let restored = fixture
        .ui
        .open(&mut (), &fixture.entry, PanelScope::Application, None)
        .unwrap();
    assert_eq!(restored, OpenOutcome::Restored(handle));
    fixture.ui.drive(&mut ());
    assert!(log_contains(&fixture, "draft=\"persist\""));
    // 首开与恢复各走一次控制器核验/刷新边界。
    assert!(log_contains(&fixture, "refreshes=2"));
}

#[test]
fn hide_preserves_accepted_plain_inputs_and_drops_stale_loads() {
    let mut fixture = fixture();
    let handle = open_app(&mut fixture, TestArgs::plain("root"));
    fixture
        .ui
        .send::<TestController, TestView>(&fixture.entry, &handle, TestEvent::Draft("kept"))
        .unwrap();
    let ticket = fixture
        .ui
        .begin_load::<TestController, TestView>(&fixture.entry, &handle)
        .unwrap();
    fixture
        .ui
        .submit::<TestController, TestView>(&fixture.entry, &ticket, TestEvent::LoadResult(9))
        .unwrap();

    // 隐藏不清空已接纳普通输入,隐藏期间不分发。
    fixture.ui.hide(&mut (), &handle).unwrap();
    let report = fixture.ui.drive(&mut ());
    assert_eq!(report.inputs_delivered(), 0);
    assert_eq!(report.inputs_pending(), 1);
    assert_eq!(report.renders_attempted(), 0);

    // 恢复后普通输入继续交付;旧加载结果复验失效。
    fixture.ui.show(&mut (), &handle).unwrap();
    let report = fixture.ui.drive(&mut ());
    assert_eq!(report.inputs_delivered(), 1);
    assert_eq!(report.inputs_stale(), 0);
    assert_eq!(report.inputs_pending(), 0);
    assert!(log_contains(&fixture, "draft=\"kept\""));
    assert!(log_contains(&fixture, "load=[]"));
}

// ---- 关闭确认 ----

#[test]
fn close_confirmation_and_stale_rejection() {
    let mut fixture = fixture();
    let handle = open_app(&mut fixture, TestArgs::plain("root"));
    fixture.flags.close_confirm.set(true);
    fixture
        .ui
        .send::<TestController, TestView>(&fixture.entry, &handle, TestEvent::Draft("unsaved"))
        .unwrap();
    fixture.ui.drive(&mut ());

    let CloseRequest::ConfirmationPending(confirmation) =
        fixture.ui.close(&mut (), &handle).unwrap()
    else {
        panic!("expected confirmation");
    };
    assert!(fixture.ui.contains(&handle));

    // 未确认不释放;丢弃凭据即取消。
    drop(confirmation.clone());
    assert!(fixture.ui.contains(&handle));

    // 新事件改变控制器后,旧确认版本失效,不得丢弃新草稿。
    fixture
        .ui
        .send::<TestController, TestView>(&fixture.entry, &handle, TestEvent::Draft("-more"))
        .unwrap();
    fixture.ui.drive(&mut ());
    assert!(matches!(
        fixture.ui.confirm_close(&mut (), &confirmation),
        Err(UiError::ConfirmationStale)
    ));
    assert!(fixture.ui.contains(&handle));

    // 重新请求关闭得到绑定当前版本的新确认,确认后释放。
    let CloseRequest::ConfirmationPending(fresh) = fixture.ui.close(&mut (), &handle).unwrap()
    else {
        panic!("expected fresh confirmation");
    };
    fixture.ui.confirm_close(&mut (), &fresh).unwrap();
    assert!(!fixture.ui.contains(&handle));
    assert!(log_contains(&fixture, "draft=\"unsaved-more\""));
}

#[test]
fn confirmations_invalidate_before_state_changing_callbacks() {
    let mut fixture = fixture();
    let handle = open_app(&mut fixture, TestArgs::plain("root"));
    fixture.flags.close_confirm.set(true);
    fixture
        .ui
        .send::<TestController, TestView>(&fixture.entry, &handle, TestEvent::Draft("d0"))
        .unwrap();
    fixture.ui.drive(&mut ());

    // 返回 Err 的 apply 也在调用前使旧确认失效。
    let CloseRequest::ConfirmationPending(first) = fixture.ui.close(&mut (), &handle).unwrap()
    else {
        panic!("expected confirmation");
    };
    fixture
        .ui
        .send::<TestController, TestView>(&fixture.entry, &handle, TestEvent::Fail("apply"))
        .unwrap();
    fixture.ui.drive(&mut ());
    assert!(matches!(
        fixture.ui.confirm_close(&mut (), &first),
        Err(UiError::ConfirmationStale)
    ));

    // suspend/refresh 边界同样先失效旧确认。
    let CloseRequest::ConfirmationPending(second) = fixture.ui.close(&mut (), &handle).unwrap()
    else {
        panic!("expected confirmation");
    };
    fixture.ui.hide(&mut (), &handle).unwrap();
    fixture.ui.show(&mut (), &handle).unwrap();
    assert!(matches!(
        fixture.ui.confirm_close(&mut (), &second),
        Err(UiError::ConfirmationStale)
    ));

    // revise 亦然:复用路径的新入参使旧确认失效。
    let CloseRequest::ConfirmationPending(third) = fixture.ui.close(&mut (), &handle).unwrap()
    else {
        panic!("expected confirmation");
    };
    fixture
        .ui
        .open(
            &mut (),
            &fixture.entry,
            PanelScope::Application,
            Some(&TestArgs::plain("revised")),
        )
        .unwrap();
    assert!(matches!(
        fixture.ui.confirm_close(&mut (), &third),
        Err(UiError::ConfirmationStale)
    ));
}

#[test]
fn revise_failure_invalidates_load_tickets_and_close_confirmation() {
    let mut fixture = fixture();
    let handle = open_app(&mut fixture, TestArgs::plain("root"));
    fixture.flags.close_confirm.set(true);
    fixture
        .ui
        .send::<TestController, TestView>(&fixture.entry, &handle, TestEvent::Draft("draft"))
        .unwrap();
    fixture.ui.drive(&mut ());
    let CloseRequest::ConfirmationPending(confirmation) =
        fixture.ui.close(&mut (), &handle).unwrap()
    else {
        panic!("expected confirmation");
    };
    let ticket = fixture
        .ui
        .begin_load::<TestController, TestView>(&fixture.entry, &handle)
        .unwrap();
    fixture.flags.revise_fails.set(true);
    let error = fixture
        .ui
        .open(
            &mut (),
            &fixture.entry,
            PanelScope::Application,
            Some(&TestArgs::plain("failed-revision")),
        )
        .unwrap_err();
    assert!(matches!(
        error,
        UiError::PanelFault {
            stage: Stage::ReviseArgs,
            ..
        }
    ));
    assert!(matches!(
        fixture.ui.confirm_close(&mut (), &confirmation),
        Err(UiError::ConfirmationStale)
    ));
    let rejected = fixture
        .ui
        .submit::<TestController, TestView>(&fixture.entry, &ticket, TestEvent::LoadResult(9))
        .unwrap_err();
    assert_eq!(rejected.reason, RejectionReason::TicketStale);
}

#[test]
fn repeated_mutating_close_requests_invalidate_older_confirmation() {
    let mut fixture = fixture();
    let handle = open_app(&mut fixture, TestArgs::plain("root"));
    fixture.flags.close_confirm.set(true);
    fixture
        .ui
        .send::<TestController, TestView>(&fixture.entry, &handle, TestEvent::Draft("draft"))
        .unwrap();
    fixture.ui.drive(&mut ());
    let CloseRequest::ConfirmationPending(first) = fixture.ui.close(&mut (), &handle).unwrap()
    else {
        panic!("expected confirmation");
    };
    let CloseRequest::ConfirmationPending(second) = fixture.ui.close(&mut (), &handle).unwrap()
    else {
        panic!("expected confirmation");
    };
    assert!(matches!(
        fixture.ui.confirm_close(&mut (), &first),
        Err(UiError::ConfirmationStale)
    ));
    fixture.ui.confirm_close(&mut (), &second).unwrap();
}

#[test]
fn close_and_confirm_close_reject_while_inputs_pending() {
    let mut fixture = fixture();
    let handle = open_app(&mut fixture, TestArgs::plain("root"));
    fixture
        .ui
        .send::<TestController, TestView>(&fixture.entry, &handle, TestEvent::Draft("unsent"))
        .unwrap();
    // 已接纳未处理输入时普通关闭明确拒绝,不静默丢弃。
    assert!(matches!(
        fixture.ui.close(&mut (), &handle),
        Err(UiError::CloseBlockedByPendingInputs)
    ));
    assert!(fixture.ui.contains(&handle));

    fixture.flags.close_confirm.set(true);
    fixture.ui.drive(&mut ());
    let CloseRequest::ConfirmationPending(confirmation) =
        fixture.ui.close(&mut (), &handle).unwrap()
    else {
        panic!("expected confirmation");
    };
    fixture
        .ui
        .send::<TestController, TestView>(&fixture.entry, &handle, TestEvent::Draft("-more"))
        .unwrap();
    assert!(matches!(
        fixture.ui.confirm_close(&mut (), &confirmation),
        Err(UiError::CloseBlockedByPendingInputs)
    ));

    // 驱动消费后旧凭据因版本前进被拒,重新确认可完成关闭。
    fixture.ui.drive(&mut ());
    assert!(matches!(
        fixture.ui.confirm_close(&mut (), &confirmation),
        Err(UiError::ConfirmationStale)
    ));
    let CloseRequest::ConfirmationPending(fresh) = fixture.ui.close(&mut (), &handle).unwrap()
    else {
        panic!("expected fresh confirmation");
    };
    fixture.ui.confirm_close(&mut (), &fresh).unwrap();
    assert!(!fixture.ui.contains(&handle));
}

#[test]
fn close_without_draft_releases_directly() {
    let mut fixture = fixture();
    let handle = open_app(&mut fixture, TestArgs::plain("root"));
    // 已保存的草稿不阻塞关闭:close_request 只看未保存草稿。
    fixture
        .ui
        .send::<TestController, TestView>(&fixture.entry, &handle, TestEvent::Draft("temp"))
        .unwrap();
    fixture.ui.drive(&mut ());
    fixture
        .ui
        .send::<TestController, TestView>(&fixture.entry, &handle, TestEvent::Save)
        .unwrap();
    fixture.ui.drive(&mut ());
    assert!(log_contains(&fixture, "saved=[\"temp\"]"));
    let request = fixture.ui.close(&mut (), &handle).unwrap();
    assert!(request.is_released());
    assert!(!fixture.ui.contains(&handle));
    // 普通关闭不触发敏感缓存清除边界。
    assert_eq!(fixture.flags.revocations.get(), 0);
}

// ---- 旧句柄 / 跨中心 ----

#[test]
fn stale_handle_after_reopen_and_foreign_center() {
    let mut fixture = fixture();
    let old = open_app(&mut fixture, TestArgs::plain("first"));
    fixture.ui.close(&mut (), &old).unwrap();
    let fresh = open_app(&mut fixture, TestArgs::plain("second"));
    assert_ne!(old, fresh);

    // 旧句柄不得命中新实例,也不能退回按 key 路由。
    assert!(fixture.ui.get(&old).is_none());
    assert!(!fixture.ui.contains(&old));
    assert!(matches!(
        fixture.ui.hide(&mut (), &old),
        Err(UiError::InstanceNotFound)
    ));
    assert!(matches!(
        fixture.ui.show(&mut (), &old),
        Err(UiError::InstanceNotFound)
    ));
    assert!(matches!(
        fixture.ui.focus(&old),
        Err(UiError::InstanceNotFound)
    ));
    assert!(matches!(
        fixture.ui.close(&mut (), &old),
        Err(UiError::InstanceNotFound)
    ));

    // 跨中心句柄被拒绝。
    let mut other = fixture_with(config(8, 8, 8, 8, 8, 8), ScopeLevel::Application);
    assert!(matches!(
        other.ui.focus(&fresh),
        Err(UiError::ForeignHandle)
    ));
    assert!(other.ui.get(&fresh).is_none());
    let foreign_open = other.ui.open(
        &mut (),
        &fixture.entry,
        PanelScope::Application,
        Some(&TestArgs::plain("x")),
    );
    assert!(matches!(foreign_open, Err(UiError::ForeignEntry)));

    // 跨中心提交被拒,负载交还。
    let rejection = other
        .ui
        .send::<TestController, TestView>(&fixture.entry, &fresh, TestEvent::Add(1))
        .unwrap_err();
    assert_eq!(rejection.reason, RejectionReason::ForeignCenter);
    assert_eq!(rejection.into_event(), TestEvent::Add(1));
}

// ---- 焦点 ----

#[test]
fn focus_rules_and_clearing() {
    let mut fixture = fixture();
    let handle = open_app(&mut fixture, TestArgs::plain("root"));
    fixture.ui.focus(&handle).unwrap();
    assert_eq!(fixture.ui.focused(), Some(handle));

    // 隐藏焦点面板清除焦点,宿主负责移交。
    assert!(fixture.ui.hide(&mut (), &handle).unwrap());
    assert_eq!(fixture.ui.focused(), None);
    // 隐藏面板不能获得焦点。
    assert!(matches!(
        fixture.ui.focus(&handle),
        Err(UiError::NotVisible)
    ));

    fixture.ui.show(&mut (), &handle).unwrap();
    fixture.ui.focus(&handle).unwrap();
    fixture.ui.close(&mut (), &handle).unwrap();
    assert_eq!(fixture.ui.focused(), None);
}

// ---- 迟到加载双重复验 ----

#[test]
fn load_tickets_stale_at_enqueue_and_dispatch() {
    let mut fixture = fixture();
    let handle = open_app(&mut fixture, TestArgs::plain("root"));

    let first = fixture
        .ui
        .begin_load::<TestController, TestView>(&fixture.entry, &handle)
        .unwrap();
    let second = fixture
        .ui
        .begin_load::<TestController, TestView>(&fixture.entry, &handle)
        .unwrap();
    // 新加载使旧票据在入队前失效。
    let rejection = fixture
        .ui
        .submit::<TestController, TestView>(&fixture.entry, &first, TestEvent::LoadResult(1))
        .unwrap_err();
    assert_eq!(rejection.reason, RejectionReason::TicketStale);
    fixture
        .ui
        .submit::<TestController, TestView>(&fixture.entry, &second, TestEvent::LoadResult(2))
        .unwrap();
    fixture.ui.drive(&mut ());
    assert!(log_contains(&fixture, "load=[2]"));

    // 可见性改变使票据失效;隐藏面板不接收展示结果。
    let third = fixture
        .ui
        .begin_load::<TestController, TestView>(&fixture.entry, &handle)
        .unwrap();
    fixture.ui.hide(&mut (), &handle).unwrap();
    let rejection = fixture
        .ui
        .submit::<TestController, TestView>(&fixture.entry, &third, TestEvent::LoadResult(3))
        .unwrap_err();
    assert_eq!(rejection.reason, RejectionReason::NotVisible);

    // 入队有效、分发前因更新加载而失效:不投递,计入 stale。
    fixture.ui.show(&mut (), &handle).unwrap();
    let fourth = fixture
        .ui
        .begin_load::<TestController, TestView>(&fixture.entry, &handle)
        .unwrap();
    fixture
        .ui
        .submit::<TestController, TestView>(&fixture.entry, &fourth, TestEvent::LoadResult(4))
        .unwrap();
    // 提交之后、分发之前发生新加载:实际分发前复验拒绝旧结果。
    let _fifth_epoch = fixture
        .ui
        .begin_load::<TestController, TestView>(&fixture.entry, &handle)
        .unwrap();
    let report = fixture.ui.drive(&mut ());
    assert_eq!(report.inputs_stale(), 0);
    assert!(log_contains(&fixture, "load=[2]"));

    // 关闭后票据对应实例消失。
    fixture.ui.show(&mut (), &handle).unwrap();
    let fifth = fixture
        .ui
        .begin_load::<TestController, TestView>(&fixture.entry, &handle)
        .unwrap();
    fixture.ui.close(&mut (), &handle).unwrap();
    let rejection = fixture
        .ui
        .submit::<TestController, TestView>(&fixture.entry, &fifth, TestEvent::LoadResult(5))
        .unwrap_err();
    assert_eq!(rejection.reason, RejectionReason::InstanceGone);
}

// ---- 容量与失败清理 ----

#[test]
fn instance_capacity_counts_failed_releases() {
    let mut fixture = fixture_with(config(8, 1, 8, 8, 8, 8), ScopeLevel::Pet);
    let pet_a = ScopeIdentity::new("pet-a").unwrap();
    let pet_b = ScopeIdentity::new("pet-b").unwrap();
    let handle = open_pet(&mut fixture, &pet_a, "a");
    assert!(matches!(
        fixture.ui.open(
            &mut (),
            &fixture.entry,
            PanelScope::Pet(pet_b.clone()),
            Some(&TestArgs::plain("b"))
        ),
        Err(UiError::InstanceCapacity)
    ));

    // 释放失败:记录保留,容量不归还。
    fixture.flags.fail_release.set(true);
    let error = fixture.ui.close(&mut (), &handle).unwrap_err();
    let UiError::ReleaseIncomplete { record, stage, .. } = error else {
        panic!("expected ReleaseIncomplete");
    };
    assert_eq!(stage, Stage::ControllerRelease);
    assert_eq!(fixture.ui.pending_releases().len(), 1);
    assert!(
        fixture
            .ui
            .open(
                &mut (),
                &fixture.entry,
                PanelScope::Pet(pet_b.clone()),
                Some(&TestArgs::plain("b"))
            )
            .is_err()
    );

    // 重试成功后归还额度;旧句柄已失效,不影响重开的新面板。
    fixture.flags.fail_release.set(false);
    fixture.ui.retry_release(&mut (), record).unwrap();
    assert!(fixture.ui.pending_releases().is_empty());
    open_pet(&mut fixture, &pet_b, "b");
    assert!(matches!(
        fixture.ui.close(&mut (), &handle),
        Err(UiError::InstanceNotFound)
    ));
}

#[test]
fn pending_release_ids_are_center_bound_and_not_reusable() {
    let mut fixture = fixture();
    let handle = open_app(&mut fixture, TestArgs::plain("root"));
    fixture.flags.fail_release.set(true);
    let UiError::ReleaseIncomplete { record, .. } = fixture.ui.close(&mut (), &handle).unwrap_err()
    else {
        panic!("expected ReleaseIncomplete");
    };
    fixture.flags.fail_release.set(false);
    fixture.ui.retry_release(&mut (), record).unwrap();
    // 成功重试后同一编号不再命中,外部无法从原始数还原编号。
    assert!(matches!(
        fixture.ui.retry_release(&mut (), record),
        Err(UiError::PendingReleaseNotFound)
    ));
    // 其他中心持有的编号不命中本中心记录。
    let mut other = fixture_with(config(8, 8, 8, 8, 8, 8), ScopeLevel::Application);
    assert!(matches!(
        other.ui.retry_release(&mut (), record),
        Err(UiError::PendingReleaseNotFound)
    ));
}

#[test]
fn mount_failure_cleans_up_and_failed_cleanup_keeps_record() {
    // 挂载失败 + 清理成功:仍会卸载部分创建的资源,不留半注册实例或失败记录。
    let mut fixture = fixture();
    let mut args = TestArgs::plain("broken-mount");
    args.mount_failures = 1;
    let error = fixture
        .ui
        .open(
            &mut (),
            &fixture.entry,
            PanelScope::Application,
            Some(&args),
        )
        .unwrap_err();
    assert!(matches!(
        error,
        UiError::PanelFault {
            stage: Stage::Mount,
            ..
        }
    ));
    assert_eq!(fixture.flags.unmount_calls.get(), 1);
    assert!(fixture.ui.pending_releases().is_empty());
    assert!(fixture.ui.list().is_empty());

    // 挂载失败 + 清理失败:保留可重试记录并计入容量。
    fixture.flags.fail_release.set(true);
    let error = fixture
        .ui
        .open(
            &mut (),
            &fixture.entry,
            PanelScope::Application,
            Some(&args),
        )
        .unwrap_err();
    let UiError::ReleaseIncomplete { record, stage, .. } = error else {
        panic!("expected ReleaseIncomplete");
    };
    assert_eq!(stage, Stage::ControllerRelease);
    assert_eq!(fixture.ui.pending_releases().len(), 1);
    fixture.flags.fail_release.set(false);
    fixture.ui.retry_release(&mut (), record).unwrap();
    assert!(fixture.ui.pending_releases().is_empty());

    // 清理成功后可以正常打开。
    open_app(&mut fixture, TestArgs::plain("healthy"));
}

#[test]
fn first_open_cleanup_failure_keeps_initial_and_cleanup_errors() {
    let mut fixture = fixture();
    let mut args = TestArgs::plain("broken-mount");
    args.mount_failures = 1;
    fixture.flags.fail_release.set(true);
    fixture.flags.view_unmount_failures.set(1);
    fixture.flags.chain_release_errors.set(true);
    fixture.flags.chain_unmount_errors.set(true);
    let error = fixture
        .ui
        .open(
            &mut (),
            &fixture.entry,
            PanelScope::Application,
            Some(&args),
        )
        .unwrap_err();
    let UiError::ReleaseIncomplete { record, stage, .. } = error else {
        panic!("expected ReleaseIncomplete");
    };
    assert_eq!(stage, Stage::ControllerRelease);
    // 初始挂载错误与清理错误(卸载+释放)一并保留。
    assert_eq!(
        error
            .initial_source()
            .map(<dyn Error>::to_string)
            .as_deref(),
        Some("test error at mount")
    );
    let cleanup_text = error
        .source()
        .map(<dyn Error>::to_string)
        .unwrap_or_default();
    assert!(cleanup_text.contains("test error at unmount"));
    assert!(cleanup_text.contains("test error at release"));
    let aggregate = error
        .source()
        .and_then(|source| source.downcast_ref::<AggregateError>())
        .expect("aggregate error is publicly inspectable");
    assert_eq!(
        aggregate
            .first_error()
            .downcast_ref::<TestError>()
            .map(|error| error.stage.as_str()),
        Some("unmount")
    );
    assert_eq!(
        aggregate
            .second_error()
            .downcast_ref::<TestError>()
            .map(|error| error.stage.as_str()),
        Some("release")
    );
    assert!(
        aggregate
            .first_error()
            .source()
            .is_some_and(|source| source.downcast_ref::<RootCause>().is_some())
    );
    assert!(
        aggregate
            .second_error()
            .source()
            .is_some_and(|source| source.downcast_ref::<RootCause>().is_some())
    );
    assert_eq!(fixture.ui.pending_releases().len(), 1);

    // 记录可重试:卸载与释放都会重试,成功后额度归还。
    fixture.flags.fail_release.set(false);
    fixture.flags.view_unmount_failures.set(0);
    fixture.ui.retry_release(&mut (), record).unwrap();
    assert!(fixture.ui.pending_releases().is_empty());
    assert_eq!(fixture.flags.unmount_calls.get(), 2);
    open_app(&mut fixture, TestArgs::plain("healthy"));
}

#[test]
fn first_open_unmount_failure_keeps_capacity_after_controller_release() {
    let mut fixture = fixture_with(config(8, 1, 8, 8, 8, 8), ScopeLevel::Pet);
    let pet_a = ScopeIdentity::new("pet-a").unwrap();
    let pet_b = ScopeIdentity::new("pet-b").unwrap();
    let mut args = TestArgs::plain("mount-fails");
    args.mount_failures = 1;
    args.unmount_failures = 1;
    let error = fixture
        .ui
        .open(
            &mut (),
            &fixture.entry,
            PanelScope::Pet(pet_a.clone()),
            Some(&args),
        )
        .unwrap_err();
    let UiError::ReleaseIncomplete { record, stage, .. } = error else {
        panic!("expected pending release");
    };
    assert_eq!(stage, Stage::Unmount);
    assert_eq!(fixture.flags.release_calls.get(), 1);
    assert_eq!(fixture.ui.pending_releases().len(), 1);
    assert!(matches!(
        fixture.ui.open(
            &mut (),
            &fixture.entry,
            PanelScope::Pet(pet_b),
            Some(&TestArgs::plain("blocked")),
        ),
        Err(UiError::InstanceCapacity)
    ));

    fixture.ui.retry_release(&mut (), record).unwrap();
    assert_eq!(fixture.flags.release_calls.get(), 1);
    assert!(fixture.ui.pending_releases().is_empty());
    assert!(matches!(
        fixture
            .ui
            .open(
                &mut (),
                &fixture.entry,
                PanelScope::Pet(pet_a),
                Some(&TestArgs::plain("reopened")),
            )
            .unwrap(),
        OpenOutcome::Created(_)
    ));
}

#[test]
fn restore_mount_failure_with_unmount_failure_is_quarantined() {
    let mut fixture = fixture();
    let handle = open_app(&mut fixture, TestArgs::plain("root"));
    fixture.ui.hide(&mut (), &handle).unwrap();

    // 恢复时挂载失败,随后的卸载亦失败:卸载错误不被吞,初始挂载错误保留。
    fixture.flags.view_mount_failures.set(1);
    fixture.flags.view_unmount_failures.set(1);
    let error = fixture.ui.show(&mut (), &handle).unwrap_err();
    let UiError::ReleaseIncomplete {
        record,
        stage,
        source,
        initial,
        ..
    } = &error
    else {
        panic!("expected PanelFault");
    };
    assert_eq!(*stage, Stage::Unmount);
    assert_eq!(source.to_string(), "test error at unmount");
    assert!(initial.is_some());
    assert_eq!(*stage, Stage::Unmount);
    assert_eq!(
        error
            .initial_source()
            .map(<dyn Error>::to_string)
            .as_deref(),
        Some("test error at mount")
    );
    assert!(fixture.ui.list().is_empty());
    assert!(!fixture.ui.contains(&handle));
    assert_eq!(fixture.ui.pending_releases().len(), 1);
    assert!(matches!(
        fixture.ui.focus(&handle),
        Err(UiError::InstanceNotFound)
    ));
    assert_eq!(fixture.flags.mount_calls.get(), 2);

    fixture.flags.view_mount_failures.set(0);
    fixture.flags.view_unmount_failures.set(0);
    fixture.ui.retry_release(&mut (), *record).unwrap();
    assert_eq!(fixture.flags.mount_calls.get(), 2);
    assert_eq!(fixture.flags.release_calls.get(), 1);
    let replacement = open_app(&mut fixture, TestArgs::plain("replacement"));
    assert_ne!(replacement, handle);
    fixture.ui.drive(&mut ());
    assert!(log_contains(&fixture, "revisions=[]"));
}

#[test]
fn p2_pending_cleanup_blocks_same_key_before_factory_and_allows_other_keys() {
    for failure_kind in 0..3 {
        let mut fixture = fixture_with(config(8, 8, 8, 8, 8, 8), ScopeLevel::Pet);
        let first_pet = ScopeIdentity::new("pending-first").unwrap();
        let other_pet = ScopeIdentity::new("pending-other").unwrap();
        let first_scope = PanelScope::Pet(first_pet.clone());
        let error = if failure_kind == 0 {
            fixture.flags.view_mount_failures.set(1);
            fixture.flags.view_unmount_failures.set(2);
            fixture
                .ui
                .open(
                    &mut (),
                    &fixture.entry,
                    first_scope.clone(),
                    Some(&TestArgs::plain("partial-mount")),
                )
                .unwrap_err()
        } else {
            let handle = open_pet(&mut fixture, &first_pet, "first");
            fixture.flags.view_unmount_failures.set(2);
            if failure_kind == 1 {
                fixture.ui.close(&mut (), &handle).unwrap_err()
            } else {
                fixture.ui.hide(&mut (), &handle).unwrap_err()
            }
        };
        let UiError::ReleaseIncomplete { record, key, .. } = error else {
            panic!("expected pending release for failure {failure_kind}");
        };
        assert_eq!(fixture.flags.factory_calls.get(), 1);
        let error = fixture
            .ui
            .open(
                &mut (),
                &fixture.entry,
                first_scope.clone(),
                Some(&TestArgs::plain("duplicate")),
            )
            .unwrap_err();
        assert!(error.to_string().contains(&record.to_string()));
        assert!(matches!(
            error,
            UiError::PendingReleaseForKey {
                record: refused_record,
                key: ref refused_key,
            } if refused_record == record && refused_key == &key
        ));
        assert_eq!(fixture.flags.factory_calls.get(), 1);
        assert!(fixture.ui.list().is_empty());
        assert_eq!(fixture.ui.pending_releases()[0].key, key);

        let other_handle = open_pet(&mut fixture, &other_pet, "other");
        assert_eq!(fixture.flags.factory_calls.get(), 2);
        assert!(fixture.ui.contains(&other_handle));
        assert!(fixture.ui.retry_release(&mut (), record).is_err());
        let pending_before_retry = fixture.ui.pending_releases();
        let reopen_error = fixture.ui.open(
            &mut (),
            &fixture.entry,
            first_scope,
            Some(&TestArgs::plain("retry-pending")),
        );
        assert!(matches!(
            reopen_error,
            Err(UiError::PendingReleaseForKey {
                record: refused_record,
                key: ref refused_key,
            }) if refused_record == record && refused_key == &key
        ));
        assert_eq!(fixture.flags.factory_calls.get(), 2);
        assert_eq!(fixture.ui.pending_releases(), pending_before_retry);
        fixture.ui.retry_release(&mut (), record).unwrap();
        assert!(fixture.ui.pending_releases().is_empty());
        let reopened = open_pet(&mut fixture, &first_pet, "reopened");
        assert!(fixture.ui.contains(&reopened));
        assert!(fixture.ui.contains(&other_handle));
        assert_eq!(fixture.flags.factory_calls.get(), 3);
    }
}

#[test]
fn p2_hidden_invalidated_load_does_not_block_close() {
    let mut fixture = fixture();
    let handle = open_app(&mut fixture, TestArgs::plain("root"));
    let ticket = fixture
        .ui
        .begin_load::<TestController, TestView>(&fixture.entry, &handle)
        .unwrap();
    fixture
        .ui
        .submit::<TestController, TestView>(&fixture.entry, &ticket, TestEvent::LoadResult(7))
        .unwrap();
    fixture.ui.hide(&mut (), &handle).unwrap();
    assert_eq!(fixture.ui.get(&handle).unwrap().pending_inputs, 0);
    for _ in 0..16 {
        let report = fixture.ui.drive(&mut ());
        assert_eq!(report.inputs_pending(), 0);
        assert_eq!(report.inputs_delivered(), 0);
    }
    fixture.ui.close(&mut (), &handle).unwrap();
    assert!(!fixture.ui.contains(&handle));
    assert!(fixture.ui.pending_releases().is_empty());
}

#[test]
fn p2_hidden_invalidated_load_keeps_plain_inputs_and_close_protection() {
    let mut fixture = fixture();
    let handle = open_app(&mut fixture, TestArgs::plain("root"));
    let ticket = fixture
        .ui
        .begin_load::<TestController, TestView>(&fixture.entry, &handle)
        .unwrap();
    fixture
        .ui
        .submit::<TestController, TestView>(&fixture.entry, &ticket, TestEvent::LoadResult(7))
        .unwrap();
    fixture
        .ui
        .send::<TestController, TestView>(&fixture.entry, &handle, TestEvent::Draft("retained"))
        .unwrap();
    fixture.ui.hide(&mut (), &handle).unwrap();
    assert_eq!(fixture.ui.get(&handle).unwrap().pending_inputs, 1);
    assert!(matches!(
        fixture.ui.close(&mut (), &handle),
        Err(UiError::CloseBlockedByPendingInputs)
    ));
    assert_eq!(fixture.ui.drive(&mut ()).inputs_delivered(), 0);
    fixture.ui.show(&mut (), &handle).unwrap();
    assert_eq!(fixture.ui.drive(&mut ()).inputs_delivered(), 1);
    fixture.ui.drive(&mut ());
    assert!(log_contains(&fixture, r#"draft="retained""#));
    assert!(log_contains(&fixture, "load=[]"));
}

#[test]
fn p2_new_load_reclaims_stale_capacity_but_current_result_blocks_close() {
    let mut fixture = fixture_with(config(8, 8, 1, 8, 8, 8), ScopeLevel::Application);
    let handle = open_app(&mut fixture, TestArgs::plain("root"));
    let old_ticket = fixture
        .ui
        .begin_load::<TestController, TestView>(&fixture.entry, &handle)
        .unwrap();
    fixture
        .ui
        .submit::<TestController, TestView>(&fixture.entry, &old_ticket, TestEvent::LoadResult(1))
        .unwrap();
    let current_ticket = fixture
        .ui
        .begin_load::<TestController, TestView>(&fixture.entry, &handle)
        .unwrap();
    assert_eq!(fixture.ui.get(&handle).unwrap().pending_inputs, 0);
    fixture
        .ui
        .submit::<TestController, TestView>(
            &fixture.entry,
            &current_ticket,
            TestEvent::LoadResult(2),
        )
        .unwrap();
    assert!(matches!(
        fixture.ui.close(&mut (), &handle),
        Err(UiError::CloseBlockedByPendingInputs)
    ));
    fixture.ui.drive(&mut ());
    fixture.ui.drive(&mut ());
    assert!(log_contains(&fixture, "load=[2]"));
    fixture.ui.close(&mut (), &handle).unwrap();
    assert!(!fixture.ui.contains(&handle));
}

#[test]
fn p2_failed_revision_discards_invalidated_loads_but_keeps_plain_inputs() {
    let mut fixture = fixture();
    let handle = open_app(&mut fixture, TestArgs::plain("root"));
    let ticket = fixture
        .ui
        .begin_load::<TestController, TestView>(&fixture.entry, &handle)
        .unwrap();
    fixture
        .ui
        .send::<TestController, TestView>(&fixture.entry, &handle, TestEvent::Draft("retained"))
        .unwrap();
    fixture
        .ui
        .submit::<TestController, TestView>(&fixture.entry, &ticket, TestEvent::LoadResult(9))
        .unwrap();
    fixture.flags.revise_fails.set(true);
    assert!(
        fixture
            .ui
            .open(
                &mut (),
                &fixture.entry,
                PanelScope::Application,
                Some(&TestArgs::plain("failed"))
            )
            .is_err()
    );
    assert_eq!(fixture.ui.get(&handle).unwrap().pending_inputs, 1);
    assert!(matches!(
        fixture.ui.close(&mut (), &handle),
        Err(UiError::CloseBlockedByPendingInputs)
    ));
    assert_eq!(fixture.ui.drive(&mut ()).inputs_delivered(), 1);
    fixture.ui.drive(&mut ());
    assert!(log_contains(&fixture, r#"draft="retained""#));
    assert!(log_contains(&fixture, "load=[]"));
}

#[test]
fn p2_hidden_invalidated_load_allows_confirmed_close() {
    let mut fixture = fixture();
    let handle = open_app(&mut fixture, TestArgs::plain("root"));
    fixture.flags.close_confirm.set(true);
    fixture
        .ui
        .send::<TestController, TestView>(
            &fixture.entry,
            &handle,
            TestEvent::Draft("confirm-hidden-draft"),
        )
        .unwrap();
    fixture.ui.drive(&mut ());
    let ticket = fixture
        .ui
        .begin_load::<TestController, TestView>(&fixture.entry, &handle)
        .unwrap();
    fixture
        .ui
        .submit::<TestController, TestView>(&fixture.entry, &ticket, TestEvent::LoadResult(1))
        .unwrap();
    fixture.ui.hide(&mut (), &handle).unwrap();
    let CloseRequest::ConfirmationPending(confirmation) =
        fixture.ui.close(&mut (), &handle).unwrap()
    else {
        panic!("expected hidden close confirmation");
    };
    fixture.ui.confirm_close(&mut (), &confirmation).unwrap();
    assert!(!fixture.ui.contains(&handle));
}

#[test]
fn hide_unmount_failure_quarantines_and_rejects_interaction() {
    let mut fixture = fixture();
    let handle = open_app(&mut fixture, TestArgs::plain("root"));
    fixture.ui.focus(&handle).unwrap();
    fixture.flags.view_unmount_failures.set(1);
    let error = fixture.ui.hide(&mut (), &handle).unwrap_err();
    assert!(matches!(
        error,
        UiError::ReleaseIncomplete {
            stage: Stage::Unmount,
            ..
        }
    ));
    assert_eq!(fixture.ui.focused(), None);
    assert!(fixture.ui.list().is_empty());
    assert!(!fixture.ui.contains(&handle));
    assert_eq!(fixture.ui.pending_releases().len(), 1);
    assert_eq!(fixture.ui.drive(&mut ()).renders_attempted(), 0);
    let rejected = fixture
        .ui
        .send::<TestController, TestView>(&fixture.entry, &handle, TestEvent::Add(1))
        .unwrap_err();
    assert_eq!(rejected.reason, RejectionReason::InstanceGone);
}

#[test]
fn scope_revocation_clears_cache_on_existing_pending_release() {
    let mut fixture = fixture_with(config(8, 8, 8, 8, 8, 8), ScopeLevel::Pet);
    let pet = ScopeIdentity::new("pet-revoke").unwrap();
    let handle = open_pet(&mut fixture, &pet, "root");
    fixture.flags.fail_release.set(true);
    let UiError::ReleaseIncomplete { record, .. } = fixture.ui.close(&mut (), &handle).unwrap_err()
    else {
        panic!("expected pending release");
    };
    assert_eq!(fixture.flags.revocations.get(), 0);

    let report = fixture.ui.force_release_scope(
        &mut (),
        &PanelScope::Pet(pet),
        ReleaseCause::IdentityRemoved,
    );
    assert_eq!(report.failed().len(), 1);
    assert_eq!(fixture.flags.revocations.get(), 1);
    assert_eq!(
        fixture.ui.pending_releases()[0].cause,
        Some(ReleaseCause::IdentityRemoved)
    );

    fixture.flags.fail_release.set(false);
    fixture.ui.retry_release(&mut (), record).unwrap();
    assert_eq!(fixture.flags.revocations.get(), 1);
    assert!(fixture.ui.pending_releases().is_empty());
}

#[test]
fn failed_release_clears_focus_and_force_purges_despite_unmount_failure() {
    let mut fixture = fixture_with(config(8, 8, 8, 8, 8, 8), ScopeLevel::Pet);
    let first_pet = ScopeIdentity::new("first-release").unwrap();
    let second_pet = ScopeIdentity::new("second-release").unwrap();
    let handle = open_pet(&mut fixture, &first_pet, "root");
    fixture.ui.focus(&handle).unwrap();
    fixture.flags.fail_release.set(true);

    // 释放转入失败记录时立即清焦点。
    let UiError::ReleaseIncomplete { record, .. } = fixture.ui.close(&mut (), &handle).unwrap_err()
    else {
        panic!("expected ReleaseIncomplete");
    };
    assert_eq!(fixture.ui.focused(), None);
    assert_eq!(fixture.ui.pending_releases().len(), 1);
    // 普通关闭失败不触发敏感缓存清除。
    assert_eq!(fixture.flags.revocations.get(), 0);

    // 强制释放即使卸载失败也调用控制器敏感缓存清除边界。
    fixture.flags.view_unmount_failures.set(1);
    let second = open_pet(&mut fixture, &second_pet, "second");
    fixture.ui.focus(&second).unwrap();
    let UiError::ReleaseIncomplete {
        record: second_record,
        ..
    } = fixture
        .ui
        .force_release(&mut (), &second, ReleaseCause::ContextRevoked)
        .unwrap_err()
    else {
        panic!("expected ReleaseIncomplete");
    };
    assert_eq!(fixture.ui.focused(), None);
    assert_eq!(fixture.flags.revocations.get(), 1);

    // 失败记录可重试;普通记录重试不再触发撤权边界。
    fixture.flags.fail_release.set(false);
    fixture.flags.view_unmount_failures.set(0);
    fixture.ui.retry_release(&mut (), record).unwrap();
    assert_eq!(fixture.flags.revocations.get(), 1);
    fixture.ui.retry_release(&mut (), second_record).unwrap();
    assert!(fixture.ui.pending_releases().is_empty());
}

#[test]
fn force_cleanup_bypasses_confirmation_and_records_cause() {
    let mut fixture = fixture();
    let handle = open_app(&mut fixture, TestArgs::plain("root"));
    fixture.flags.close_confirm.set(true);
    fixture
        .ui
        .send::<TestController, TestView>(&fixture.entry, &handle, TestEvent::Draft("keep"))
        .unwrap();
    fixture.ui.drive(&mut ());
    fixture.ui.focus(&handle).unwrap();

    // 强制清理绕过草稿确认,标明宿主退出等原因,清除焦点。
    fixture
        .ui
        .force_release(&mut (), &handle, ReleaseCause::HostExit)
        .unwrap();
    assert!(!fixture.ui.contains(&handle));
    assert_eq!(fixture.ui.focused(), None);

    // 释放失败的强制清理保留记录,并记录原因。
    fixture.flags.fail_release.set(true);
    let second = open_app(&mut fixture, TestArgs::plain("second"));
    let error = fixture
        .ui
        .force_release(&mut (), &second, ReleaseCause::IdentityRemoved)
        .unwrap_err();
    let UiError::ReleaseIncomplete { record, .. } = error else {
        panic!("expected ReleaseIncomplete");
    };
    let pending = &fixture.ui.pending_releases()[0];
    assert_eq!(pending.id, record);
    assert_eq!(pending.cause, Some(ReleaseCause::IdentityRemoved));
    fixture.flags.fail_release.set(false);
    fixture.ui.retry_release(&mut (), record).unwrap();

    // 作用域清理只影响该作用域,逐项报告成功数。
    let mut scoped = fixture_with(config(8, 8, 8, 8, 8, 8), ScopeLevel::Pet);
    let pet_a = ScopeIdentity::new("pet-a").unwrap();
    let pet_b = ScopeIdentity::new("pet-b").unwrap();
    let in_scope = open_pet(&mut scoped, &pet_a, "a");
    open_pet(&mut scoped, &pet_b, "b");
    let report = scoped.ui.force_release_scope(
        &mut (),
        &PanelScope::Pet(pet_a),
        ReleaseCause::IdentityRemoved,
    );
    assert_eq!(report.released(), 1);
    assert!(report.failed().is_empty());
    assert!(!scoped.ui.contains(&in_scope));
    assert_eq!(scoped.ui.list().len(), 1);
}

#[test]
fn force_release_scope_reports_each_failure_and_continues() {
    let mut scoped = fixture_with(config(8, 8, 8, 8, 8, 8), ScopeLevel::Pet);
    let pet_a = ScopeIdentity::new("pet-a").unwrap();
    let pet_b = ScopeIdentity::new("pet-b").unwrap();
    let in_scope_a = open_pet(&mut scoped, &pet_a, "a");
    let in_scope_b = open_pet(&mut scoped, &pet_b, "b");
    scoped.flags.fail_release.set(true);

    // 两个实例都失败,但逐项继续处理,不吞错误也不中断。
    let report = scoped.ui.force_release_scope(
        &mut (),
        &PanelScope::Pet(pet_a.clone()),
        ReleaseCause::HostExit,
    );
    assert_eq!(report.released(), 0);
    assert_eq!(report.failed().len(), 1);
    assert_eq!(report.failed()[0].handle, in_scope_a);
    assert!(matches!(
        report.failed()[0].error,
        UiError::ReleaseIncomplete { .. }
    ));
    // 失败实例转入记录,不再出现在存活列表;其他实例不受影响。
    assert_eq!(scoped.ui.list().len(), 1);
    assert!(!scoped.ui.contains(&in_scope_a));
    assert!(scoped.ui.contains(&in_scope_b));

    // 全部失败实例的记录可逐个重试归还额度。
    let records: Vec<_> = scoped
        .ui
        .force_release_scope(&mut (), &PanelScope::Pet(pet_b), ReleaseCause::HostExit)
        .failed()
        .iter()
        .map(|fault| match fault.error {
            UiError::ReleaseIncomplete { record, .. } => record,
            _ => unreachable!("verified ReleaseIncomplete above"),
        })
        .collect();
    assert_eq!(records.len(), 1);
    scoped.flags.fail_release.set(false);
    let pending: Vec<_> = scoped.ui.pending_releases().iter().map(|p| p.id).collect();
    assert_eq!(pending.len(), 2);
    for record in pending {
        scoped.ui.retry_release(&mut (), record).unwrap();
    }
    assert!(scoped.ui.pending_releases().is_empty());
    assert!(scoped.ui.list().is_empty());
}

// ---- 队列背压 ----

#[test]
fn input_queue_backpressure_returns_payload() {
    let mut fixture = fixture_with(config(8, 8, 1, 8, 8, 8), ScopeLevel::Pet);
    let pet_a = ScopeIdentity::new("pet-a").unwrap();
    let pet_b = ScopeIdentity::new("pet-b").unwrap();
    let handle = open_pet(&mut fixture, &pet_a, "root");
    fixture
        .ui
        .send::<TestController, TestView>(&fixture.entry, &handle, TestEvent::Add(1))
        .unwrap();
    let rejection = fixture
        .ui
        .send::<TestController, TestView>(&fixture.entry, &handle, TestEvent::Add(2))
        .unwrap_err();
    assert_eq!(rejection.reason, RejectionReason::QueueFull);
    assert_eq!(rejection.into_event(), TestEvent::Add(2));

    // 另一作用域的视图采集撞上自己的队满:计入背压报告,不静默丢弃。
    let emitter = {
        let mut args =
            TestArgs::with_emits("emitter", vec![vec![TestEvent::Add(3), TestEvent::Add(4)]]);
        args.emits = vec![
            vec![TestEvent::Add(3), TestEvent::Add(4)],
            vec![TestEvent::Add(6)],
        ];
        match fixture
            .ui
            .open(
                &mut (),
                &fixture.entry,
                PanelScope::Pet(pet_b.clone()),
                Some(&args),
            )
            .unwrap()
        {
            OpenOutcome::Created(handle) => handle,
            outcome => panic!("expected Created, got {outcome:?}"),
        }
    };
    fixture
        .ui
        .send::<TestController, TestView>(&fixture.entry, &emitter, TestEvent::Add(5))
        .unwrap();
    let report = fixture.ui.drive(&mut ());
    assert_eq!(report.inputs_backpressure(), 1);
    // 已入队的负载全部交付;新展示模型最迟下一轮呈现。
    assert!(log_contains(&fixture, "counter=1"));
    fixture.ui.drive(&mut ());
    assert!(log_contains(&fixture, "counter=8"));
}

#[test]
fn navigation_budget_backpressure_and_refusal_report() {
    let mut fixture = fixture_with(config(8, 8, 8, 2, 8, 1), ScopeLevel::Application);
    let handle = open_app(&mut fixture, TestArgs::plain("root"));

    // 一轮内两条导航入队,预算 1 只执行一条,剩余保留。
    fixture
        .ui
        .send::<TestController, TestView>(
            &fixture.entry,
            &handle,
            TestEvent::EmitNav(TestNav::HideSelf),
        )
        .unwrap();
    fixture
        .ui
        .send::<TestController, TestView>(
            &fixture.entry,
            &handle,
            TestEvent::EmitNav(TestNav::HideSelf),
        )
        .unwrap();
    let report = fixture.ui.drive(&mut ());
    assert_eq!(report.navigations_executed(), 1);
    assert_eq!(report.navigations_pending(), 1);
    assert_eq!(
        fixture.ui.get(&handle).unwrap().visibility,
        Visibility::Hidden
    );

    // 队列已占用一条;本轮再入两条,第二条经 Err 交还控制器保留,
    // 报告只累计拒绝次数。
    fixture.ui.show(&mut (), &handle).unwrap();
    fixture
        .ui
        .send::<TestController, TestView>(
            &fixture.entry,
            &handle,
            TestEvent::EmitNav(TestNav::HideSelf),
        )
        .unwrap();
    fixture
        .ui
        .send::<TestController, TestView>(
            &fixture.entry,
            &handle,
            TestEvent::EmitNav(TestNav::HideSelf),
        )
        .unwrap();
    let report = fixture.ui.drive(&mut ());
    assert_eq!(report.navigations_refused(), 1);
    assert_eq!(
        fixture.flags.refused_nav.borrow().last(),
        Some(&Navigate::Hide(handle))
    );

    // requeue_navigation 受导航队列容量约束;队满时把原命令交还。
    fixture
        .ui
        .requeue_navigation(Navigate::Focus(handle))
        .unwrap();
    let error = fixture
        .ui
        .requeue_navigation(Navigate::Focus(handle))
        .unwrap_err();
    assert!(matches!(
        error,
        UiError::NavigationQueueFull(Navigate::Focus(_))
    ));

    // 每轮按预算消费队列;对隐藏面板的 Focus 不暗中显示,计入导航故障。
    let report = fixture.ui.drive(&mut ());
    assert_eq!(report.navigations_pending(), 1);
    let report = fixture.ui.drive(&mut ());
    assert_eq!(report.navigations_pending(), 0);
    assert_eq!(report.navigation_faults().len(), 1);
    assert_eq!(
        fixture.ui.get(&handle).unwrap().visibility,
        Visibility::Hidden
    );
}

#[test]
fn revise_returns_refused_commands_to_controller() {
    // 导航队列只有一个空位且已被占用;revise 的两条命令都被拒,
    // 原命令经 Err 交还控制器,意图不丢,由调用者保留后重交。
    let mut fixture = fixture_with(config(8, 8, 8, 1, 8, 8), ScopeLevel::Application);
    let handle = open_app(&mut fixture, TestArgs::plain("root"));
    fixture
        .ui
        .requeue_navigation(Navigate::Focus(handle))
        .unwrap();
    assert_eq!(fixture.ui.focused(), None);

    let mut args = TestArgs::plain("revise");
    args.revise_navs = vec![TestNav::HideSelf, TestNav::CloseSelf];
    let reused = fixture
        .ui
        .open(
            &mut (),
            &fixture.entry,
            PanelScope::Application,
            Some(&args),
        )
        .unwrap();
    assert_eq!(reused, OpenOutcome::Reused(handle));
    assert_eq!(
        *fixture.flags.refused_nav.borrow(),
        vec![Navigate::Hide(handle), Navigate::Close(handle)]
    );

    // 消费队列后,控制器保留的命令可逐条重交并生效。
    fixture.ui.drive(&mut ());
    assert_eq!(fixture.ui.focused(), Some(handle));
    fixture
        .ui
        .requeue_navigation(Navigate::Hide(handle))
        .unwrap();
    fixture.ui.drive(&mut ());
    assert_eq!(
        fixture.ui.get(&handle).unwrap().visibility,
        Visibility::Hidden
    );
    fixture
        .ui
        .requeue_navigation(Navigate::Close(handle))
        .unwrap();
    fixture.ui.drive(&mut ());
    assert!(!fixture.ui.contains(&handle));
}

#[test]
fn navigation_close_with_confirmation_via_report() {
    let mut fixture = fixture();
    let handle = open_app(&mut fixture, TestArgs::plain("root"));
    fixture.flags.close_confirm.set(true);
    fixture
        .ui
        .send::<TestController, TestView>(&fixture.entry, &handle, TestEvent::Draft("draft"))
        .unwrap();
    fixture
        .ui
        .send::<TestController, TestView>(
            &fixture.entry,
            &handle,
            TestEvent::EmitNav(TestNav::CloseSelf),
        )
        .unwrap();
    let report = fixture.ui.drive(&mut ());
    assert_eq!(report.confirmations().len(), 1);
    assert!(fixture.ui.contains(&handle));
    fixture
        .ui
        .confirm_close(&mut (), &report.confirmations()[0])
        .unwrap();
    assert!(!fixture.ui.contains(&handle));
}

// ---- 每轮事件预算 ----

#[test]
fn event_budget_limits_each_round_without_draining() {
    let mut fixture = fixture_with(config(8, 8, 8, 8, 1, 8), ScopeLevel::Application);
    let handle = open_app(&mut fixture, TestArgs::plain("root"));
    for value in [1, 10, 100] {
        fixture
            .ui
            .send::<TestController, TestView>(&fixture.entry, &handle, TestEvent::Add(value))
            .unwrap();
    }

    // 每轮只处理预算内事件,不循环驱动到队空。
    let report = fixture.ui.drive(&mut ());
    assert_eq!(report.inputs_delivered(), 1);
    assert_eq!(report.inputs_pending(), 2);
    assert!(log_contains(&fixture, "counter=1"));
    assert!(!log_contains(&fixture, "counter=11"));

    let report = fixture.ui.drive(&mut ());
    assert_eq!(report.inputs_delivered(), 1);
    assert_eq!(report.inputs_pending(), 1);
    assert!(log_contains(&fixture, "counter=11"));

    let report = fixture.ui.drive(&mut ());
    assert_eq!(report.inputs_delivered(), 1);
    assert_eq!(report.inputs_pending(), 0);
    assert!(log_contains(&fixture, "counter=111"));
}

#[test]
fn event_budget_rotates_fairly_across_registrations() {
    let mut fixture = fixture_with(config(8, 8, 8, 8, 1, 8), ScopeLevel::Application);
    let second_entry = register_second_kind(&mut fixture, "other", ScopeLevel::Application);
    let mut persistent_failure = TestArgs::plain("persistent-failure");
    persistent_failure.repeat_emit = Some(TestEvent::Fail("repeat"));
    let first = fixture
        .ui
        .open(
            &mut (),
            &fixture.entry,
            PanelScope::Application,
            Some(&persistent_failure),
        )
        .unwrap()
        .handle();
    let second_args = TestArgs::with_emits("second", vec![vec![TestEvent::Add(5)]]);
    let second = fixture
        .ui
        .open(
            &mut (),
            &second_entry,
            PanelScope::Application,
            Some(&second_args),
        )
        .unwrap()
        .handle();

    let first_round = fixture.ui.drive(&mut ());
    assert_eq!(first_round.inputs_delivered(), 0);
    assert_eq!(first_round.faults().len(), 1);
    let next_round = fixture.ui.drive(&mut ());
    assert_eq!(next_round.inputs_delivered(), 1);
    assert!(log_contains(&fixture, "key=other@application"));
    assert!(log_contains(&fixture, "counter=5 key=other@application"));
    assert!(fixture.ui.contains(&first));
    assert!(fixture.ui.contains(&second));
}

#[test]
fn event_budget_visits_each_target_once_per_round() {
    let mut fixture = fixture_with(config(8, 8, 8, 8, 2, 8), ScopeLevel::Application);
    let second_entry = register_second_kind(&mut fixture, "second", ScopeLevel::Application);
    let third_entry = register_second_kind(&mut fixture, "third", ScopeLevel::Application);
    let first = fixture
        .ui
        .open(
            &mut (),
            &fixture.entry,
            PanelScope::Application,
            Some(&TestArgs::with_emits(
                "first",
                vec![vec![TestEvent::Fail("first")]],
            )),
        )
        .unwrap()
        .handle();
    let second = fixture
        .ui
        .open(
            &mut (),
            &second_entry,
            PanelScope::Application,
            Some(&TestArgs::with_emits(
                "second",
                vec![vec![TestEvent::Fail("second")]],
            )),
        )
        .unwrap()
        .handle();
    let third = fixture
        .ui
        .open(
            &mut (),
            &third_entry,
            PanelScope::Application,
            Some(&TestArgs::with_emits(
                "third",
                vec![vec![TestEvent::Fail("third")]],
            )),
        )
        .unwrap()
        .handle();

    let report = fixture.ui.drive(&mut ());
    assert_eq!(report.faults().len(), 2);
    assert_eq!(report.faults()[0].handle, first);
    assert_eq!(report.faults()[1].handle, second);
    assert_eq!(report.inputs_pending(), 1);
    assert_eq!(fixture.ui.drive(&mut ()).faults()[0].handle, third);
}

#[test]
fn event_budget_rotates_fairly_across_instances_in_one_registration() {
    let mut fixture = fixture_with(config(8, 8, 8, 8, 1, 8), ScopeLevel::Pet);
    let pet_a = ScopeIdentity::new("pet-a").unwrap();
    let pet_b = ScopeIdentity::new("pet-b").unwrap();
    let mut persistent_failure = TestArgs::plain("persistent-failure");
    persistent_failure.repeat_emit = Some(TestEvent::Fail("repeat"));
    let first = fixture
        .ui
        .open(
            &mut (),
            &fixture.entry,
            PanelScope::Pet(pet_a.clone()),
            Some(&persistent_failure),
        )
        .unwrap()
        .handle();
    let second_args = TestArgs::with_emits("second", vec![vec![TestEvent::Add(7)]]);
    let second = fixture
        .ui
        .open(
            &mut (),
            &fixture.entry,
            PanelScope::Pet(pet_b),
            Some(&second_args),
        )
        .unwrap()
        .handle();

    assert_eq!(fixture.ui.drive(&mut ()).faults().len(), 1);
    let next_round = fixture.ui.drive(&mut ());
    assert_eq!(next_round.inputs_delivered(), 1);
    assert!(log_contains(&fixture, "counter=7 key=probe@pet:pet-b"));
    assert!(fixture.ui.contains(&first));
    assert!(fixture.ui.contains(&second));
}

// ---- 驱动顺序:先渲染后应用 ----

#[test]
fn drive_renders_all_visible_panels_before_applying_new_events() {
    let mut fixture = fixture_with(config(8, 8, 8, 8, 8, 8), ScopeLevel::Pet);
    let pet_a = ScopeIdentity::new("pet-a").unwrap();
    let pet_b = ScopeIdentity::new("pet-b").unwrap();
    let _a = {
        let args = TestArgs::with_emits("a", vec![vec![TestEvent::Add(1)]]);
        match fixture
            .ui
            .open(&mut (), &fixture.entry, PanelScope::Pet(pet_a), Some(&args))
            .unwrap()
        {
            OpenOutcome::Created(handle) => handle,
            outcome => panic!("expected Created, got {outcome:?}"),
        }
    };
    let _b = {
        let args = TestArgs::with_emits("b", vec![vec![TestEvent::Add(2)]]);
        match fixture
            .ui
            .open(&mut (), &fixture.entry, PanelScope::Pet(pet_b), Some(&args))
            .unwrap()
        {
            OpenOutcome::Created(handle) => handle,
            outcome => panic!("expected Created, got {outcome:?}"),
        }
    };

    // 一轮内:两个面板都完成渲染与采集后,才处理新采集事件。
    let report = fixture.ui.drive(&mut ());
    assert_eq!(report.renders_attempted(), 2);
    assert_eq!(report.inputs_delivered(), 2);
    let log = fixture.log.borrow();
    let first_apply = log
        .iter()
        .position(|line| line.starts_with("apply "))
        .expect("collected inputs applied");
    let renders_before_first_apply = log
        .iter()
        .take(first_apply)
        .filter(|line| line.contains("draft="))
        .count();
    assert_eq!(
        renders_before_first_apply, 2,
        "both panels must render before any collected event is applied"
    );
}

// ---- 单面板错误与错误链 ----

#[test]
fn single_panel_fault_preserves_stage_and_source_chain() {
    let mut fixture = fixture_with(config(8, 8, 8, 8, 8, 8), ScopeLevel::Pet);
    let pet_a = ScopeIdentity::new("pet-a").unwrap();
    let pet_b = ScopeIdentity::new("pet-b").unwrap();
    let broken = {
        let mut args = TestArgs::plain("broken");
        args.render_failures = 1;
        match fixture
            .ui
            .open(
                &mut (),
                &fixture.entry,
                PanelScope::Pet(pet_a.clone()),
                Some(&args),
            )
            .unwrap()
        {
            OpenOutcome::Created(handle) => handle,
            outcome => panic!("expected Created, got {outcome:?}"),
        }
    };
    let healthy = open_pet(&mut fixture, &pet_b, "healthy");
    fixture
        .ui
        .send::<TestController, TestView>(&fixture.entry, &healthy, TestEvent::Add(7))
        .unwrap();

    // 单面板渲染失败记录阶段与 source 链,不阻断其他面板。
    let report = fixture.ui.drive(&mut ());
    assert_eq!(report.faults().len(), 1);
    let fault = &report.faults()[0];
    assert_eq!(fault.handle, broken);
    assert_eq!(fault.stage, Stage::Render);
    let source = fault.source_error().source().expect("chained error");
    assert_eq!(source.to_string(), "root cause: root-detail");

    // 健康面板照常交付与呈现。
    assert!(log_contains(&fixture, "counter=7"));
    assert_eq!(
        fixture.ui.get(&broken).unwrap().visibility,
        Visibility::Visible
    );

    // 事件处理失败同样保留阶段,面板继续可用。
    fixture
        .ui
        .send::<TestController, TestView>(&fixture.entry, &broken, TestEvent::FailChained("apply"))
        .unwrap();
    let report = fixture.ui.drive(&mut ());
    assert_eq!(report.faults().len(), 1);
    assert_eq!(report.faults()[0].stage, Stage::ApplyEvent);
    assert!(report.faults()[0].source_error().source().is_some());
    // 无内因的错误保留阶段但 source 链为空。
    fixture
        .ui
        .send::<TestController, TestView>(&fixture.entry, &broken, TestEvent::Fail("apply-plain"))
        .unwrap();
    let report = fixture.ui.drive(&mut ());
    assert_eq!(report.faults().len(), 1);
    assert_eq!(report.faults()[0].stage, Stage::ApplyEvent);
    assert!(report.faults()[0].source_error().source().is_none());
    fixture
        .ui
        .send::<TestController, TestView>(&fixture.entry, &broken, TestEvent::Add(1))
        .unwrap();
    let report = fixture.ui.drive(&mut ());
    assert!(report.faults().is_empty());
    assert!(log_contains(&fixture, "counter=1"));
}

// ---- API 形状 ----

#[test]
fn rejected_submit_debug_hides_event_payload() {
    let rejection: moe_ui_core::RejectedSubmit<TestEvent> = moe_ui_core::RejectedSubmit {
        reason: RejectionReason::QueueFull,
        event: TestEvent::Draft("secret-body"),
    };
    let rendered = format!("{rejection:?}");
    assert!(rendered.contains("QueueFull"));
    assert!(!rendered.contains("secret-body"));
}
