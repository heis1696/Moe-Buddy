//! Windows 原生宿主：事件循环、控件编排与受信动作分发。
//!
//! 宿主只把窗口输入翻译为 `ChatApplication` 的既有 `Action`，并按快照
//! 渲染有界视图；业务规则、授权与提交边界全部留在应用层。
//!
//! ## 重入安全：状态计算与窗口副作用分离
//!
//! 窗口过程从 `GWLP_USERDATA` 取回 `DesktopState` 裸指针。为避免同步
//! Win32 调用（`SetWindowTextW` 回发 `EN_CHANGE`、`MoveWindow`/`SetWindowPos`
//! 同步重绘、`MessageBoxW` 模态循环）重入窗口过程时出现两个并存的
//! `&mut DesktopState`（别名可变引用即 UB），本模块遵守两条硬性规则：
//!
//! 1. 借用只在单个消息处理的"阶段 A"内存在：完成纯状态计算（视图模型、
//!    布局几何、量测）后立即结束。允许在阶段 A 内调用的 Win32 仅限
//!    不重入本窗口过程的封闭集合——`GetClientRect`、`GetDpiForWindow`、
//!    `GetWindowTextLengthW`/`GetWindowTextW`（由目标控件自身过程应答）、
//!    控件只读查询（`LB_GETCURSEL` 等）以及不分发消息的 GDI 调用
//!    （`CreateFontW`、`GetTextMetricsW`、`DrawTextW(DT_CALCRECT)`）。
//!    `InvalidateRect`/`SetScrollInfo` 只改排队状态，也可在阶段 A 使用。
//! 2. 一切可能同步重入的窗口副作用（`SetWindowTextW`、`SendMessageW`、
//!    `EnableWindow`、`ShowWindow`、`MoveWindow`、`SetWindowPos`、
//!    `MessageBoxW`、`DestroyWindow`、`SetFocus`、`UpdateWindow`）都在
//!    借用结束后的"阶段 B"执行，只消费计划中的拷贝数据（HWND、文本、
//!    矩形），不再触碰状态指针。重入进来的消息各自建立自己的短借用。
//!
//! 生命周期：`WM_NCDESTROY` 先清空 `GWLP_USERDATA`、注销实例代际，再
//! 释放状态 Box，此后任何迟到消息都取不到指针；销毁期间（`DestroyWindow`
//! 同步派发的焦点变化与子窗口销毁）到达的消息仍能看到存活的状态，由
//! 各自短借用安全处理。
//!
//! ## 同步回调返回后的两重复验
//!
//! 允许同步回调的副作用（`SetWindowTextW`、`EnableWindow`、`ShowWindow`、
//! `MoveWindow`、`SendMessageW` 扇出、`WM_SETFONT`、`SetWindowPos`、模态
//! `MessageBoxW`）返回后必须复验，失败立即停止：
//!
//! 1. 实例存活：每个窗口在 `WM_NCCREATE` 时把随机实例代际登记入进程内
//!    注册表，`WM_NCDESTROY` 先注销再释放 Box。复验只比对登记表与栈上
//!    捕获值，绝不解引用可能已释放的指针；窗口销毁、HWND 复用或堆地址
//!    复用都会产生不同代际，不会误判存活（`GWLP_USERDATA` 相等不足以
//!    排除复用）。
//! 2. 计划修订：`DesktopState.revision` 在每次结构性 `Action` 成功后递增
//!    （`SetDraft` 除外：真实键入由队列驱动、不会在阶段 B 内交错，而
//!    阶段 B 内的 `EN_CHANGE` 只可能是本计划 `SetWindowTextW` 的幂等
//!    回显）。阶段 A 产出的 `WindowPlan` 捕获当时修订号；阶段 B 每执行
//!    一步可重入副作用就比对一次，重入方若已完成撤权等结构性变更，
//!    剩余副作用（重新启用控件、回写过期草稿）一律不再执行，界面由
//!    重入方自己的刷新收敛。
//!
//! 只读 Win32 查询同样按回调对待：`GetWindowTextW`/`LB_GETCURSEL` 会进
//! 入控件过程（子类化后可在其中销毁主窗口），查询返回后先复验实例
//! 存活，才允许建立下一段状态借用。

use std::cell::RefCell;
use std::collections::HashMap;
use std::mem::size_of;
use std::ptr::null;
use std::time::Instant;

use getrandom::fill;
use moe_chat_app::{
    Action, AppError, AppSnapshot, ChatApplication, ChatMessage, PanelKind, PanelVisibility,
};
use windows_sys::Win32::Foundation::{
    COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM,
};
use windows_sys::Win32::Graphics::Gdi::{
    CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, CreateFontW, CreateSolidBrush, DEFAULT_CHARSET,
    DEFAULT_PITCH, DeleteObject, FW_BOLD, FW_NORMAL, GetDC, GetTextMetricsW, HBRUSH, HDC, HFONT,
    InvalidateRect, OUT_DEFAULT_PRECIS, ReleaseDC, SelectObject, SetBkColor, SetTextColor,
    TEXTMETRICW, UpdateWindow,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::{DRAWITEMSTRUCT, EM_EMPTYUNDOBUFFER, SetScrollInfo};
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForSystem, GetDpiForWindow,
    SetProcessDpiAwarenessContext,
};
use windows_sys::Win32::UI::Input::Ime::{
    GCS_COMPSTR, ImmGetCompositionStringW, ImmGetContext, ImmReleaseContext,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    EnableWindow, GetKeyState, SetFocus, VK_RETURN, VK_SHIFT,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AdjustWindowRect, BN_CLICKED, BS_OWNERDRAW, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT,
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, EN_CHANGE, EN_KILLFOCUS,
    EN_SETFOCUS, ES_AUTOHSCROLL, ES_AUTOVSCROLL, ES_MULTILINE, ES_WANTRETURN, GWLP_USERDATA,
    GetClientRect, GetMessageW, GetScrollInfo, GetWindowLongPtrW, GetWindowTextLengthW,
    GetWindowTextW, IDYES, IsDialogMessageW, IsWindow, LB_ADDSTRING, LB_GETCURSEL, LB_RESETCONTENT,
    LB_SETCURSEL, LB_SETITEMDATA, LB_SETITEMHEIGHT, LBN_KILLFOCUS, LBN_SELCHANGE, LBN_SETFOCUS,
    LBS_HASSTRINGS, LBS_NOINTEGRALHEIGHT, LBS_NOTIFY, LBS_OWNERDRAWFIXED, MB_DEFBUTTON2,
    MB_ICONWARNING, MB_YESNO, MINMAXINFO, MSG, MessageBoxW, MoveWindow, PostQuitMessage,
    RegisterClassExW, SB_BOTTOM, SB_LINEDOWN, SB_LINEUP, SB_PAGEDOWN, SB_PAGEUP, SB_THUMBPOSITION,
    SB_THUMBTRACK, SB_TOP, SB_VERT, SCROLLINFO, SIF_PAGE, SIF_POS, SIF_RANGE, SIF_TRACKPOS,
    SW_HIDE, SW_SHOW, SW_SHOWNA, SWP_NOACTIVATE, SWP_NOZORDER, SendMessageW, SetWindowLongPtrW,
    SetWindowPos, SetWindowTextW, ShowWindow, TranslateMessage, WM_CLOSE, WM_COMMAND, WM_CREATE,
    WM_CTLCOLOREDIT, WM_CTLCOLORLISTBOX, WM_DRAWITEM, WM_ERASEBKGND, WM_GETMINMAXINFO, WM_KEYDOWN,
    WM_MOUSEWHEEL, WM_NCCREATE, WM_NCDESTROY, WM_PAINT, WM_SETFOCUS, WM_SETFONT, WM_SIZE,
    WM_VSCROLL, WNDCLASSEXW, WS_CHILD, WS_CLIPCHILDREN, WS_EX_CONTROLPARENT, WS_OVERLAPPEDWINDOW,
    WS_TABSTOP, WS_VISIBLE, WS_VSCROLL,
};

use crate::chatlog;
use crate::layout::{self, Rect};
use crate::presenter::{self, Row, Severity, SideTab};
use crate::theme;

mod model_settings;
mod paint;

#[cfg(test)]
mod tests;

const WINDOW_CLASS: &str = "MoeBuddyDesktopWindow";
const CANVAS_CLASS: &str = "MoeBuddyChatCanvas";
const WINDOW_TITLE: &str = "Moe Buddy";
/// `WM_DPICHANGED`；本地定义以避免特性组合差异。
const WM_DPICHANGED_MSG: u32 = 0x02E0;
const WHEEL_DELTA: i32 = 120;
const WHEEL_LINES: i32 = 3;

const ID_CANVAS: i32 = 150;
const ID_INPUT: i32 = 201;
const ID_SEND: i32 = 202;
const ID_TAB_BASE: i32 = 203;
const ID_CHAT_TAB: i32 = 206;
const ID_MEM_LIST: i32 = 210;
const ID_MEM_INPUT: i32 = 211;
const ID_MEM_REMEMBER: i32 = 212;
const ID_MEM_REFRESH: i32 = 213;
const ID_MEM_SEARCH: i32 = 214;
const ID_MEM_EDIT: i32 = 215;
const ID_MEM_DELETE: i32 = 216;
const ID_PRESET_LIST: i32 = 220;
const ID_PRESET_APPLY: i32 = 221;
const ID_LOG_LIST: i32 = 230;
const ID_LOG_REFRESH: i32 = 231;
const ID_REVOKE: i32 = 240;
const TAB_IDS: [i32; 3] = [ID_TAB_BASE, ID_TAB_BASE + 1, ID_TAB_BASE + 2];

/// 撤销授权/关闭确认的呈现接缝：产品实现走 `MessageBoxW` 模态循环，
/// 测试注入脚本化答案以驱动同一套阶段逻辑。
type ConfirmDialog = unsafe fn(window: HWND, title: &str, text: &str) -> i32;

#[derive(Clone, Copy)]
struct Controls {
    chat_tab: HWND,
    model_fields: [HWND; 6],
    input: HWND,
    send: HWND,
    tabs: [HWND; 3],
    mem_list: HWND,
    mem_input: HWND,
    mem_remember: HWND,
    mem_refresh: HWND,
    mem_search: HWND,
    mem_edit: HWND,
    mem_delete: HWND,
    preset_list: HWND,
    preset_apply: HWND,
    log_list: HWND,
    log_refresh: HWND,
    revoke: HWND,
}

impl Controls {
    const fn dangling() -> Self {
        Self {
            chat_tab: std::ptr::null_mut(),
            model_fields: [std::ptr::null_mut(); 6],
            input: std::ptr::null_mut(),
            send: std::ptr::null_mut(),
            tabs: [std::ptr::null_mut(); 3],
            mem_list: std::ptr::null_mut(),
            mem_input: std::ptr::null_mut(),
            mem_remember: std::ptr::null_mut(),
            mem_refresh: std::ptr::null_mut(),
            mem_search: std::ptr::null_mut(),
            mem_edit: std::ptr::null_mut(),
            mem_delete: std::ptr::null_mut(),
            preset_list: std::ptr::null_mut(),
            preset_apply: std::ptr::null_mut(),
            log_list: std::ptr::null_mut(),
            log_refresh: std::ptr::null_mut(),
            revoke: std::ptr::null_mut(),
        }
    }
}

#[derive(Clone, Copy)]
struct Fonts {
    hero: HFONT,
    title: HFONT,
    body: HFONT,
    body_bold: HFONT,
    small: HFONT,
    mono: HFONT,
}

impl Fonts {
    const fn dangling() -> Self {
        Self {
            hero: std::ptr::null_mut(),
            title: std::ptr::null_mut(),
            body: std::ptr::null_mut(),
            body_bold: std::ptr::null_mut(),
            small: std::ptr::null_mut(),
            mono: std::ptr::null_mut(),
        }
    }
}

#[derive(Clone, Copy)]
struct Brushes {
    nav: HBRUSH,
    bg: HBRUSH,
    panel: HBRUSH,
    accent: HBRUSH,
    accent_dark: HBRUSH,
    accent_soft: HBRUSH,
    border: HBRUSH,
    warn_soft: HBRUSH,
    disabled_fill: HBRUSH,
}

impl Brushes {
    const fn dangling() -> Self {
        Self {
            nav: std::ptr::null_mut(),
            bg: std::ptr::null_mut(),
            panel: std::ptr::null_mut(),
            accent: std::ptr::null_mut(),
            accent_dark: std::ptr::null_mut(),
            accent_soft: std::ptr::null_mut(),
            border: std::ptr::null_mut(),
            warn_soft: std::ptr::null_mut(),
            disabled_fill: std::ptr::null_mut(),
        }
    }
}

struct ViewState {
    header: presenter::HeaderModel,
    status: presenter::StatusModel,
    notice: Option<String>,
    memory: presenter::PanelModel,
    presets: presenter::PanelModel,
    logs: presenter::PanelModel,
    active_tab: Option<SideTab>,
    mem_selected: Option<String>,
    editing_memory: Option<(String, u64)>,
    preset_selected: Option<String>,
    mem_detail: String,
    preset_detail: String,
    log_detail: String,
    error: Option<(String, Severity)>,
}

struct CanvasView {
    layout: Option<chatlog::HistoryLayout>,
    messages: Vec<ChatMessage>,
    sig: (usize, i32, usize),
    scroll: i32,
}

struct DesktopState {
    app: ChatApplication,
    model_key: String,
    initial_settings: Option<moe_chat_app::ModelSettings>,
    model_worker: Option<std::sync::mpsc::Receiver<crate::transport::Reply>>,
    clock: Instant,
    epoch_ms: u64,
    /// 本窗口的随机实例代际，登记于 `WINDOW_INSTANCES`；存活复验的锚点。
    instance: u64,
    /// 状态修订号：结构性 `Action` 成功后递增；在途 `WindowPlan` 捕获
    /// 旧值，阶段 B 检查点发现前进即中止。
    revision: u64,
    main: HWND,
    canvas: HWND,
    controls: Controls,
    fonts: Fonts,
    brushes: Brushes,
    metrics: theme::Metrics,
    panels: [layout::PanelLayout; 3],
    dpi: u32,
    body_line: i32,
    small_line: i32,
    layout: Option<layout::ClientLayout>,
    view: ViewState,
    canvas_view: CanvasView,
    list_sigs: [Option<(usize, u64)>; 3],
    focus_ring: i32,
    confirm: ConfirmDialog,
}

impl Drop for DesktopState {
    fn drop(&mut self) {
        // 只做 GDI 句柄释放；这些调用不分发窗口消息，销毁路径无重入。
        unsafe {
            for handle in [
                self.fonts.hero,
                self.fonts.title,
                self.fonts.body,
                self.fonts.body_bold,
                self.fonts.small,
                self.fonts.mono,
            ] {
                if !handle.is_null() {
                    DeleteObject(handle as _);
                }
            }
            for handle in [
                self.brushes.nav,
                self.brushes.bg,
                self.brushes.panel,
                self.brushes.accent,
                self.brushes.accent_dark,
                self.brushes.accent_soft,
                self.brushes.border,
                self.brushes.warn_soft,
                self.brushes.disabled_fill,
            ] {
                if !handle.is_null() {
                    DeleteObject(handle as _);
                }
            }
        }
    }
}

/// 一次刷新（阶段 A）产出的窗口副作用清单。只含拷贝数据，阶段 B
/// 执行时每步可重入副作用后经 `valid()` 复验；实例销毁或状态修订号
/// 前进都立即中止，剩余副作用不再执行。`state` 仅供 `valid()` 在实例
/// 存活确认后读取修订号，不得用于其他访问。
struct WindowPlan {
    state: *mut DesktopState,
    instance: Option<u64>,
    revision: u64,
    controls: Controls,
    canvas: HWND,
    main: HWND,
    input_enabled: bool,
    send_enabled: bool,
    tabs_enabled: [bool; 3],
    revoke_enabled: bool,
    memory_on: bool,
    memory_selected: bool,
    presets_on: bool,
    logs_on: bool,
    list_has_rows: [bool; 3],
    apply_enabled: bool,
    clear_revoked_inputs: bool,
    draft: Option<String>,
    lists: [Option<ListReload>; 3],
    moves: Vec<(HWND, i32, i32, i32, i32)>,
    item_heights: Vec<(HWND, i32)>,
    scroll: Option<(HWND, i32, u32, i32)>,
    invalidate_canvas: bool,
    invalidate_main: bool,
}

impl WindowPlan {
    /// 阶段 B 检查点：先按注册表代际复验实例（不触碰状态内存），实例
    /// 存活才允许读取修订号。失效即中止，剩余副作用交由重入方的刷新
    /// 或后续刷新收敛。
    unsafe fn valid(&self) -> bool {
        instance_valid(self.main, self.instance) && (*self.state).revision == self.revision
    }
}

/// 列表框一次重建：行主文本与目标选中项（-1 表示清除选中）。
struct ListReload {
    control: HWND,
    items: Vec<String>,
    selection: i32,
}

/// 撤销授权/关闭确认的产品实现：真实模态消息框。
unsafe fn modal_confirm(window: HWND, title: &str, text: &str) -> i32 {
    MessageBoxW(
        window,
        wide(text).as_ptr(),
        wide(title).as_ptr(),
        MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2,
    )
}

pub fn run() -> Result<(), String> {
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    let window_instance_id = process_generation()?;
    let process_generation = process_generation()?;
    let epoch_ms = u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| "Invalid system time")?
            .as_millis(),
    )
    .map_err(|_| "Invalid system time")?;
    let mut application = crate::storage::open_application(process_generation, epoch_ms)?;
    application
        .dispatch(Action::Open(PanelKind::Chat), 0)
        .map_err(|error| error.public_message().to_owned())?;

    let instance = unsafe { GetModuleHandleW(null()) };
    if instance.is_null() {
        return Err("unable to obtain the desktop module handle".to_owned());
    }
    let dpi = unsafe { GetDpiForSystem() };
    let snapshot = application.snapshot();
    let state = Box::new(DesktopState {
        app: application,
        model_key: String::new(),
        initial_settings: crate::storage::load_settings()?,
        model_worker: None,
        clock: Instant::now(),
        epoch_ms,
        instance: window_instance_id,
        revision: 0,
        main: std::ptr::null_mut(),
        canvas: std::ptr::null_mut(),
        controls: Controls::dangling(),
        fonts: Fonts::dangling(),
        brushes: Brushes::dangling(),
        metrics: theme::Metrics::new(dpi),
        panels: empty_panel_layouts(),
        dpi,
        body_line: theme::scale(24, dpi),
        small_line: theme::scale(18, dpi),
        layout: None,
        view: ViewState::initial(&snapshot),
        canvas_view: CanvasView {
            layout: None,
            messages: Vec::new(),
            sig: (0, 0, 0),
            scroll: 0,
        },
        list_sigs: [None; 3],
        focus_ring: 0,
        confirm: modal_confirm,
    });
    let state_ptr = Box::into_raw(state);
    let main_window: HWND;

    unsafe {
        if !register_classes(instance) {
            drop(Box::from_raw(state_ptr));
            return Err("unable to register the desktop window classes".to_owned());
        }
        let metrics = (*state_ptr).metrics;
        let (min_w, min_h) = window_size_for(metrics.min_client_w, metrics.min_client_h);
        let (init_w, init_h) = window_size_for(metrics.initial_client_w, metrics.initial_client_h);
        let window = CreateWindowExW(
            WS_EX_CONTROLPARENT,
            wide(WINDOW_CLASS).as_ptr(),
            wide(WINDOW_TITLE).as_ptr(),
            WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            init_w.max(min_w),
            init_h.max(min_h),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            instance,
            state_ptr.cast(),
        );
        if window.is_null() {
            drop(Box::from_raw(state_ptr));
            return Err("unable to create the desktop window".to_owned());
        }
        ShowWindow(window, SW_SHOW);
        UpdateWindow(window);
        let input = (*state_ptr).controls.input;
        SetFocus(input);
        main_window = window;
    }

    let mut message: MSG = unsafe { std::mem::zeroed() };
    loop {
        let result = unsafe { GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) };
        if result == -1 {
            return Err("the desktop message loop failed".to_owned());
        }
        if result == 0 {
            break;
        }
        unsafe {
            if intercept_enter(&message) {
                continue;
            }
            if redirect_wheel(&mut message) {
                continue;
            }
            // 窗口销毁后 Box 已释放；销毁期间仍可能派发迟到消息，
            // 此时跳过对话框导航，只做默认翻译与分发。
            if IsWindow(main_window) != 0 && IsDialogMessageW(main_window, &message) != 0 {
                continue;
            }
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    Ok(())
}

fn process_generation() -> Result<u64, String> {
    let mut bytes = [0_u8; 8];
    fill(&mut bytes).map_err(|_| "unable to create a process generation".to_owned())?;
    let value = u64::from_le_bytes(bytes);
    Ok(if value == 0 { 1 } else { value })
}

/// 裸 Enter 在聊天输入与记忆输入中触发动作；输入法组合或 Shift+Enter
/// 时把按键原样交回控件，保证换行与候选确认不误发。
unsafe fn intercept_enter(message: &MSG) -> bool {
    if message.message != WM_KEYDOWN || message.wParam != VK_RETURN as usize {
        return false;
    }
    let pointer = state_pointer_of(message.hwnd);
    if pointer.is_null() {
        return false;
    }
    let (input, mem_input) = {
        let state = &*pointer;
        (state.controls.input, state.controls.mem_input)
    };
    let is_chat_input = message.hwnd == input;
    let is_memory_input = message.hwnd == mem_input;
    if !is_chat_input && !is_memory_input {
        return false;
    }
    let shift_down = GetKeyState(VK_SHIFT as i32) as u16 & 0x8000 != 0;
    let composing = ime_composing(message.hwnd);
    if !presenter::enter_sends(composing, shift_down) {
        TranslateMessage(message);
        DispatchMessageW(message);
        return true;
    }
    if is_chat_input {
        send_chat(pointer);
    } else {
        remember_memory(pointer);
    }
    true
}

/// 焦点落在输入控件或主窗口时，滚轮重定向到聊天历史画布。
/// 返回 true 表示消息已由画布处理完毕。
unsafe fn redirect_wheel(message: &mut MSG) -> bool {
    if message.message != WM_MOUSEWHEEL {
        return false;
    }
    let pointer = state_pointer_of(message.hwnd);
    if pointer.is_null() {
        return false;
    }
    let target = {
        let state = &*pointer;
        if message.hwnd == state.controls.input
            || message.hwnd == state.controls.mem_input
            || message.hwnd == state.main
        {
            Some(state.canvas)
        } else {
            None
        }
    };
    match target {
        Some(canvas) => {
            SendMessageW(canvas, WM_MOUSEWHEEL, message.wParam, message.lParam);
            true
        }
        None => false,
    }
}

unsafe fn ime_composing(hwnd: HWND) -> bool {
    let context = ImmGetContext(hwnd);
    if context.is_null() {
        return false;
    }
    let length = ImmGetCompositionStringW(context, GCS_COMPSTR, std::ptr::null_mut(), 0);
    ImmReleaseContext(hwnd, context);
    length > 0
}

unsafe fn state_pointer_of(hwnd: HWND) -> *mut DesktopState {
    if hwnd.is_null() {
        return std::ptr::null_mut();
    }
    GetWindowLongPtrW(main_window_of(hwnd), GWLP_USERDATA) as *mut DesktopState
}

unsafe fn main_window_of(hwnd: HWND) -> HWND {
    let mut candidate = hwnd;
    for _ in 0..4 {
        let parent = windows_sys::Win32::UI::WindowsAndMessaging::GetParent(candidate);
        if parent.is_null() {
            return candidate;
        }
        candidate = parent;
    }
    candidate
}

// 窗口实例注册表：`WM_NCCREATE` 登记 (HWND → 实例代际)，`WM_NCDESTROY`
// 先注销再释放状态 Box。UI 单线程，`thread_local` 即可；表生存期独立于
// 状态 Box，窗口销毁后复验仍然可读。
thread_local! {
    static WINDOW_INSTANCES: RefCell<HashMap<isize, u64>> = RefCell::new(HashMap::new());
}

fn register_window_instance(window: HWND, instance: u64) {
    WINDOW_INSTANCES.with(|registry| {
        registry.borrow_mut().insert(window as isize, instance);
    });
}

fn unregister_window_instance(window: HWND) {
    WINDOW_INSTANCES.with(|registry| {
        registry.borrow_mut().remove(&(window as isize));
    });
}

/// 当前登记在案的实例代际；`None` 表示窗口未登记（已销毁或尚未创建）。
fn window_instance(window: HWND) -> Option<u64> {
    WINDOW_INSTANCES.with(|registry| registry.borrow().get(&(window as isize)).copied())
}

/// 同步回调返回后的实例复验：捕获值有效且登记表仍是同一代际。窗口
/// 销毁（注销）、HWND 复用或堆地址复用（新实例新代际）都判失效。
fn instance_valid(window: HWND, captured: Option<u64>) -> bool {
    captured.is_some() && window_instance(window) == captured
}

unsafe fn register_classes(instance: HINSTANCE) -> bool {
    let cursor = windows_sys::Win32::UI::WindowsAndMessaging::LoadCursorW(
        std::ptr::null_mut(),
        windows_sys::Win32::UI::WindowsAndMessaging::IDC_ARROW,
    );
    let main_class_name = wide(WINDOW_CLASS);
    let main_class = WNDCLASSEXW {
        cbSize: size_of::<WNDCLASSEXW>() as u32,
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(main_proc),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: instance,
        hIcon: std::ptr::null_mut(),
        hCursor: cursor,
        hbrBackground: std::ptr::null_mut(),
        lpszMenuName: null(),
        lpszClassName: main_class_name.as_ptr(),
        hIconSm: std::ptr::null_mut(),
    };
    if RegisterClassExW(&main_class) == 0 {
        return false;
    }
    let canvas_class_name = wide(CANVAS_CLASS);
    let canvas_class = WNDCLASSEXW {
        cbSize: size_of::<WNDCLASSEXW>() as u32,
        style: 0,
        lpfnWndProc: Some(canvas_proc),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: instance,
        hIcon: std::ptr::null_mut(),
        hCursor: cursor,
        hbrBackground: std::ptr::null_mut(),
        lpszMenuName: null(),
        lpszClassName: canvas_class_name.as_ptr(),
        hIconSm: std::ptr::null_mut(),
    };
    RegisterClassExW(&canvas_class) != 0
}

unsafe extern "system" fn main_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_NCCREATE {
        let create =
            &*(lparam as *const windows_sys::Win32::UI::WindowsAndMessaging::CREATESTRUCTW);
        SetWindowLongPtrW(window, GWLP_USERDATA, create.lpCreateParams as isize);
        let state = create.lpCreateParams as *mut DesktopState;
        (*state).main = window;
        // 登记实例代际；此后的存活复验以注册表为准，不依赖
        // `GWLP_USERDATA` 与指针值的相等（不足以排除 HWND/地址复用）。
        register_window_instance(window, (*state).instance);
    }
    let pointer = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut DesktopState;
    if pointer.is_null() {
        return DefWindowProcW(window, message, wparam, lparam);
    }
    // 终止消息最先处理：先清空 GWLP_USERDATA、注销实例代际，再释放
    // Box。此后任何迟到/嵌套消息既取不到指针，复验也判失效，只能走
    // 默认过程。
    if message == WM_NCDESTROY {
        SetWindowLongPtrW(window, GWLP_USERDATA, 0);
        unregister_window_instance(window);
        drop(Box::from_raw(pointer));
        PostQuitMessage(0);
        return 0;
    }
    match message {
        WM_CREATE => {
            create_stage(pointer, window);
            0
        }
        windows_sys::Win32::UI::WindowsAndMessaging::WM_TIMER => {
            model_settings::poll(pointer);
            0
        }
        WM_SIZE => {
            if wparam as u32 != windows_sys::Win32::UI::WindowsAndMessaging::SIZE_MINIMIZED {
                refresh(pointer);
            }
            0
        }
        WM_GETMINMAXINFO => {
            let info = &mut *(lparam as *mut MINMAXINFO);
            let (width, height) = {
                let state = &*pointer;
                window_size_for(state.metrics.min_client_w, state.metrics.min_client_h)
            };
            info.ptMinTrackSize = POINT {
                x: width,
                y: height,
            };
            0
        }
        WM_DPICHANGED_MSG => {
            let suggested = *(lparam as *const RECT);
            let dpi = GetDpiForWindow(window);
            apply_dpi_at(pointer, window, dpi, &suggested);
            0
        }
        WM_SETFOCUS => {
            let input = {
                let state = &*pointer;
                if state.view.status.send_enabled {
                    Some(state.controls.input)
                } else {
                    None
                }
            };
            if let Some(input) = input {
                SetFocus(input);
            }
            0
        }
        WM_COMMAND => {
            handle_command(pointer, window, wparam);
            0
        }
        WM_DRAWITEM => {
            let draw = &*(lparam as *const DRAWITEMSTRUCT);
            let state = &*pointer;
            paint::draw_item(state, draw);
            1
        }
        WM_CTLCOLOREDIT
        | WM_CTLCOLORLISTBOX
        | windows_sys::Win32::UI::WindowsAndMessaging::WM_CTLCOLORSTATIC
        | windows_sys::Win32::UI::WindowsAndMessaging::WM_CTLCOLORBTN => {
            let hdc = wparam as HDC;
            let brush = {
                let state = &*pointer;
                state.brushes.panel
            };
            SetTextColor(hdc, theme::INK);
            SetBkColor(hdc, theme::PANEL);
            brush as isize
        }
        WM_PAINT => {
            let state = &*pointer;
            paint::paint_main(state, window);
            0
        }
        WM_ERASEBKGND => 1,
        WM_CLOSE => {
            handle_window_close(pointer, window);
            0
        }
        _ => DefWindowProcW(window, message, wparam, lparam),
    }
}

unsafe extern "system" fn canvas_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message != WM_PAINT
        && message != WM_ERASEBKGND
        && message != WM_VSCROLL
        && message != WM_MOUSEWHEEL
    {
        return DefWindowProcW(window, message, wparam, lparam);
    }
    let parent = windows_sys::Win32::UI::WindowsAndMessaging::GetParent(window);
    let pointer = GetWindowLongPtrW(parent, GWLP_USERDATA) as *mut DesktopState;
    if pointer.is_null() {
        return DefWindowProcW(window, message, wparam, lparam);
    }
    match message {
        WM_PAINT => {
            let state = &*pointer;
            paint::paint_canvas(state, window);
            0
        }
        WM_ERASEBKGND => 1,
        WM_VSCROLL | WM_MOUSEWHEEL => {
            let state = &mut *pointer;
            scroll_canvas(state, message, wparam);
            0
        }
        _ => DefWindowProcW(window, message, wparam, lparam),
    }
}

/// 画布滚动：只改动状态里的滚动值并同步滚动条。所用 Win32 调用
/// （`GetClientRect`/`GetScrollInfo`/`SetScrollInfo`/`InvalidateRect`）
/// 均不分发窗口消息，借用期间不会重入。
unsafe fn scroll_canvas(state: &mut DesktopState, message: u32, wparam: WPARAM) {
    let Some(history) = state.canvas_view.layout.as_ref() else {
        return;
    };
    let content = history.content_height;
    let mut rect = RECT::default();
    GetClientRect(state.canvas, &mut rect);
    let viewport = rect.bottom - rect.top;
    let line = state.body_line.max(1);
    let current = state.canvas_view.scroll;
    let next = if message == WM_MOUSEWHEEL {
        let delta = ((wparam >> 16) & 0xFFFF) as u16 as i16 as i32;
        current - delta / WHEEL_DELTA * WHEEL_LINES * line
    } else {
        match (wparam & 0xFFFF) as i32 {
            SB_LINEUP => current - line,
            SB_LINEDOWN => current + line,
            SB_PAGEUP => current - viewport,
            SB_PAGEDOWN => current + viewport,
            SB_THUMBPOSITION | SB_THUMBTRACK => {
                // 拖动位置是 32 位值，必须走 `GetScrollInfo(SIF_TRACKPOS)`；
                // `wParam` 高 16 位会在超过 65535px 时截断。
                let mut info = SCROLLINFO {
                    cbSize: size_of::<SCROLLINFO>() as u32,
                    fMask: SIF_TRACKPOS,
                    ..SCROLLINFO::default()
                };
                GetScrollInfo(state.canvas, SB_VERT, &mut info);
                info.nTrackPos
            }
            SB_TOP => 0,
            SB_BOTTOM => i32::MAX,
            _ => current,
        }
    };
    let clamped = chatlog::clamp_scroll(next, content, viewport);
    if clamped != current {
        state.canvas_view.scroll = clamped;
        let info = SCROLLINFO {
            cbSize: size_of::<SCROLLINFO>() as u32,
            fMask: SIF_RANGE | SIF_PAGE | SIF_POS,
            nMin: 0,
            nMax: content,
            nPage: viewport.max(1) as u32,
            nPos: clamped,
            nTrackPos: 0,
        };
        SetScrollInfo(state.canvas, SB_VERT, &info, 1);
        InvalidateRect(state.canvas, null(), 0);
    }
}

/// WM_COMMAND 分发。各动作自行管理两阶段；`window` 供模态确认使用。
unsafe fn handle_command(pointer: *mut DesktopState, window: HWND, wparam: WPARAM) {
    let id = (wparam & 0xFFFF) as i32;
    let notification = ((wparam >> 16) & 0xFFFF) as u32;
    match notification {
        BN_CLICKED => match id {
            ID_CHAT_TAB => select_tab(pointer, None),
            252 => model_settings::save(pointer),
            ID_SEND => send_chat(pointer),
            ID_MEM_REMEMBER => remember_memory(pointer),
            ID_MEM_EDIT => edit_memory(pointer),
            ID_MEM_DELETE => delete_memory(pointer, window),
            ID_MEM_REFRESH => {
                let (input, search, main) = {
                    let state = &mut *pointer;
                    state.view.editing_memory = None;
                    (
                        state.controls.mem_input,
                        state.controls.mem_search,
                        state.main,
                    )
                };
                let captured = window_instance(main);
                SetWindowTextW(input, wide("").as_ptr());
                if !instance_valid(main, captured) {
                    return;
                }
                SetWindowTextW(search, wide("").as_ptr());
                if !instance_valid(main, captured) {
                    return;
                }
                act_at(pointer, Action::SearchMemories(String::new()));
                refresh(pointer);
            }
            ID_PRESET_APPLY => apply_selected_preset(pointer),
            ID_LOG_REFRESH => {
                act_at(pointer, Action::RefreshDiagnostics);
                refresh(pointer);
            }
            ID_REVOKE => revoke_authorization(pointer, window),
            _ => {
                if let Some(index) = TAB_IDS.iter().position(|tab| *tab == id) {
                    let tab = SideTab::ALL[index];
                    let target = {
                        let state = &*pointer;
                        if state.view.active_tab == Some(tab) {
                            None
                        } else {
                            Some(tab)
                        }
                    };
                    select_tab(pointer, target);
                }
            }
        },
        EN_CHANGE => {
            if id == ID_MEM_SEARCH {
                if (*pointer).view.header.revoked {
                    return;
                }
                let (main, input) = {
                    let state = &*pointer;
                    (state.main, state.controls.mem_search)
                };
                let Some(text) = control_text_if_alive(main, input) else {
                    return;
                };
                act_at(pointer, Action::SearchMemories(text));
                refresh(pointer);
                return;
            }
            if id == ID_INPUT {
                let (main, input) = {
                    let state = &*pointer;
                    (state.main, state.controls.input)
                };
                // WM_GETTEXT 进入控件过程后才返回文本；实例失效则状态
                // 可能已释放，不得继续借用。
                let Some(text) = control_text_if_alive(main, input) else {
                    return;
                };
                act_at(pointer, Action::SetDraft(text));
                invalidate_status(pointer);
            }
        }
        EN_SETFOCUS | LBN_SETFOCUS => {
            {
                let state = &mut *pointer;
                state.focus_ring = id;
            }
            invalidate_control_area(pointer, id);
        }
        EN_KILLFOCUS | LBN_KILLFOCUS => {
            {
                let state = &mut *pointer;
                if state.focus_ring == id {
                    state.focus_ring = 0;
                }
            }
            invalidate_control_area(pointer, id);
        }
        LBN_SELCHANGE => match id {
            ID_MEM_LIST => {
                let (main, mem_list) = {
                    let state = &*pointer;
                    (state.main, state.controls.mem_list)
                };
                // LB_GETCURSEL 进入控件过程；查询移出借用并复验实例，
                // 避免子类化控件在查询中重入时出现并存借用。
                let Some(selection) = selection_if_alive(main, mem_list) else {
                    return;
                };
                {
                    let state = &mut *pointer;
                    state.view.mem_selected = selection
                        .and_then(|index| state.view.memory.rows.get(index as usize))
                        .map(|row| row.id.clone());
                    update_details(state);
                }
                refresh(pointer);
            }
            ID_PRESET_LIST => {
                let (main, preset_list) = {
                    let state = &*pointer;
                    (state.main, state.controls.preset_list)
                };
                let Some(selection) = selection_if_alive(main, preset_list) else {
                    return;
                };
                {
                    let state = &mut *pointer;
                    state.view.preset_selected = selection
                        .and_then(|index| state.view.presets.rows.get(index as usize))
                        .map(|row| row.id.clone());
                    update_details(state);
                }
                invalidate_sidebar(pointer);
                update_apply_enabled(pointer);
            }
            _ => {}
        },
        _ => {}
    }
}

/// 选中预设变化后立即同步“使用此预设”的可用性，不必等下一次全量刷新。
unsafe fn update_apply_enabled(pointer: *mut DesktopState) {
    let (apply, enabled) = {
        let state = &*pointer;
        let enabled = state.view.active_tab == Some(SideTab::Presets)
            && state.view.presets.visible
            && !state.view.header.revoked
            && state.view.presets.rows.iter().any(|row| {
                Some(row.id.as_str()) == state.view.preset_selected.as_deref() && !row.active
            });
        (state.controls.preset_apply, enabled)
    };
    EnableWindow(apply, enabled as i32);
}

/// 选中预设变化后立即同步“使用此预设”的可用性，不必等下一次全量刷新。
unsafe fn send_chat(pointer: *mut DesktopState) {
    let (main, input) = {
        let state = &*pointer;
        if !state.view.status.send_enabled {
            return;
        }
        (state.main, state.controls.input)
    };
    let Some(text) = control_text_if_alive(main, input) else {
        return;
    };
    act_at(pointer, Action::SetDraft(text));
    act_at(pointer, Action::Send);
    model_settings::start(pointer);
    refresh(pointer);
}

unsafe fn remember_memory(pointer: *mut DesktopState) {
    let (main, mem_input) = {
        let state = &*pointer;
        if state.view.header.revoked || !state.view.memory.visible {
            return;
        }
        (state.main, state.controls.mem_input)
    };
    let captured = window_instance(main);
    let Some(text) = control_text_if_alive(main, mem_input) else {
        return;
    };
    let succeeded = {
        let state = &mut *pointer;
        let action = match &state.view.editing_memory {
            Some((id, revision)) => Action::UpdateMemory {
                id: id.clone(),
                revision: *revision,
                text,
            },
            None => Action::Remember(text),
        };
        act(state, action);
        if state.view.error.is_none() {
            state.view.editing_memory = None;
        }
        state.view.error.is_none()
    };
    refresh(pointer);
    if succeeded && instance_valid(main, captured) {
        // 单行 EDIT；`SetWindowTextW` 可能同步回发 EN_CHANGE，由
        // `handle_command` 的短借用安全处理。刷新期间实例若已销毁，
        // 不再触碰任何句柄。
        SetWindowTextW(mem_input, wide("").as_ptr());
    }
}

unsafe fn edit_memory(pointer: *mut DesktopState) {
    let (main, input, text) = {
        let state = &mut *pointer;
        let snapshot = state.app.snapshot();
        let Some(note) = snapshot
            .memories
            .iter()
            .find(|note| Some(&note.id) == state.view.mem_selected.as_ref())
        else {
            return;
        };
        state.view.editing_memory = Some((note.id.clone(), note.revision));
        (state.main, state.controls.mem_input, note.text.clone())
    };
    let captured = window_instance(main);
    SetWindowTextW(input, wide(&text).as_ptr());
    if instance_valid(main, captured) {
        SetFocus(input);
        if instance_valid(main, captured) {
            invalidate_sidebar(pointer);
        }
    }
}

unsafe fn delete_memory(pointer: *mut DesktopState, window: HWND) {
    let (note, confirm) = {
        let state = &*pointer;
        let snapshot = state.app.snapshot();
        let Some(note) = snapshot
            .memories
            .iter()
            .find(|note| Some(&note.id) == state.view.mem_selected.as_ref())
            .cloned()
        else {
            return;
        };
        (note, state.confirm)
    };
    let captured = window_instance(window);
    if confirm(
        window,
        "删除这条记忆？",
        "删除后，后续对话将不再引用这条记忆。已有聊天内容不会被删除。",
    ) != IDYES
        || !instance_valid(window, captured)
    {
        return;
    }
    act_at(
        pointer,
        Action::DeleteMemory {
            id: note.id.clone(),
            revision: note.revision,
        },
    );
    let (input, clear) = {
        let state = &mut *pointer;
        let clear = state.view.error.is_none()
            && state
                .view
                .editing_memory
                .as_ref()
                .is_some_and(|(id, _)| *id == note.id);
        if clear {
            state.view.editing_memory = None;
        }
        (state.controls.mem_input, clear)
    };
    if clear {
        SetWindowTextW(input, wide("").as_ptr());
        if !instance_valid(window, captured) {
            return;
        }
        SendMessageW(input, EM_EMPTYUNDOBUFFER, 0, 0);
        if !instance_valid(window, captured) {
            return;
        }
    }
    refresh(pointer);
}

unsafe fn apply_selected_preset(pointer: *mut DesktopState) {
    {
        let state = &mut *pointer;
        let Some(id) = state.view.preset_selected.clone() else {
            return;
        };
        let Some(row) = state.view.presets.rows.iter().find(|row| row.id == id) else {
            return;
        };
        if row.active || state.view.header.revoked || !state.view.presets.visible {
            return;
        }
        act(state, Action::SelectPreset(id));
    }
    refresh(pointer);
}

unsafe fn revoke_authorization(pointer: *mut DesktopState, window: HWND) {
    let captured = window_instance(window);
    let confirm = {
        let state = &*pointer;
        state.confirm
    };
    // 阶段 B：模态循环期间不持有任何借用；重入的消息各自处理。
    let answer = confirm(
        window,
        "撤销授权",
        "撤销后本进程将不再接受任何操作，且无法恢复。确认撤销本次授权？",
    );
    // 模态返回后按实例代际复验：窗口若在模态期间销毁或 HWND/地址被
    // 新实例复用，一律不再触碰指针。
    if !instance_valid(window, captured) {
        return;
    }
    if answer == IDYES {
        act_at(pointer, Action::Revoke);
        refresh(pointer);
    }
}

unsafe fn select_tab(pointer: *mut DesktopState, target: Option<SideTab>) {
    let main = (*pointer).main;
    let captured = window_instance(main);
    {
        let state = &mut *pointer;
        let snapshot = state.app.snapshot();
        let visible = |tab: SideTab| {
            snapshot.panels.iter().any(|panel| {
                panel.kind == tab.panel_kind() && panel.visibility == PanelVisibility::Visible
            })
        };
        for tab in SideTab::ALL {
            if visible(tab) && target != Some(tab) {
                act(state, Action::Hide(tab.panel_kind()));
            }
        }
        if let Some(tab) = target {
            if !visible(tab) {
                act(state, Action::Open(tab.panel_kind()));
            }
            match tab {
                SideTab::Memory => act(state, Action::RefreshMemories),
                SideTab::Logs => act(state, Action::RefreshDiagnostics),
                SideTab::Presets => {}
            }
        }
        state.view.active_tab = target;
    }
    refresh(pointer);
    if !instance_valid(main, captured) {
        return;
    }
    let focus = {
        let state = &*pointer;
        if state.view.header.revoked {
            None
        } else {
            Some(state.view.active_tab.map_or(state.controls.input, |tab| {
                state.controls.tabs[SideTab::ALL.iter().position(|value| *value == tab).unwrap()]
            }))
        }
    };
    if let Some(control) = focus {
        SetFocus(control);
    }
}

unsafe fn act_at(pointer: *mut DesktopState, action: Action) {
    let state = &mut *pointer;
    act(state, action);
}

fn act(state: &mut DesktopState, action: Action) {
    let now = elapsed_ms(state);
    // SetDraft 不推进修订号：真实键入由消息队列驱动，阶段 B 内不泵
    // 队列、不会交错；阶段 B 内出现的 EN_CHANGE 只能是本计划自己
    // `SetWindowTextW` 的幂等回显，不构成对在途计划的结构性失效。
    let structural = !matches!(action, Action::SetDraft(_));
    let revoke = matches!(action, Action::Revoke);
    match state.app.dispatch(action, now) {
        Ok(()) => {
            if revoke {
                state.model_key.clear();
                state.initial_settings = None;
            }
            state.view.error = None;
            if structural {
                state.revision = state.revision.wrapping_add(1);
            }
        }
        Err(error) => set_error(state, &error),
    }
}

fn set_error(state: &mut DesktopState, error: &AppError) {
    state.view.error = Some((
        format!("操作未执行：{}", error.public_message()),
        Severity::Error,
    ));
}

/// 经既有关闭确认语义关闭窗口：有未发送草稿时先请求确认再丢弃。
/// `DestroyWindow` 同步派发的销毁/焦点消息由各消息自己的短借用处理；
/// 销毁返回后本函数不再触碰状态指针。
unsafe fn handle_window_close(pointer: *mut DesktopState, window: HWND) {
    let (input, confirm, revoked) = {
        let state = &*pointer;
        (
            state.controls.mem_input,
            state.confirm,
            state.view.header.revoked,
        )
    };
    let captured = window_instance(window);
    let Some(memory_draft) = control_text_if_alive(window, input) else {
        return;
    };
    if !revoked
        && !memory_draft.trim().is_empty()
        && (confirm(
            window,
            "还有未保存的记忆",
            "记忆输入尚未保存。关闭会丢弃这些内容，仍要关闭吗？",
        ) != IDYES
            || !instance_valid(window, captured))
    {
        return;
    }
    enum CloseStep {
        Destroy,
        Ask,
        Report,
    }
    let step = {
        let state = &mut *pointer;
        match state
            .app
            .dispatch(Action::Close(PanelKind::Chat), elapsed_ms(state))
        {
            Ok(()) => {
                let snapshot = state.app.snapshot();
                let pending = snapshot.panels.iter().any(|panel| {
                    panel.kind == PanelKind::Chat
                        && panel.confirmation
                        && panel.visibility != PanelVisibility::Closed
                });
                if pending {
                    CloseStep::Ask
                } else {
                    CloseStep::Destroy
                }
            }
            Err(error) => {
                if error.public_code() == "panel_not_open" || error.public_code() == "revoked" {
                    CloseStep::Destroy
                } else {
                    set_error(state, &error);
                    CloseStep::Report
                }
            }
        }
    };
    match step {
        CloseStep::Destroy => {
            DestroyWindow(window);
        }
        CloseStep::Report => refresh(pointer),
        CloseStep::Ask => {
            let captured = window_instance(window);
            let confirm = {
                let state = &*pointer;
                state.confirm
            };
            let answer = confirm(
                window,
                "关闭确认",
                "聊天草稿尚未发送，关闭将丢弃草稿。确认关闭？",
            );
            if !instance_valid(window, captured) {
                return;
            }
            let outcome = {
                let state = &mut *pointer;
                if answer == IDYES {
                    match state.app.dispatch(
                        Action::ConfirmClose {
                            panel: PanelKind::Chat,
                            discard: true,
                        },
                        elapsed_ms(state),
                    ) {
                        Ok(()) => CloseStep::Destroy,
                        Err(error) => {
                            set_error(state, &error);
                            CloseStep::Report
                        }
                    }
                } else {
                    let _ = state.app.dispatch(
                        Action::ConfirmClose {
                            panel: PanelKind::Chat,
                            discard: false,
                        },
                        elapsed_ms(state),
                    );
                    CloseStep::Report
                }
            };
            match outcome {
                CloseStep::Destroy => {
                    DestroyWindow(window);
                }
                CloseStep::Report => refresh(pointer),
                CloseStep::Ask => unreachable!("close plan never re-asks"),
            }
        }
    }
}

fn elapsed_ms(state: &DesktopState) -> u64 {
    state
        .epoch_ms
        .saturating_add(u64::try_from(state.clock.elapsed().as_millis()).unwrap_or(u64::MAX))
}

impl ViewState {
    fn initial(snapshot: &AppSnapshot) -> Self {
        Self {
            header: presenter::header(snapshot),
            status: presenter::status(snapshot),
            notice: presenter::notice(snapshot),
            memory: presenter::memory_panel(snapshot),
            presets: presenter::presets_panel(snapshot),
            logs: presenter::logs_panel(snapshot),
            mem_detail: presenter::panel_detail(SideTab::Memory, None),
            preset_detail: presenter::panel_detail(SideTab::Presets, None),
            log_detail: presenter::panel_detail(SideTab::Logs, None),
            active_tab: None,
            mem_selected: None,
            editing_memory: None,
            preset_selected: None,
            error: None,
        }
    }
}

/// 全量刷新。阶段 A 借用状态完成视图模型与副作用计划计算（`None` 表
/// 示量测期间窗口实例失效，直接放弃）；阶段 B 无借用执行计划，每步
/// 可重入副作用后复验实例与修订号，失效立即中止。
unsafe fn refresh(pointer: *mut DesktopState) {
    let (main, input) = {
        let state = &*pointer;
        (state.main, state.controls.input)
    };
    let Some(current) = control_text_if_alive(main, input) else {
        return;
    };
    let plan = {
        let state = &mut *pointer;
        let snapshot = state.app.snapshot();
        state.view.header = presenter::header(&snapshot);
        state.view.status = presenter::status(&snapshot);
        state.view.notice = presenter::notice(&snapshot);
        state.view.memory = presenter::memory_panel(&snapshot);
        state.view.presets = presenter::presets_panel(&snapshot);
        state.view.logs = presenter::logs_panel(&snapshot);
        state.view.mem_selected =
            retained_selection(&state.view.memory.rows, state.view.mem_selected.take());
        state.view.preset_selected =
            retained_selection(&state.view.presets.rows, state.view.preset_selected.take());
        update_details(state);
        match build_window_plan(pointer, state, &snapshot, &current) {
            Some(plan) => plan,
            None => return,
        }
    };
    apply_window_plan(plan);
}

fn update_details(state: &mut DesktopState) {
    let memory_row = selected_row(&state.view.memory.rows, &state.view.mem_selected);
    let preset_row = selected_row(&state.view.presets.rows, &state.view.preset_selected);
    state.view.mem_detail = presenter::panel_detail(SideTab::Memory, memory_row);
    state.view.preset_detail = presenter::panel_detail(SideTab::Presets, preset_row);
    state.view.log_detail = presenter::panel_detail(SideTab::Logs, None);
}

fn selected_row<'a>(rows: &'a [Row], selected: &Option<String>) -> Option<&'a Row> {
    let id = selected.as_ref()?;
    rows.iter().find(|row| &row.id == id)
}

fn retained_selection(rows: &[Row], previous: Option<String>) -> Option<String> {
    let id = previous?;
    rows.iter()
        .find(|row| row.id == id)
        .map(|row| row.id.clone())
}

/// 阶段 A：由最新快照推导控件启用/可见性、草稿同步、列表重建、布局
/// 移动与画布量测。允许的 Win32 见模块文档封闭集合；`GetWindowTextW`
/// 属于可重入查询，返回后复验实例，失效返回 `None` 且不再触碰状态。
unsafe fn build_window_plan(
    pointer: *mut DesktopState,
    state: &mut DesktopState,
    snapshot: &AppSnapshot,
    current: &str,
) -> Option<WindowPlan> {
    let captured_instance = window_instance(state.main);
    let captured_revision = state.revision;
    let interactive = !snapshot.revoked;
    if !interactive {
        // 撤权：清除所选记忆与详情文本（输入框文本与撤销缓冲在阶段 B 清空）。
        state.view.mem_selected = None;
        state.view.editing_memory = None;
        state.view.mem_detail.clear();
    }
    let active_tab = state.view.active_tab;
    let memory_active = active_tab == Some(SideTab::Memory) && state.view.memory.visible;
    let presets_active = active_tab == Some(SideTab::Presets) && state.view.presets.visible;
    let logs_active = active_tab == Some(SideTab::Logs) && state.view.logs.visible;

    let controls = state.controls;
    let mut plan = WindowPlan {
        state: pointer,
        instance: captured_instance,
        revision: captured_revision,
        controls,
        canvas: state.canvas,
        main: state.main,
        input_enabled: interactive,
        send_enabled: interactive && state.view.status.send_enabled,
        tabs_enabled: [interactive; 3],
        revoke_enabled: interactive,
        memory_on: memory_active && interactive,
        memory_selected: memory_active && interactive && state.view.mem_selected.is_some(),
        presets_on: presets_active && interactive,
        logs_on: logs_active && interactive,
        list_has_rows: [
            !state.view.memory.rows.is_empty(),
            !state.view.presets.rows.is_empty(),
            !state.view.logs.rows.is_empty(),
        ],
        apply_enabled: false,
        clear_revoked_inputs: !interactive,
        // 撤权计划永不回写草稿：即便快照仍带残留文本，也不得把它
        // 送回已清空的输入框。
        draft: None,
        lists: [None, None, None],
        moves: Vec::new(),
        item_heights: Vec::new(),
        scroll: None,
        invalidate_canvas: false,
        invalidate_main: false,
    };

    plan.lists[0] = list_reload(
        controls.mem_list,
        &state.view.memory.rows,
        0,
        state.view.mem_selected.as_deref(),
        &mut state.list_sigs,
    );
    plan.lists[1] = list_reload(
        controls.preset_list,
        &state.view.presets.rows,
        1,
        state.view.preset_selected.as_deref(),
        &mut state.list_sigs,
    );
    plan.lists[2] = list_reload(
        controls.log_list,
        &state.view.logs.rows,
        2,
        None,
        &mut state.list_sigs,
    );

    plan.apply_enabled = presets_active
        && interactive
        && state.view.presets.rows.iter().any(|row| {
            Some(row.id.as_str()) == state.view.preset_selected.as_deref() && !row.active
        });

    // WM_GETTEXT 会进入输入框过程；返回后实例若已失效（子类化回调
    // 中销毁主窗口），状态可能已释放，`?` 立即放弃本计划。
    if interactive && current != snapshot.chat.draft {
        plan.draft = Some(snapshot.chat.draft.clone());
    }

    let mut rect = RECT::default();
    GetClientRect(state.main, &mut rect);
    let client_w = rect.right - rect.left;
    let client_h = rect.bottom - rect.top;
    let plan_layout = layout::compute_with_drawer(
        client_w,
        client_h,
        state.view.notice.is_some(),
        state.view.active_tab.is_some(),
        &state.metrics,
    );
    let panel_layouts = [
        layout::panel_children(plan_layout.panel, &state.metrics, 4, true),
        layout::panel_children(plan_layout.panel, &state.metrics, 1, false),
        layout::panel_children(plan_layout.panel, &state.metrics, 1, false),
    ];
    let previous_viewport = state
        .layout
        .map_or(plan_layout.canvas.h, |old| old.canvas.h);
    state.panels = panel_layouts.clone();
    state.layout = Some(plan_layout);

    let memory = &panel_layouts[0];
    for (handle, rect) in [
        (controls.chat_tab, plan_layout.chat_tab),
        (controls.mem_search, memory.search),
        (controls.mem_edit, memory.actions[2]),
        (controls.mem_delete, memory.actions[3]),
    ] {
        plan.moves.push((handle, rect.x, rect.y, rect.w, rect.h));
    }
    let presets = &panel_layouts[1];
    let logs = &panel_layouts[2];
    plan.moves.push((
        state.canvas,
        plan_layout.canvas.x,
        plan_layout.canvas.y,
        plan_layout.canvas.w,
        plan_layout.canvas.h,
    ));
    plan.moves.push((
        controls.input,
        plan_layout.input.x,
        plan_layout.input.y,
        plan_layout.input.w,
        plan_layout.input.h,
    ));
    plan.moves.push((
        controls.send,
        plan_layout.send.x,
        plan_layout.send.y,
        plan_layout.send.w,
        plan_layout.send.h,
    ));
    for (tab, rect) in controls.tabs.iter().zip(&plan_layout.tabs) {
        plan.moves.push((*tab, rect.x, rect.y, rect.w, rect.h));
    }
    plan.moves.push((
        controls.mem_list,
        memory.list.x,
        memory.list.y,
        memory.list.w,
        memory.list.h,
    ));
    plan.moves.push((
        controls.mem_input,
        memory.input.x,
        memory.input.y,
        memory.input.w,
        memory.input.h,
    ));
    plan.moves.push((
        controls.mem_remember,
        memory.actions[0].x,
        memory.actions[0].y,
        memory.actions[0].w,
        memory.actions[0].h,
    ));
    plan.moves.push((
        controls.mem_refresh,
        memory.actions[1].x,
        memory.actions[1].y,
        memory.actions[1].w,
        memory.actions[1].h,
    ));
    for (control, rect) in controls
        .model_fields
        .into_iter()
        .zip(model_settings::rects(presets.list, state.dpi))
    {
        plan.moves.push((control, rect.x, rect.y, rect.w, rect.h));
    }
    plan.moves.push((
        controls.preset_list,
        presets.list.x,
        presets.list.y,
        presets.list.w - theme::scale(88, state.dpi),
        theme::scale(44, state.dpi),
    ));
    plan.moves.push((
        controls.preset_apply,
        presets.list.right() - theme::scale(80, state.dpi),
        presets.list.y,
        theme::scale(80, state.dpi),
        theme::scale(36, state.dpi),
    ));
    plan.moves.push((
        controls.log_list,
        logs.list.x,
        logs.list.y,
        logs.list.w,
        logs.list.h,
    ));
    plan.moves.push((
        controls.log_refresh,
        logs.actions[0].x,
        logs.actions[0].y,
        logs.actions[0].w,
        logs.actions[0].h,
    ));
    plan.moves.push((
        controls.revoke,
        plan_layout.revoke.x,
        plan_layout.revoke.y,
        plan_layout.revoke.w,
        plan_layout.revoke.h,
    ));

    plan.item_heights
        .push((controls.mem_list, state.small_line + state.metrics.gap * 2));
    plan.item_heights.push((
        controls.preset_list,
        state.small_line * 2 + state.metrics.gap,
    ));
    plan.item_heights.push((
        controls.log_list,
        state.small_line * 2 + state.metrics.gap * 2,
    ));

    // 画布：按新布局宽度量测，避免一次缩放/resize 滞后。
    // `GetDC` + `DrawTextW(DT_CALCRECT)` 只做 GDI 量测，不分发消息。
    let viewport = plan_layout.canvas.h;
    let width = plan_layout.canvas.w;
    let messages = &snapshot.chat.messages;
    let total_chars: usize = messages.iter().map(|message| message.text.len()).sum();
    let sig = (messages.len(), width, total_chars);
    if state.canvas_view.layout.is_none() || state.canvas_view.sig != sig {
        let text_width = paint::bubble_inner_width(state, width);
        let metrics = chatlog_metrics(state);
        let measured = chatlog::layout_history(messages, metrics, |text| {
            paint::measure_lines(state, text, text_width)
        });
        let previous = state.canvas_view.layout.take();
        let (previous_scroll, previous_content) = match previous {
            Some(history) => (state.canvas_view.scroll, history.content_height),
            None => (0, 0),
        };
        state.canvas_view.scroll = chatlog::scroll_after_content_change(
            previous_scroll,
            previous_content,
            measured.content_height,
            previous_viewport,
            viewport,
        );
        state.canvas_view.messages = messages.to_vec();
        state.canvas_view.layout = Some(measured);
        state.canvas_view.sig = sig;
    } else if let Some(history) = state.canvas_view.layout.as_ref() {
        state.canvas_view.scroll = chatlog::scroll_after_content_change(
            state.canvas_view.scroll,
            history.content_height,
            history.content_height,
            previous_viewport,
            viewport,
        );
    }
    let content_height = state
        .canvas_view
        .layout
        .as_ref()
        .map_or(0, |history| history.content_height);
    plan.scroll = Some((
        state.canvas,
        content_height,
        viewport.max(1) as u32,
        state.canvas_view.scroll,
    ));
    plan.invalidate_canvas = true;
    plan.invalidate_main = true;
    Some(plan)
}

/// 阶段 B：执行副作用。不持有状态借用；每一步允许同步回调的副作用
/// （`SetWindowTextW`、`EnableWindow`、`ShowWindow`、`LB_*` 扇出）返回
/// 后经 `plan.valid()` 复验——重入消息若已销毁窗口或完成撤权等结构性
/// 变更，剩余副作用（重新启用控件、回写过期草稿、按旧几何移动）立即
/// 停止，界面由重入方自己的刷新收敛。`MoveWindow` 作用于标准子控件、
/// `SetScrollInfo`/`InvalidateRect` 不分发窗口消息，同组尾部统一复验。
unsafe fn apply_window_plan(plan: WindowPlan) {
    let controls = plan.controls;
    // EM_SETCUEBANNER: hints are native control content, not a painted overlay.
    for (control, hint) in [
        (controls.mem_search, "搜索记忆…"),
        (controls.mem_input, "写下希望 Moe 记住的事…"),
    ] {
        SendMessageW(control, 0x1501, 0, wide(hint).as_ptr() as LPARAM);
        if !plan.valid() {
            return;
        }
    }
    if plan.clear_revoked_inputs {
        // 撤权：文本与撤销缓冲一起清空——WM_GETTEXT 读不到内容，
        // 撤销（EM_UNDO）也无法恢复。
        SetWindowTextW(controls.input, wide("").as_ptr());
        if !plan.valid() {
            return;
        }
        SendMessageW(controls.input, EM_EMPTYUNDOBUFFER, 0, 0);
        if !plan.valid() {
            return;
        }
        SetWindowTextW(controls.mem_input, wide("").as_ptr());
        if !plan.valid() {
            return;
        }
        SendMessageW(controls.mem_input, EM_EMPTYUNDOBUFFER, 0, 0);
        if !plan.valid() {
            return;
        }
        SetWindowTextW(controls.mem_search, wide("").as_ptr());
        if !plan.valid() {
            return;
        }
        SendMessageW(controls.mem_search, EM_EMPTYUNDOBUFFER, 0, 0);
    }
    if !plan.valid() {
        return;
    }
    for (control, enabled) in [
        (controls.chat_tab, plan.input_enabled),
        (controls.input, plan.input_enabled),
        (controls.send, plan.send_enabled),
        (controls.tabs[0], plan.tabs_enabled[0]),
        (controls.tabs[1], plan.tabs_enabled[1]),
        (controls.tabs[2], plan.tabs_enabled[2]),
        (controls.revoke, plan.revoke_enabled),
    ] {
        EnableWindow(control, enabled as i32);
        if !plan.valid() {
            return;
        }
    }

    for (control, on) in [
        (controls.mem_list, plan.memory_on && plan.list_has_rows[0]),
        (controls.mem_input, plan.memory_on),
        (controls.mem_remember, plan.memory_on),
        (controls.mem_refresh, plan.memory_on),
        (controls.mem_search, plan.memory_on),
        (controls.mem_edit, plan.memory_on),
        (controls.mem_delete, plan.memory_on),
        (
            controls.preset_list,
            plan.presets_on && plan.list_has_rows[1],
        ),
        (controls.preset_apply, plan.presets_on),
        (controls.log_list, plan.logs_on && plan.list_has_rows[2]),
        (controls.log_refresh, plan.logs_on),
    ] {
        ShowWindow(control, if on { SW_SHOWNA } else { SW_HIDE });
        if !plan.valid() {
            return;
        }
        EnableWindow(control, on as i32);
        if !plan.valid() {
            return;
        }
    }
    for control in controls.model_fields {
        if plan.clear_revoked_inputs {
            SetWindowTextW(control, wide("").as_ptr());
            if !plan.valid() {
                return;
            }
        }
        ShowWindow(control, if plan.presets_on { SW_SHOWNA } else { SW_HIDE });
        if !plan.valid() {
            return;
        }
        EnableWindow(control, (!plan.clear_revoked_inputs) as i32);
        if !plan.valid() {
            return;
        }
    }
    EnableWindow(controls.preset_apply, plan.apply_enabled as i32);
    if !plan.valid() {
        return;
    }
    for control in [controls.mem_edit, controls.mem_delete] {
        EnableWindow(control, plan.memory_selected as i32);
        if !plan.valid() {
            return;
        }
    }

    for reload in plan.lists.iter().flatten() {
        reload_list(reload, &plan);
        if !plan.valid() {
            return;
        }
    }

    if let Some(draft) = &plan.draft {
        // 多行 EDIT 会同步回发 EN_CHANGE；重入的 `handle_command` 以
        // 短借用处理同一份草稿（SetDraft 不推进修订号，属幂等收敛）。
        SetWindowTextW(controls.input, wide(draft).as_ptr());
        if !plan.valid() {
            return;
        }
    }

    for (control, x, y, w, h) in &plan.moves {
        MoveWindow(*control, *x, *y, (*w).max(1), (*h).max(1), 1);
        if !plan.valid() {
            return;
        }
    }
    if !plan.valid() {
        return;
    }
    for (control, height) in &plan.item_heights {
        SendMessageW(*control, LB_SETITEMHEIGHT, 0, *height as isize);
        if !plan.valid() {
            return;
        }
    }
    if !plan.valid() {
        return;
    }
    if let Some((canvas, n_max, page, pos)) = plan.scroll {
        let info = SCROLLINFO {
            cbSize: size_of::<SCROLLINFO>() as u32,
            fMask: SIF_RANGE | SIF_PAGE | SIF_POS,
            nMin: 0,
            nMax: n_max,
            nPage: page,
            nPos: pos,
            nTrackPos: 0,
        };
        SetScrollInfo(canvas, SB_VERT, &info, 1);
    }
    if plan.invalidate_canvas {
        InvalidateRect(plan.canvas, null(), 0);
    }
    if plan.invalidate_main {
        InvalidateRect(plan.main, null(), 0);
    }
}

/// 以“行数 + 内容 FNV-1a”为签名重建列表，避免每次按键清空选中与滚动。
/// 签名比对在阶段 A 完成；返回 `None` 表示无需重建。
fn list_reload(
    control: HWND,
    rows: &[Row],
    slot: usize,
    keep_id: Option<&str>,
    list_sigs: &mut [Option<(usize, u64)>; 3],
) -> Option<ListReload> {
    // FNV-1a 覆盖行文本与激活标记：字符在行间互换时总和不变，哈希会变。
    let mut content_hash: u64 = 0xcbf2_9ce4_8422_2325;
    for row in rows {
        for byte in row.primary.as_bytes() {
            content_hash ^= u64::from(*byte);
            content_hash = content_hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        if let Some(secondary) = row.secondary.as_deref() {
            for byte in secondary.as_bytes() {
                content_hash ^= u64::from(*byte);
                content_hash = content_hash.wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
        content_hash ^= u64::from(row.active);
        content_hash = content_hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let sig = (rows.len(), content_hash);
    if list_sigs[slot] == Some(sig) {
        return None;
    }
    list_sigs[slot] = Some(sig);
    let selection = keep_id
        .and_then(|id| rows.iter().position(|row| row.id == id))
        .map_or(-1, |index| index as i32);
    Some(ListReload {
        control,
        items: rows.iter().map(|row| row.primary.clone()).collect(),
        selection,
    })
}

unsafe fn reload_list(reload: &ListReload, plan: &WindowPlan) {
    SendMessageW(reload.control, LB_RESETCONTENT, 0, 0);
    if !plan.valid() {
        return;
    }
    for (index, item) in reload.items.iter().enumerate() {
        let text = wide(item);
        let position = SendMessageW(reload.control, LB_ADDSTRING, 0, text.as_ptr() as isize);
        if !plan.valid() {
            return;
        }
        if position >= 0 {
            SendMessageW(
                reload.control,
                LB_SETITEMDATA,
                position as usize,
                index as isize,
            );
            if !plan.valid() {
                return;
            }
        }
    }
    SendMessageW(reload.control, LB_SETCURSEL, reload.selection as usize, 0);
}

/// 读取控件文本并复验窗口实例。`GetWindowTextW` 会进入控件过程
/// （子类化的控件或异常路径可能在其中销毁主窗口），文本返回后先比对
/// 注册表代际，实例失效返回 `None`，调用方不得再解引用状态指针。
unsafe fn control_text_if_alive(main: HWND, control: HWND) -> Option<String> {
    let captured = window_instance(main);
    let text = control_text(control);
    if instance_valid(main, captured) {
        Some(text)
    } else {
        None
    }
}

/// `LB_GETCURSEL` 同样会进入控件过程；与 `control_text_if_alive` 同一
/// 纪律：查询移出借用、返回后复验实例，才允许继续使用状态。外层
/// `Option` 为 `None` 表示实例失效；内层 `None` 表示无选中项。
unsafe fn selection_if_alive(main: HWND, control: HWND) -> Option<Option<i32>> {
    let captured = window_instance(main);
    let index = SendMessageW(control, LB_GETCURSEL, 0, 0);
    if instance_valid(main, captured) {
        Some(if index < 0 { None } else { Some(index as i32) })
    } else {
        None
    }
}

fn chatlog_metrics(state: &DesktopState) -> chatlog::Metrics {
    chatlog::Metrics {
        line_height: state.body_line,
        label_height: state.small_line,
        pad_y: state.metrics.gap,
        gap: state.metrics.gap * 3,
    }
}

unsafe fn create_stage(pointer: *mut DesktopState, window: HWND) {
    let captured = window_instance(window);
    let (controls, fonts) = {
        let state = &mut *pointer;
        create_resources(state);
        // 子窗口创建只回发未匹配的 WM_MEASUREITEM（不建立任何引用），
        // 可以在借用内完成；控件句柄在返回前写入状态。
        let controls = create_controls(window, state);
        (controls, state.fonts)
    };
    // WM_SETFONT(重绘=1) 会同步触发自绘按钮/列表重绘并回发
    // WM_DRAWITEM；子类化控件甚至可能在回调中销毁主窗口。扇出返回后
    // 先复验实例，失效即不再刷新（状态 Box 可能已释放）。
    apply_fonts(controls, fonts, window, captured);
    if !instance_valid(window, captured) {
        return;
    }
    refresh(pointer);
}

unsafe fn apply_fonts(controls: Controls, fonts: Fonts, window: HWND, captured: Option<u64>) {
    for control in controls.model_fields {
        SendMessageW(control, WM_SETFONT, fonts.small as usize, 1);
        if !instance_valid(window, captured) {
            return;
        }
    }
    model_settings::labels(controls, window, captured);
    if !instance_valid(window, captured) {
        return;
    }
    let font_map = [
        (controls.chat_tab, fonts.small),
        (controls.mem_list, fonts.body),
        (controls.preset_list, fonts.body),
        (controls.log_list, fonts.mono),
        (controls.input, fonts.body),
        (controls.mem_input, fonts.body),
        (controls.send, fonts.body_bold),
        (controls.tabs[0], fonts.small),
        (controls.tabs[1], fonts.small),
        (controls.tabs[2], fonts.small),
        (controls.mem_remember, fonts.small),
        (controls.mem_refresh, fonts.small),
        (controls.mem_search, fonts.small),
        (controls.mem_edit, fonts.small),
        (controls.mem_delete, fonts.small),
        (controls.preset_apply, fonts.small),
        (controls.log_refresh, fonts.small),
        (controls.revoke, fonts.small),
    ];
    for (control, face) in font_map {
        SendMessageW(control, WM_SETFONT, face as usize, 1);
        if !instance_valid(window, captured) {
            return;
        }
    }
}

unsafe fn create_resources(state: &mut DesktopState) {
    state.brushes = Brushes {
        nav: brush(theme::NAV),
        bg: brush(theme::BG),
        panel: brush(theme::PANEL),
        accent: brush(theme::ACCENT),
        accent_dark: brush(theme::ACCENT_DARK),
        accent_soft: brush(theme::ACCENT_SOFT),
        border: brush(theme::BORDER),
        warn_soft: brush(theme::WARN_SOFT),
        disabled_fill: brush(theme::DISABLED_FILL),
    };
    rebuild_fonts(state);
}

/// 重建 GDI 字体与行高；字号保留 96 DPI 基准，入口统一按当前 DPI
/// 缩放一次。全部为 GDI 调用（`CreateFontW`/`GetDC`/`GetTextMetricsW`），
/// 不分发窗口消息。
unsafe fn rebuild_fonts(state: &mut DesktopState) {
    let old = [
        state.fonts.hero,
        state.fonts.title,
        state.fonts.body,
        state.fonts.body_bold,
        state.fonts.small,
        state.fonts.mono,
    ];
    state.fonts = Fonts {
        hero: font(state.dpi, 30, true, false),
        title: font(state.dpi, state.metrics.font_title, true, false),
        body: font(state.dpi, state.metrics.font_body, false, false),
        body_bold: font(state.dpi, state.metrics.font_body, true, false),
        small: font(state.dpi, state.metrics.font_small, false, false),
        mono: font(state.dpi, state.metrics.font_mono, false, true),
    };
    for handle in old {
        if !handle.is_null() {
            DeleteObject(handle as _);
        }
    }
    let (body_line, small_line) = font_line_heights(state);
    state.body_line = body_line;
    state.small_line = small_line;
    state.canvas_view.layout = None;
    state.canvas_view.sig = (0, 0, 0);
}

unsafe fn font_line_heights(state: &DesktopState) -> (i32, i32) {
    let hdc = GetDC(state.main);
    let mut metric = TEXTMETRICW::default();
    SelectObject(hdc, state.fonts.body as _);
    GetTextMetricsW(hdc, &mut metric);
    let body = metric.tmHeight + metric.tmExternalLeading;
    SelectObject(hdc, state.fonts.small as _);
    GetTextMetricsW(hdc, &mut metric);
    let small = metric.tmHeight + metric.tmExternalLeading;
    ReleaseDC(state.main, hdc);
    (body.max(1), small.max(1))
}

unsafe fn font(dpi: u32, size_px: i32, bold: bool, mono: bool) -> HFONT {
    let face = wide(if mono {
        theme::FONT_MONO
    } else {
        theme::FONT_UI
    });
    CreateFontW(
        -theme::scale(size_px, dpi),
        0,
        0,
        0,
        if bold {
            FW_BOLD as i32
        } else {
            FW_NORMAL as i32
        },
        0,
        0,
        0,
        u32::from(DEFAULT_CHARSET),
        u32::from(OUT_DEFAULT_PRECIS),
        u32::from(CLIP_DEFAULT_PRECIS),
        u32::from(CLEARTYPE_QUALITY),
        u32::from(DEFAULT_PITCH),
        face.as_ptr(),
    )
}

fn brush(colorref: COLORREF) -> HBRUSH {
    unsafe { CreateSolidBrush(colorref) }
}

unsafe fn create_controls(window: HWND, state: &mut DesktopState) -> Controls {
    let list_style = WS_TABSTOP
        | WS_VSCROLL
        | (LBS_NOTIFY | LBS_OWNERDRAWFIXED | LBS_NOINTEGRALHEIGHT | LBS_HASSTRINGS) as u32;
    let button_style = WS_TABSTOP | (BS_OWNERDRAW as u32);
    let mut controls = Controls::dangling();
    controls.chat_tab = create_control(
        window,
        "BUTTON",
        "聊天",
        button_style,
        0,
        ID_CHAT_TAB,
        Rect::zero(),
    );
    state.canvas = create_control(
        window,
        CANVAS_CLASS,
        "",
        WS_VSCROLL,
        0,
        ID_CANVAS,
        Rect::zero(),
    );
    controls.input = create_control(
        window,
        "EDIT",
        "",
        WS_TABSTOP | (ES_MULTILINE | ES_AUTOVSCROLL | ES_WANTRETURN) as u32,
        0,
        ID_INPUT,
        Rect::zero(),
    );
    controls.send = create_control(
        window,
        "BUTTON",
        "发送",
        button_style,
        0,
        ID_SEND,
        Rect::zero(),
    );
    for (index, tab) in SideTab::ALL.iter().enumerate() {
        controls.tabs[index] = create_control(
            window,
            "BUTTON",
            presenter::tab_title(*tab),
            button_style,
            0,
            TAB_IDS[index],
            Rect::zero(),
        );
    }
    controls.mem_list = create_control(
        window,
        "LISTBOX",
        "",
        list_style,
        0,
        ID_MEM_LIST,
        Rect::zero(),
    );
    controls.mem_input = create_control(
        window,
        "EDIT",
        "",
        WS_TABSTOP | WS_VSCROLL | (ES_MULTILINE | ES_AUTOVSCROLL | ES_WANTRETURN) as u32,
        0,
        ID_MEM_INPUT,
        Rect::zero(),
    );
    controls.mem_remember = create_control(
        window,
        "BUTTON",
        "保存记忆",
        button_style,
        0,
        ID_MEM_REMEMBER,
        Rect::zero(),
    );
    controls.mem_refresh = create_control(
        window,
        "BUTTON",
        "新建 / 刷新",
        button_style,
        0,
        ID_MEM_REFRESH,
        Rect::zero(),
    );
    controls.preset_list = create_control(
        window,
        "LISTBOX",
        "",
        list_style,
        0,
        ID_PRESET_LIST,
        Rect::zero(),
    );
    for (index, slot) in controls.model_fields.iter_mut().enumerate() {
        let initial = state
            .initial_settings
            .as_ref()
            .map(|settings| match index {
                0 => settings.endpoint.as_str(),
                1 => settings.model.as_str(),
                3 => settings.persona.as_str(),
                _ => "",
            })
            .unwrap_or("");
        *slot = create_control(
            window,
            if index < 4 { "EDIT" } else { "BUTTON" },
            initial,
            WS_TABSTOP
                | if index < 4 {
                    (if index == 3 {
                        (ES_MULTILINE | ES_AUTOVSCROLL | ES_WANTRETURN) as u32
                    } else {
                        ES_AUTOHSCROLL as u32
                    }) | if index == 2 {
                        windows_sys::Win32::UI::WindowsAndMessaging::ES_PASSWORD as u32
                    } else {
                        0
                    }
                } else if index == 4 {
                    windows_sys::Win32::UI::WindowsAndMessaging::BS_AUTOCHECKBOX as u32
                } else {
                    BS_OWNERDRAW as u32
                },
            0,
            if index == 5 { 252 } else { 260 + index as i32 },
            Rect::zero(),
        );
    }
    controls.preset_apply = create_control(
        window,
        "BUTTON",
        "切换预设",
        button_style,
        0,
        ID_PRESET_APPLY,
        Rect::zero(),
    );
    controls.log_list = create_control(
        window,
        "LISTBOX",
        "",
        list_style,
        0,
        ID_LOG_LIST,
        Rect::zero(),
    );
    controls.log_refresh = create_control(
        window,
        "BUTTON",
        "刷新",
        button_style,
        0,
        ID_LOG_REFRESH,
        Rect::zero(),
    );
    controls.revoke = create_control(
        window,
        "BUTTON",
        "撤销授权",
        button_style,
        0,
        ID_REVOKE,
        Rect::zero(),
    );
    controls.mem_search = create_control(
        window,
        "EDIT",
        "",
        WS_TABSTOP | ES_AUTOHSCROLL as u32,
        0,
        ID_MEM_SEARCH,
        Rect::zero(),
    );
    controls.mem_edit = create_control(
        window,
        "BUTTON",
        "编辑选中",
        button_style,
        0,
        ID_MEM_EDIT,
        Rect::zero(),
    );
    controls.mem_delete = create_control(
        window,
        "BUTTON",
        "删除选中",
        button_style,
        0,
        ID_MEM_DELETE,
        Rect::zero(),
    );
    state.controls = controls;
    for control in [
        controls.mem_list,
        controls.mem_input,
        controls.mem_remember,
        controls.mem_refresh,
        controls.mem_search,
        controls.mem_edit,
        controls.mem_delete,
        controls.preset_list,
        controls.preset_apply,
        controls.log_list,
        controls.log_refresh,
    ] {
        ShowWindow(control, SW_HIDE);
    }
    controls
}

unsafe fn create_control(
    parent: HWND,
    class: &str,
    text: &str,
    style: u32,
    ex_style: u32,
    id: i32,
    rect: Rect,
) -> HWND {
    let class_name = wide(class);
    let title = wide(text);
    CreateWindowExW(
        ex_style,
        class_name.as_ptr(),
        title.as_ptr(),
        WS_CHILD | WS_VISIBLE | style,
        rect.x,
        rect.y,
        rect.w.max(1),
        rect.h.max(1),
        parent,
        id as usize as *mut _,
        std::ptr::null_mut(),
        null(),
    )
}

/// DPI 变化的两阶段执行：入口由 `WM_DPICHANGED` 以实时 `GetDpiForWindow`
/// 驱动；测试以固定 DPI 直接调用同一套阶段逻辑。阶段 A 重建字体与度量
/// （仅 GDI 调用）；阶段 B 无借用做 `WM_SETFONT` 扇出与 `SetWindowPos`
/// （同步 WM_SIZE 可重入 refresh，子类化回调也可能销毁窗口）——每个
/// 允许同步回调的副作用返回后先复验实例存活，再继续；末次全量刷新
/// 由 `refresh` 自身的检查点兜底，窗口已销毁时不会再解引用指针。
unsafe fn apply_dpi_at(pointer: *mut DesktopState, window: HWND, dpi: u32, suggested: &RECT) {
    if dpi == 0 {
        return;
    }
    let captured = window_instance(window);
    let (controls, fonts) = {
        let state = &mut *pointer;
        state.dpi = dpi;
        state.metrics = theme::Metrics::new(dpi);
        rebuild_fonts(state);
        (state.controls, state.fonts)
    };
    apply_fonts(controls, fonts, window, captured);
    if !instance_valid(window, captured) {
        return;
    }
    SetWindowPos(
        window,
        std::ptr::null_mut(),
        suggested.left,
        suggested.top,
        suggested.right - suggested.left,
        suggested.bottom - suggested.top,
        SWP_NOZORDER | SWP_NOACTIVATE,
    );
    if !instance_valid(window, captured) {
        return;
    }
    refresh(pointer);
}

unsafe fn invalidate_status(pointer: *mut DesktopState) {
    let (main, area) = {
        let state = &*pointer;
        (
            state.main,
            state.layout.as_ref().map(|plan| RECT {
                left: plan.status.x,
                top: plan.status.y,
                right: plan.status.right(),
                bottom: plan.status.bottom(),
            }),
        )
    };
    if let Some(area) = area {
        InvalidateRect(main, &area, 0);
    }
}

unsafe fn invalidate_sidebar(pointer: *mut DesktopState) {
    let (main, area) = {
        let state = &*pointer;
        (
            state.main,
            state.layout.as_ref().map(|plan| RECT {
                left: plan.sidebar.x,
                top: plan.sidebar.y,
                right: plan.sidebar.right(),
                bottom: plan.sidebar.bottom(),
            }),
        )
    };
    if let Some(area) = area {
        InvalidateRect(main, &area, 0);
    }
}

unsafe fn invalidate_control_area(pointer: *mut DesktopState, id: i32) {
    let (main, area) = {
        let state = &*pointer;
        let rect = match id {
            ID_INPUT => state.layout.as_ref().map(|plan| plan.composer),
            ID_MEM_INPUT => state.panels.first().map(|panel| panel.input),
            ID_MEM_SEARCH => state.panels.first().map(|panel| panel.search),
            ID_MEM_LIST => state.panels.first().map(|panel| panel.list),
            ID_PRESET_LIST => state.panels.get(1).map(|panel| panel.list),
            ID_LOG_LIST => state.panels.get(2).map(|panel| panel.list),
            260..=263 => state
                .panels
                .get(1)
                .map(|panel| model_settings::rects(panel.list, state.dpi)[(id - 260) as usize]),
            _ => None,
        };
        (
            state.main,
            rect.map(|rect| RECT {
                left: rect.x - theme::scale(6, state.dpi),
                top: rect.y - theme::scale(6, state.dpi),
                right: rect.right() + theme::scale(6, state.dpi),
                bottom: rect.bottom() + theme::scale(6, state.dpi),
            }),
        )
    };
    if let Some(area) = area {
        InvalidateRect(main, &area, 0);
    }
}

unsafe fn control_text(control: HWND) -> String {
    let length = GetWindowTextLengthW(control);
    if length <= 0 {
        return String::new();
    }
    let mut buffer = vec![0_u16; length as usize + 1];
    let copied = GetWindowTextW(control, buffer.as_mut_ptr(), buffer.len() as i32);
    String::from_utf16_lossy(&buffer[..copied.max(0) as usize])
}

/// 把客户区尺寸换算为含边框与标题栏的窗口尺寸。
fn window_size_for(client_w: i32, client_h: i32) -> (i32, i32) {
    let mut frame = RECT {
        left: 0,
        top: 0,
        right: client_w,
        bottom: client_h,
    };
    unsafe {
        AdjustWindowRect(&mut frame, WS_OVERLAPPEDWINDOW, 0);
    }
    (frame.right - frame.left, frame.bottom - frame.top)
}

fn empty_panel_layouts() -> [layout::PanelLayout; 3] {
    fn zero() -> layout::PanelLayout {
        layout::PanelLayout {
            title: Rect::zero(),
            search: Rect::zero(),
            list: Rect::zero(),
            detail: Rect::zero(),
            input: Rect::zero(),
            actions: Vec::new(),
        }
    }
    [zero(), zero(), zero()]
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

/// GUI 子系统下启动失败无法写控制台；以消息框尽力呈现一次。
pub fn report_startup_failure(message: &str) {
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            wide(message).as_ptr(),
            wide(WINDOW_TITLE).as_ptr(),
            MB_ICONWARNING,
        );
    }
}
