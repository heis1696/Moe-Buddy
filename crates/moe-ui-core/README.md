# moe-ui-core

UI 管理与呈现框架的 U1 内核:标准库实现的纯 Rust 调用中心、控制器/视图 trait、类型化面板组合与有界生命周期驱动。零第三方依赖,不接文件、网络、系统时钟、线程或异步运行时;不选择 GUI,也不把无头内核记为真实界面完成。冻结契约见 [U1 阶段](../../docs/AGENT-MEMORY-PLAN.md#u1-冻结契约)。

`UiManager<Context>` 由单宿主持有并显式注入访问入口,不使用全局静态单例或服务定位器;中心身份由进程内原子分配的独占序号保证,不依赖系统时钟或调用方提供的整数。它管理的是逻辑面板,不是操作系统窗口管理器:一个逻辑面板可由宿主呈现在页签、区域或独立窗口,由适配层决定。

## 快速上手

```rust
use std::cell::RefCell;
use std::fmt;
use std::rc::Rc;

use moe_ui_core::{
    CloseDecision, CloseRequest, EventSink, NavSink, OpenOutcome, PanelEntry, PanelKind,
    PanelScope, ScopeLevel, UiConfig, UiController, UiManager, UiPanel, UiView,
};

#[derive(Default)]
struct NoteModel { draft: String, saved: Vec<String> }

#[derive(Debug)]
enum NoteEvent { Type(char), Submit }

struct NoteArgs { title: String }

#[derive(Debug)]
struct NoteError;

impl fmt::Display for NoteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("note panel error")
    }
}

impl std::error::Error for NoteError {}

struct NoteController { model: NoteModel }

impl UiController for NoteController {
    type OpenArgs = NoteArgs;
    type Model = NoteModel;
    type Event = NoteEvent;
    type Error = NoteError;

    fn model(&self) -> &NoteModel { &self.model }
    fn apply(&mut self, event: NoteEvent, _nav: &mut NavSink<'_>) -> Result<(), NoteError> {
        match event {
            NoteEvent::Type(ch) => self.model.draft.push(ch),
            NoteEvent::Submit => {
                let draft = std::mem::take(&mut self.model.draft);
                self.model.saved.push(draft);
            }
        }
        Ok(())
    }
    fn revise(&mut self, _args: &NoteArgs, _nav: &mut NavSink<'_>) -> Result<(), NoteError> { Ok(()) }
    fn refresh(&mut self) -> Result<(), NoteError> { Ok(()) }
    fn suspend(&mut self) -> Result<(), NoteError> { Ok(()) }
    fn close_request(&mut self) -> CloseDecision {
        if self.model.draft.is_empty() { CloseDecision::Allow } else { CloseDecision::RequireConfirmation }
    }
    fn revoke(&mut self) { self.model.draft.clear(); }
    fn release(&mut self) -> Result<(), NoteError> { Ok(()) }
}

struct NoteView { seen: Rc<RefCell<Vec<String>>> }

impl UiView for NoteView {
    type Model = NoteModel;
    type Event = NoteEvent;
    type Context = ();
    type Error = NoteError;

    fn mount(&mut self, _context: &mut ()) -> Result<(), NoteError> { Ok(()) }
    fn render(
        &mut self, model: &NoteModel, _context: &mut (),
        _events: &mut EventSink<'_, NoteEvent>,
    ) -> Result<(), NoteError> {
        self.seen.borrow_mut().push(model.draft.clone());
        Ok(())
    }
    fn unmount(&mut self, _context: &mut ()) -> Result<(), NoteError> { Ok(()) }
}

# fn main() -> Result<(), moe_ui_core::UiError> {
let seen = Rc::new(RefCell::new(Vec::new()));
let mut context = ();
let mut ui = UiManager::new(UiConfig::new(8, 8, 8, 8, 4, 4)?)?;
let factory_seen = seen.clone();
let notes: PanelEntry<NoteController, NoteView> = ui.register(
    PanelKind::new("notes")?,
    ScopeLevel::Application,
    move |_key, _args: &NoteArgs| {
        Ok(UiPanel::new(
            NoteController { model: NoteModel::default() },
            NoteView { seen: factory_seen.clone() },
        ))
    },
)?;
let panel = match ui.open(&mut context, &notes, PanelScope::Application,
    Some(&NoteArgs { title: String::from("note") }))?
{
    OpenOutcome::Created(handle) => handle,
    _ => panic!("first open creates an instance"),
};
notes.send(&mut ui, &panel, NoteEvent::Type('h')).unwrap();
notes.send(&mut ui, &panel, NoteEvent::Type('i')).unwrap();
let report = ui.drive(&mut context);
assert_eq!(report.inputs_delivered(), 2);
assert_eq!(seen.borrow().last().map(String::as_str), Some("hi"));
// 存在未保存草稿时,关闭返回确认凭据;确认前实例继续存活。
assert!(matches!(
    ui.close(&mut context, &panel)?,
    CloseRequest::ConfirmationPending(_)
));
assert!(ui.contains(&panel));
#     Ok(())
# }
```

## API 地图

| 类型 | 职责与关键约定 |
| --- | --- |
| `UiManager<Context>` | 调用中心:`register/open/show/hide/focus/close/confirm_close/force_release/force_release_scope/retry_release/pending_releases/get/list/contains/focused/begin_load/send/submit/drive/requeue_navigation`。单宿主持有,不可克隆;跨中心句柄与错误入口均拒绝,不退回按 key 路由。 |
| `UiConfig` | 宿主显式提供的非零注册容量、同时存活实例容量、实例输入/导航队列容量与每轮事件/导航预算;未成功释放的资源计入容量,释放完成即归还。 |
| `UiController` | 控制器 trait,关联 `OpenArgs/Model/Event/Error`;`apply` 处理事件、`revise` 处理新入参、`refresh`/`suspend` 是显示/隐藏核验边界、`close_request` 裁决草稿确认、`revoke` 是撤权/强制释放的敏感缓存清除边界、`release` 释放控制器。 |
| `UiView` | 视图 trait,关联 `Model/Event/Context/Error`;`mount`/`render`/`unmount` 管理后端视图资源,视图只借用展示模型。 |
| `UiPanel<C, V>` | 控制器+视图组合;`Model/Event` 不一致无法编译。视图不获得控制器内部状态的可变引用。 |
| `PanelEntry<C, V>` | `register` 返回的类型化注册入口,绑定 `OpenArgs/Event`;工厂接收中心构造的不可变 `&PanelKey` 与入参,可据真实作用域绑定控制器;重复种类注册被拒绝。 |
| `PanelKind/PanelScope/ScopeIdentity/PanelKey` | 种类加作用域构成键;应用级与稳定桌宠身份区分,同键首版至多一个实例;身份仅定位,不授予权限。 |
| `PanelHandle` | 中心签发的实例句柄;关闭重开产生新句柄,旧句柄不得命中新实例。 |
| `Navigate/NavSink/EventSink` | 受控导航命令与有界事件出口;回调只提交命令,拿不到中心引用。导航队满时 `NavSink` 把原命令经 `Err` 交还回调保留重交,报告只累计拒绝次数。 |
| `LoadTicket` | 加载票据,绑定中心、实例与最新加载序号;新加载、可见性改变、关闭/释放使旧票失效,入队与分发前均复验。 |
| `CloseConfirmation` | 关闭确认凭据,绑定实例与交互版本;任何可能改变控制器的回调(含返回 `Err` 的 `apply`/`revise`/`close_request` 与 `refresh`/`suspend`)都在调用前使旧确认失效,不丢弃新草稿。实例仍有已接纳未处理输入时,普通 `close`/`confirm_close` 明确拒绝,不静默丢弃。 |
| `DriveReport/PanelFault/NavigateFault/ScopeReleaseReport` | 一轮驱动的有界报告:交付/积压/背压/导航拒绝计数与失败阶段、原始 `Error::source` 链;不含业务正文。作用域强制释放返回逐项有界结果,失败项保留错误链与重试编号。 |
| `UiError/Stage/ReleaseCause/PendingReleaseId/PendingReleaseSummary/AggregateError` | 统一错误、失败阶段、强制清理原因与可重试释放记录摘要;记录编号由中心绑定实例序号签发,外部不能伪造,摘要不含原始错误正文。`AggregateError` 提供按原类型检查的首错/次错访问器,保留各自的 `Error::source` 链。 |

## 生命周期与确认

首版只有 `Visible`/`Hidden` 两种存活可见性;未创建/已销毁由实例不存在表达。首次 `open`:工厂收到中心构造的实际 `PanelKey` 与入参,控制器可据键绑定归属;随后核验、挂载并登记。挂载失败仍会卸载部分创建的资源;卸载失败即使控制器释放成功也保留可枚举、可重试记录和容量,重试不重复成功步骤,并同时保留初始与清理错误。恢复挂载后的卸载失败及隐藏卸载失败会将实例隔离至待释放记录,句柄失效且不再渲染、聚焦或接收事件。`show`/`hide` 先使旧关闭确认失效,再走控制器核验/暂停边界;成功隐藏保留草稿与已接纳普通输入,隐藏期间不分发,恢复后继续交付。重复 `open` 复用或恢复,不重建、不重置、不抢焦点;新入参交给原控制器 `revise`,普通重复打开传 `None`。关闭需要确认时返回 `CloseConfirmation`,由宿主交互后 `confirm_close`;每次可变 `close_request` 调用也会使旧凭据失效。实例仍有已接纳未处理的输入时,普通 `close`/`confirm_close` 明确拒绝,须先驱动消费或由宿主 `force_release` 显式处置。`force_release`/`force_release_scope` 只用于宿主退出、身份删除或上下文撤销,不伪装业务回滚;作用域强制释放也会对先前失败记录执行撤权缓存清除边界,并在卸载前独立清空内核持有的已接纳输入及加载负载,即使视图卸载失败也不跳过,而真实像素的紧急隐藏仍由宿主负责;作用域释放返回逐项有界结果,单个失败保留错误链并继续处理其余实例。释放转入失败记录时立即清除焦点;记录编号复用实例序号并由中心绑定,不会因编号耗尽丢弃待释放资源。首版关闭即释放,需要保留就用 `hide`。

## 事件、驱动与背压

`drive` 在宿主 UI 事件线程串行执行:先交付既有输入/结果并更新模型;随后全部可见面板完成渲染与采集;再在剩余事件预算内统一处理新采集事件;最后在导航预算内执行导航。输入预算在注册与实例间按持久游标轮转,持续失败或补充输入的单个实例不能独占预算。新展示模型最迟下一轮呈现;剩余输入与导航保留至后续轮,不循环驱动到队空。所有队列容量与每轮预算来自 `UiConfig`:实例输入队满时 `send`/`submit` 把原始类型化负载交还调用者,视图采集拒绝计入 `inputs_backpressure`;导航队满时控制器回调经 `Err` 收回原命令并自行保留重交,报告只累计 `navigations_refused`,不重复保存命令正文。加载票据在入队与实际分发前双重复验,新入参 `revise` 调用前也会失效旧票据,隐藏面板不分发展示输入/结果,旧请求不得覆盖新刷新。单面板错误保留阶段与 `Error::source` 链,组合错误可通过公开 `AggregateError` 按原类型检查,不阻断其他面板;不承诺捕获 panic。视图模型的脱敏、焦点指示与非颜色状态区分,以及真实像素的紧急隐藏,由宿主适配层负责。

## 封印

句柄字段私有,外部不能伪造:

```compile_fail
use moe_ui_core::PanelHandle;
fn forge() -> PanelHandle {
    PanelHandle { center: 1, registration: 0, serial: unimplemented!() }
}
```

中心不可克隆,没有全局单例访问点:

```compile_fail
use moe_ui_core::UiManager;
fn duplicate_center(context: &UiManager<()>) -> UiManager<()> {
    context.clone()
}
```

待重试释放记录编号由中心签发并绑定实例序号,没有从原始数还原的入口:

```compile_fail
use moe_ui_core::PendingReleaseId;
fn forge() -> PendingReleaseId {
    PendingReleaseId::from_raw(1).unwrap()
}
```

组合要求控制器与视图的 `Model/Event` 一致:

```compile_fail
use moe_ui_core::{UiController, UiPanel, UiView};

struct Wrong;
impl UiController for Wrong {
    type OpenArgs = ();
    type Model = u8;
    type Event = u8;
    type Error = std::convert::Infallible;
    fn model(&self) -> &u8 { &0 }
    fn apply(&mut self, _event: u8, _nav: &mut moe_ui_core::NavSink<'_>) -> Result<(), std::convert::Infallible> { Ok(()) }
    fn revise(&mut self, _args: &(), _nav: &mut moe_ui_core::NavSink<'_>) -> Result<(), std::convert::Infallible> { Ok(()) }
    fn refresh(&mut self) -> Result<(), std::convert::Infallible> { Ok(()) }
    fn suspend(&mut self) -> Result<(), std::convert::Infallible> { Ok(()) }
    fn close_request(&mut self) -> moe_ui_core::CloseDecision { moe_ui_core::CloseDecision::Allow }
    fn revoke(&mut self) { }
    fn release(&mut self) -> Result<(), std::convert::Infallible> { Ok(()) }
}
impl UiView for Wrong {
    type Model = u16;
    type Event = u8;
    type Context = ();
    type Error = std::convert::Infallible;
    fn mount(&mut self, _context: &mut ()) -> Result<(), std::convert::Infallible> { Ok(()) }
    fn render(&mut self, _model: &u16, _context: &mut (), _events: &mut moe_ui_core::EventSink<'_, u8>) -> Result<(), std::convert::Infallible> { Ok(()) }
    fn unmount(&mut self, _context: &mut ()) -> Result<(), std::convert::Infallible> { Ok(()) }
}
fn build() -> UiPanel<Wrong, Wrong> {
    UiPanel::new(Wrong, Wrong)
}
```
