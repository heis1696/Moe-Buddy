//! 原生宿主回归：在真实窗口类与真实 Win32 控件上验证两阶段窗口过程
//! 的重入安全与交互语义。确认框以脚本化桩替换（不走会阻塞的模态框），
//! 但桩内继续显式投递并泵队列消息，模拟真实模态循环的重入；子类化
//! 控件过程注入"同步回调期间的撤权/嵌套刷新/销毁"，驱动实例存活与
//! 计划修订检查点。业务权限未开放：测试只经宿主既有 `Action` 通道
//! 驱动 `ChatApplication`。

use super::*;

#[test]
fn chat_navigation_preserves_drafts_and_empty_memory_is_readable() {
    unsafe {
        let mut window = make_window();
        let main = window.main;
        let controls = window.state().controls;
        type_text(controls.input, "未发送的聊天");
        SendMessageW(main, WM_COMMAND, command(TAB_IDS[0], BN_CLICKED), 0);
        assert_eq!(
            IsWindowVisible(controls.mem_list),
            0,
            "empty list must not cover the painted guidance"
        );
        type_text(controls.mem_input, "待保存的记忆");
        SetFocus(controls.mem_input);
        let drawer_width = window.state().layout.unwrap().canvas.w;
        SendMessageW(main, WM_COMMAND, command(ID_CHAT_TAB, BN_CLICKED), 0);
        assert_eq!(window.state().view.active_tab, None);
        assert!(window.state().layout.unwrap().canvas.w > drawer_width);
        assert_eq!(control_text(controls.input), "未发送的聊天");
        assert_eq!(control_text(controls.mem_input), "待保存的记忆");
        assert_eq!(IsWindowVisible(controls.mem_input), 0);
        assert_eq!(
            GetFocus(),
            controls.input,
            "hidden drawer does not retain focus"
        );
        // Navigation remains a native tab stop, not a mouse-only painted region.
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            GWL_STYLE, GetNextDlgTabItem, GetWindowLongW,
        };
        assert_ne!(
            GetWindowLongW(controls.chat_tab, GWL_STYLE) as u32 & WS_TABSTOP,
            0
        );
        assert_eq!(
            GetNextDlgTabItem(main, std::ptr::null_mut(), 0),
            controls.chat_tab
        );
        SendMessageW(main, WM_COMMAND, command(TAB_IDS[0], BN_CLICKED), 0);
        assert_eq!(control_text(controls.mem_input), "待保存的记忆");
        SendMessageW(main, WM_COMMAND, command(ID_MEM_REMEMBER, BN_CLICKED), 0);
        assert_ne!(
            IsWindowVisible(controls.mem_list),
            0,
            "saved row becomes selectable"
        );
        assert_eq!(window.state().app.snapshot().memories.len(), 1);
    }
}

#[test]
fn model_form_permission_apply_and_note_fit_with_notice_at_each_dpi() {
    for dpi in [96, 120, 144, 192] {
        let m = theme::Metrics::new(dpi);
        for notice in [false, true] {
            let client =
                layout::compute_with_drawer(m.min_client_w, m.min_client_h, notice, true, &m);
            let panel = layout::panel_children(client.panel, &m, 1, false);
            let fields = model_settings::rects(panel.list, dpi);
            for pair in fields.windows(2) {
                assert!(pair[0].bottom() < pair[1].y);
            }
            // Every editable field has space for its visible label and border.
            for pair in fields[..4].windows(2) {
                assert!(
                    pair[0].bottom() + theme::scale(4, dpi) <= pair[1].y - theme::scale(26, dpi)
                );
            }
            assert!(fields[5].bottom() + theme::scale(80, dpi) <= client.panel.bottom());
        }
    }
}

#[test]
fn height_only_resize_keeps_chat_at_bottom_and_preserves_scrolled_reading() {
    unsafe {
        let mut window = make_window();
        let main = window.main;
        let input = window.state().controls.input;
        type_text(input, &"一行测试消息\r\n".repeat(35));
        SendMessageW(main, WM_COMMAND, command(ID_SEND, BN_CLICKED), 0);
        let canvas = window.state().canvas;
        SendMessageW(canvas, WM_VSCROLL, SB_BOTTOM as WPARAM, 0);
        let m = window.state().metrics;
        let (width, height) = window_size_for(m.initial_client_w, m.min_client_h);
        SetWindowPos(
            main,
            std::ptr::null_mut(),
            0,
            0,
            width,
            height,
            SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
        {
            let state = window.state();
            let content = state.canvas_view.layout.as_ref().unwrap().content_height;
            let viewport = state.layout.unwrap().canvas.h;
            assert!(content > viewport);
            assert_eq!(state.canvas_view.scroll, content - viewport);
        }
        SendMessageW(canvas, WM_VSCROLL, SB_TOP as WPARAM, 0);
        let (width, height) = window_size_for(m.initial_client_w, m.initial_client_h);
        SetWindowPos(
            main,
            std::ptr::null_mut(),
            0,
            0,
            width,
            height,
            SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
        assert_eq!(window.state().canvas_view.scroll, 0);
    }
}

#[test]
fn asynchronous_auth_failure_restores_input_and_error_severity() {
    unsafe {
        let mut window = make_window();
        let main = window.main;
        window
            .state()
            .app
            .configure_model(&moe_chat_app::ModelSettings {
                endpoint: "http://127.0.0.1:9/chat/completions".into(),
                model: "synthetic".into(),
                persona: "synthetic persona".into(),
                allow_transmission: true,
            })
            .unwrap();
        let state = window.state();
        state
            .app
            .dispatch(Action::SetDraft("retry this question".into()), 1)
            .unwrap();
        state.app.dispatch(Action::Send, 2).unwrap();
        state.app.take_external_request().unwrap();
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        state.model_worker = Some(receiver);
        sender
            .send(crate::transport::Reply {
                outcome: moe_agent_runtime::ModelOutcome::Failed(
                    moe_agent_runtime::ModelFailure::Unavailable,
                ),
                message: "认证失败，请检查 API Key 与模型访问权限",
            })
            .unwrap();
        let pointer = GetWindowLongPtrW(main, GWLP_USERDATA) as *mut DesktopState;
        model_settings::poll(pointer);
        assert_eq!(
            control_text(window.state().controls.input),
            "retry this question"
        );
        assert_eq!(
            window.state().view.error.as_ref().unwrap().1,
            Severity::Error
        );
    }
}

#[test]
fn memory_editor_search_save_delete_and_unsaved_close() {
    unsafe {
        let mut window = make_window();
        let main = window.main;
        let full_width = window.state().layout.unwrap().canvas.w;
        SendMessageW(main, WM_COMMAND, command(TAB_IDS[0], BN_CLICKED), 0);
        assert!(window.state().layout.unwrap().canvas.w < full_width);
        let controls = window.state().controls;
        SetWindowTextW(controls.mem_input, wide("喜欢红茶").as_ptr());
        SendMessageW(main, WM_COMMAND, command(ID_MEM_REMEMBER, BN_CLICKED), 0);
        SendMessageW(controls.mem_list, LB_SETCURSEL, 0, 0);
        SendMessageW(main, WM_COMMAND, command(ID_MEM_LIST, LBN_SELCHANGE), 0);
        SendMessageW(main, WM_COMMAND, command(ID_MEM_EDIT, BN_CLICKED), 0);
        assert_eq!(control_text(controls.mem_input), "喜欢红茶");
        SetWindowTextW(controls.mem_input, wide("喜欢绿茶").as_ptr());
        SendMessageW(main, WM_COMMAND, command(ID_MEM_REMEMBER, BN_CLICKED), 0);
        let notes = window.state().app.snapshot().memories;
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].text, "喜欢绿茶");
        assert_eq!(notes[0].revision, 2);
        type_text(controls.mem_search, "不存在的词");
        assert_eq!(SendMessageW(controls.mem_list, LB_GETCOUNT, 0, 0), 0);
        SetWindowTextW(controls.mem_search, wide("").as_ptr());
        SendMessageW(main, WM_COMMAND, command(ID_MEM_SEARCH, EN_CHANGE), 0);
        assert_eq!(SendMessageW(controls.mem_list, LB_GETCOUNT, 0, 0), 1);
        SendMessageW(controls.mem_list, LB_SETCURSEL, 0, 0);
        SendMessageW(main, WM_COMMAND, command(ID_MEM_LIST, LBN_SELCHANGE), 0);
        push_confirm(IDNO);
        SendMessageW(main, WM_COMMAND, command(ID_MEM_DELETE, BN_CLICKED), 0);
        assert_eq!(window.state().app.snapshot().memories.len(), 1);
        push_confirm(IDYES);
        SendMessageW(main, WM_COMMAND, command(ID_MEM_DELETE, BN_CLICKED), 0);
        assert!(window.state().app.snapshot().memories.is_empty());
        SetWindowTextW(controls.mem_input, wide("尚未保存").as_ptr());
        push_confirm(IDNO);
        SendMessageW(main, WM_CLOSE, 0, 0);
        assert!(window.alive());
        assert_eq!(control_text(controls.mem_input), "尚未保存");
    }
}
use std::cell::RefCell;
use std::collections::VecDeque;
use std::sync::Once;
use std::sync::atomic::{AtomicBool, Ordering};

use windows_sys::Win32::Graphics::Gdi::{
    CreateFontIndirectW, GetBkColor, GetObjectW, GetTextColor, LOGFONTW,
};
use windows_sys::Win32::UI::Controls::{EM_CANUNDO, EM_UNDO, ODS_DISABLED, ODT_BUTTON};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetFocus, IsWindowEnabled};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallWindowProcW, GWLP_WNDPROC, GetWindowRect, IDNO, IsWindowVisible, LB_GETCOUNT, LB_SETCURSEL,
    PM_REMOVE, PeekMessageW, PostMessageW, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOZORDER, WM_CHAR,
    WM_ENABLE, WM_QUIT, WNDPROC,
};

thread_local! {
    static SCRIPTED_CONFIRM: RefCell<VecDeque<i32>> = const { RefCell::new(VecDeque::new()) };
}

/// 确认桩：弹出脚本答案后继续分发队列消息，模拟真实 `MessageBoxW`
/// 模态循环期间的重入——此时绝不允许任何存活的 `DesktopState` 借用。
unsafe fn scripted_confirm(_window: HWND, _title: &str, _text: &str) -> i32 {
    let answer = SCRIPTED_CONFIRM
        .with(|queue| queue.borrow_mut().pop_front())
        .unwrap_or(IDNO);
    let _ = pump(64);
    answer
}

struct TestWindow {
    pointer: *mut DesktopState,
    main: HWND,
}

impl TestWindow {
    /// 借用状态前必须确保没有消息在分发中（测试均为顺序访问）；
    /// 销毁后的指针由 `alive()` 检查拦截。
    unsafe fn state(&mut self) -> &mut DesktopState {
        &mut *self.pointer
    }

    fn alive(&self) -> bool {
        unsafe { !self.main.is_null() && IsWindow(self.main) != 0 }
    }
}

impl Drop for TestWindow {
    fn drop(&mut self) {
        // 已销毁的窗口其状态 Box 已由 WM_NCDESTROY 释放，不得再触碰。
        if self.alive() {
            unsafe { DestroyWindow(self.main) };
        }
    }
}

fn ensure_classes() {
    static ONCE: Once = Once::new();
    static OK: AtomicBool = AtomicBool::new(false);
    ONCE.call_once(|| {
        let instance = unsafe { GetModuleHandleW(null()) };
        let ok = !instance.is_null() && unsafe { register_classes(instance) };
        OK.store(ok, Ordering::Release);
    });
    assert!(OK.load(Ordering::Acquire), "window classes must register");
}

/// 创建与 `run()` 相同布局的真实窗口（不可见，避免测试闪烁）；
/// WM_CREATE 走完整 create_stage：资源、控件、字体扇出与首轮刷新。
fn make_window() -> TestWindow {
    make_window_with(scripted_confirm)
}

fn make_window_with(confirm: ConfirmDialog) -> TestWindow {
    ensure_classes();
    let instance = unsafe { GetModuleHandleW(null()) };
    assert!(!instance.is_null(), "module handle");
    let dpi = unsafe { GetDpiForSystem() };
    let generation = process_generation().expect("process generation");
    let window_instance_id = process_generation().expect("window instance generation");
    let mut application = ChatApplication::new(generation).expect("chat application");
    application
        .dispatch(Action::Open(PanelKind::Chat), 0)
        .expect("open chat panel");
    let snapshot = application.snapshot();
    let state = Box::new(DesktopState {
        app: application,
        model_key: String::new(),
        initial_settings: None,
        model_worker: None,
        clock: Instant::now(),
        epoch_ms: 0,
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
        confirm,
    });
    let pointer = Box::into_raw(state);
    let metrics = unsafe { (*pointer).metrics };
    let (min_w, min_h) = window_size_for(metrics.min_client_w, metrics.min_client_h);
    let (init_w, init_h) = window_size_for(metrics.initial_client_w, metrics.initial_client_h);
    let window = unsafe {
        CreateWindowExW(
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
            pointer.cast(),
        )
    };
    assert!(!window.is_null(), "test window must create");
    // 生产窗口可见；EDIT 控件只在可见窗口上对 SetWindowTextW 回发
    // EN_CHANGE，IsWindowVisible 也检查整条父链，因此测试窗口同样可见。
    unsafe { ShowWindow(window, SW_SHOWNA) };
    TestWindow {
        pointer,
        main: window,
    }
}

/// 逐字符发送 `WM_CHAR`，走真实 EDIT 过程的“用户编辑”路径：
/// 记录撤销缓冲并同步回发 EN_CHANGE（程序化 WM_SETTEXT 不记录撤销）。
unsafe fn type_text(control: HWND, text: &str) {
    for unit in text.encode_utf16() {
        SendMessageW(control, WM_CHAR, unit as usize, 0);
    }
}

/// 泵当前线程队列；`WM_QUIT` 到达即停（销毁路径会 `PostQuitMessage`）。
unsafe fn pump(max_messages: usize) -> usize {
    let mut message: MSG = unsafe { std::mem::zeroed() };
    let mut handled = 0;
    while handled < max_messages
        && PeekMessageW(&mut message, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0
    {
        if message.message == WM_QUIT {
            break;
        }
        TranslateMessage(&message);
        DispatchMessageW(&message);
        handled += 1;
    }
    handled
}

fn command(id: i32, notification: u32) -> WPARAM {
    ((notification as usize) << 16) | ((id as usize) & 0xFFFF)
}

fn push_confirm(answer: i32) {
    SCRIPTED_CONFIRM.with(|queue| queue.borrow_mut().push_back(answer));
}

/// 子类注入脚本：真实系统不会在阶段 B 的 `EnableWindow`/`WM_SETFONT`/
/// `SetWindowPos` 同步回调里撤权或销毁窗口，测试用子类化控件过程
/// 复现这类重入，驱动实例存活与计划修订检查点。
enum SubclassAction {
    /// 撤权并嵌套一次全量刷新（复现审查员注入的重入撤权路径）。
    RevokeAndRefresh,
    /// 只撤权不刷新：旧计划必须在下一个检查点中止且不回写过期草稿，
    /// 界面收敛由后续刷新负责（生产撤权路径总是自带刷新）。
    RevokeOnly,
    /// 同步销毁主窗口（复现回调期间窗口销毁）。
    DestroyOwner,
}

thread_local! {
    /// (目标控件 HWND, 拦截消息, 动作)；单发，触发即移除。
    static SUBCLASS_ARMS: RefCell<Vec<(isize, u32, SubclassAction)>> =
        const { RefCell::new(Vec::new()) };
    /// (主窗口 HWND, 原窗口过程)；主窗口的 GWLP_USERDATA 存放状态指针，
    /// 原过程必须另存。
    static MAIN_SUBCLASS: RefCell<Vec<(isize, isize)>> = const { RefCell::new(Vec::new()) };
    /// (主窗口 HWND, 拦截消息, 动作)；单发。
    static MAIN_ARMS: RefCell<Vec<(isize, u32, SubclassAction)>> =
        const { RefCell::new(Vec::new()) };
}

/// 子类化一个控件：原过程存入控件自身的 GWLP_USERDATA（标准控件不
/// 使用该槽位），拦截过程在命中的消息上执行注入脚本后按需转发。
unsafe fn arm_subclass(control: HWND, message: u32, action: SubclassAction) {
    SUBCLASS_ARMS.with(|arms| arms.borrow_mut().push((control as isize, message, action)));
    let previous = SetWindowLongPtrW(control, GWLP_WNDPROC, injecting_proc as *const () as isize);
    assert_ne!(previous, 0, "subclass must capture a valid original proc");
    SetWindowLongPtrW(control, GWLP_USERDATA, previous);
}

unsafe extern "system" fn injecting_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let action = SUBCLASS_ARMS.with(|arms| {
        let mut arms = arms.borrow_mut();
        arms.iter()
            .position(|(target, armed, _)| *target == window as isize && *armed == message)
            .map(|position| arms.remove(position).2)
    });
    if let Some(action) = action {
        let main = windows_sys::Win32::UI::WindowsAndMessaging::GetParent(window);
        match action {
            SubclassAction::RevokeAndRefresh => {
                let pointer = state_pointer_of(main);
                if !pointer.is_null() {
                    act_at(pointer, Action::Revoke);
                    refresh(pointer);
                }
            }
            SubclassAction::RevokeOnly => {
                let pointer = state_pointer_of(main);
                if !pointer.is_null() {
                    act_at(pointer, Action::Revoke);
                }
            }
            SubclassAction::DestroyOwner => {
                DestroyWindow(main);
            }
        }
        // 销毁注入可能连带销毁了本控件；窗口已失效时不得再转发。
        if IsWindow(window) == 0 {
            return 0;
        }
    }
    let original = GetWindowLongPtrW(window, GWLP_USERDATA);
    if original != 0 {
        let original: WNDPROC = std::mem::transmute(original);
        return CallWindowProcW(original, window, message, wparam, lparam);
    }
    DefWindowProcW(window, message, wparam, lparam)
}

/// 子类化主窗口（原过程另存 thread_local，不动 GWLP_USERDATA）。
unsafe fn arm_main_subclass(main: HWND, message: u32, action: SubclassAction) {
    MAIN_ARMS.with(|arms| arms.borrow_mut().push((main as isize, message, action)));
    let previous = SetWindowLongPtrW(
        main,
        GWLP_WNDPROC,
        injecting_main_proc as *const () as isize,
    );
    assert_ne!(previous, 0, "main subclass must capture original proc");
    MAIN_SUBCLASS.with(|stored| {
        stored
            .borrow_mut()
            .retain(|(hwnd, _)| *hwnd != main as isize);
        stored.borrow_mut().push((main as isize, previous));
    });
}

unsafe extern "system" fn injecting_main_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let action = MAIN_ARMS.with(|arms| {
        let mut arms = arms.borrow_mut();
        arms.iter()
            .position(|(target, armed, _)| *target == window as isize && *armed == message)
            .map(|position| arms.remove(position).2)
    });
    if let Some(SubclassAction::DestroyOwner) = action {
        DestroyWindow(window);
        if IsWindow(window) == 0 {
            return 0;
        }
    }
    let original = MAIN_SUBCLASS.with(|stored| {
        stored
            .borrow()
            .iter()
            .find(|(hwnd, _)| *hwnd == window as isize)
            .map(|(_, proc)| *proc)
    });
    if original.is_some_and(|proc| proc != 0) {
        let original: WNDPROC = std::mem::transmute(original.unwrap());
        return CallWindowProcW(original, window, message, wparam, lparam);
    }
    DefWindowProcW(window, message, wparam, lparam)
}

/// P1 回归：EN_CHANGE 的同步重入。真实键入（WM_CHAR）由 EDIT 过程
/// 同步回发 EN_CHANGE，重入的 WM_COMMAND 必须在外层无借用时处理；
/// 程序化 SetWindowTextW 不产生通知（实测），草稿保持权威、刷新收敛，
/// 且收敛后不产生回写循环。
#[test]
fn en_change_reentry_and_set_text_draft_converge() {
    unsafe {
        let mut window = make_window();
        let input = window.state().controls.input;
        let mem_input = window.state().controls.mem_input;

        // 同步重入：每个字符的 EN_CHANGE 在 WM_CHAR 内处理完毕。
        type_text(input, "你好，世界");
        assert_eq!(control_text(input), "你好，世界");
        let draft = window.state().app.snapshot().chat.draft;
        assert_eq!(
            draft, "你好，世界",
            "synchronous EN_CHANGE synced the draft"
        );

        // 程序化写入：本机 Windows 的 EDIT 对 WM_SETTEXT 不回发
        // EN_CHANGE（实测同步与排队均无）；草稿保持权威，下一次刷新
        // 把控件文本收敛回草稿，且不产生回写循环。
        SetWindowTextW(input, wide("程序写入").as_ptr());
        let _ = pump(16);
        assert_eq!(control_text(input), "程序写入");
        let draft = window.state().app.snapshot().chat.draft;
        assert_eq!(
            draft, "你好，世界",
            "draft keeps authority without EN_CHANGE"
        );
        refresh(window.pointer);
        assert_eq!(
            control_text(input),
            "你好，世界",
            "refresh converges text to draft"
        );

        // 记忆输入的 EN_CHANGE 不属于聊天输入，不影响草稿。
        type_text(mem_input, "备忘");
        let draft = window.state().app.snapshot().chat.draft;
        assert_eq!(draft, "你好，世界");

        // 已收敛的文本不再回写，不产生通知循环。
        refresh(window.pointer);
        assert_eq!(control_text(input), "你好，世界");
    }
}

/// 聊天基本操作：点“发送”走既有 Action 通道，草稿清空、消息出现。
#[test]
fn send_button_click_delivers_offline_reply() {
    unsafe {
        let mut window = make_window();
        let main = window.main;
        let input = window.state().controls.input;
        SetWindowTextW(input, wide("你好").as_ptr());
        SendMessageW(
            main,
            WM_COMMAND,
            command(ID_SEND, BN_CLICKED),
            input as LPARAM,
        );
        let snapshot = window.state().app.snapshot();
        assert!(
            !snapshot.chat.messages.is_empty(),
            "user message recorded; offline reply must follow"
        );
        assert_eq!(snapshot.chat.messages[0].text, "你好");
        assert_eq!(snapshot.chat.draft, "", "draft cleared after send");
        assert_eq!(control_text(input), "", "input synced to empty draft");
        let _ = pump(16);
    }
}

/// 记忆面板基本操作：入口显示、记住入列、选中后详情同步全文。
#[test]
fn memory_panel_flow_remember_select_detail() {
    unsafe {
        let mut window = make_window();
        let main = window.main;
        SendMessageW(main, WM_COMMAND, command(TAB_IDS[0], BN_CLICKED), 0);
        let mem_list = window.state().controls.mem_list;
        assert_eq!(
            IsWindowVisible(mem_list),
            0,
            "empty state guidance is visible"
        );

        let mem_input = window.state().controls.mem_input;
        SetWindowTextW(mem_input, wide("喜欢安静的背景音").as_ptr());
        SendMessageW(
            main,
            WM_COMMAND,
            command(ID_MEM_REMEMBER, BN_CLICKED),
            mem_input as LPARAM,
        );
        assert_eq!(
            SendMessageW(mem_list, LB_GETCOUNT, 0, 0),
            1,
            "remembered row listed"
        );
        assert_ne!(IsWindowVisible(mem_list), 0, "saved row is selectable");

        SendMessageW(mem_list, LB_SETCURSEL, 0, 0);
        SendMessageW(
            main,
            WM_COMMAND,
            command(ID_MEM_LIST, LBN_SELCHANGE),
            mem_list as LPARAM,
        );
        SendMessageW(main, WM_COMMAND, command(ID_MEM_EDIT, BN_CLICKED), 0);
        assert_eq!(control_text(mem_input), "喜欢安静的背景音");
        let _ = pump(16);
    }
}

/// 预设面板基本操作：选中未激活项启用“使用此预设”，应用后切换绑定、
/// 激活行不可重复应用。
#[test]
fn preset_flow_select_enables_apply_and_applies() {
    unsafe {
        let mut window = make_window();
        let main = window.main;
        SendMessageW(main, WM_COMMAND, command(TAB_IDS[1], BN_CLICKED), 0);
        let preset_list = window.state().controls.preset_list;
        assert!(IsWindowVisible(preset_list) != 0, "preset entry shows list");
        let rows = window.state().view.presets.rows.clone();
        assert!(rows.len() >= 2, "offline catalog provides multiple presets");
        let apply = window.state().controls.preset_apply;
        assert_eq!(
            IsWindowEnabled(apply),
            0,
            "apply disabled without selection"
        );

        let target = rows
            .iter()
            .position(|row| !row.active)
            .expect("a non-active preset");
        SendMessageW(preset_list, LB_SETCURSEL, target, 0);
        SendMessageW(
            main,
            WM_COMMAND,
            command(ID_PRESET_LIST, LBN_SELCHANGE),
            preset_list as LPARAM,
        );
        assert!(
            IsWindowEnabled(apply) != 0,
            "apply enables after selecting inactive preset"
        );

        let expected = rows[target].id.clone();
        SendMessageW(
            main,
            WM_COMMAND,
            command(ID_PRESET_APPLY, BN_CLICKED),
            apply as LPARAM,
        );
        let snapshot = window.state().app.snapshot();
        assert_eq!(snapshot.active_preset, expected, "preset binding applied");

        let active_index = window
            .state()
            .view
            .presets
            .rows
            .iter()
            .position(|row| row.id == expected)
            .expect("applied preset stays listed");
        SendMessageW(preset_list, LB_SETCURSEL, active_index, 0);
        SendMessageW(
            main,
            WM_COMMAND,
            command(ID_PRESET_LIST, LBN_SELCHANGE),
            preset_list as LPARAM,
        );
        assert_eq!(IsWindowEnabled(apply), 0, "active preset cannot re-apply");
        let _ = pump(16);
    }
}

/// 无草稿关闭：无需确认直接销毁；销毁后迟到消息不得触碰已释放状态。
#[test]
fn close_with_empty_draft_destroys_directly() {
    unsafe {
        let window = make_window();
        SendMessageW(window.main, WM_CLOSE, 0, 0);
        assert_eq!(IsWindow(window.main), 0, "window destroyed without confirm");
        let _ = pump(16);
    }
}

/// 关闭确认嵌套循环：取消保留窗口与草稿；确认丢弃后销毁。确认桩内
/// 泵队列消息模拟模态循环重入。
#[test]
fn close_confirm_idno_keeps_draft_then_idyes_destroys() {
    unsafe {
        let mut window = make_window();
        let main = window.main;
        let input = window.state().controls.input;
        type_text(input, "未发送草稿");

        push_confirm(IDNO);
        SendMessageW(main, WM_CLOSE, 0, 0);
        assert!(IsWindow(main) != 0, "cancel keeps window");
        assert_eq!(control_text(input), "未发送草稿", "cancel keeps draft");

        push_confirm(IDYES);
        SendMessageW(main, WM_CLOSE, 0, 0);
        assert_eq!(IsWindow(main), 0, "confirmed close destroys window");
        let _ = pump(16);
    }
}

/// 模态期间显式投递：确认桩在模态循环内向主窗口 `PostMessageW` 一条
/// 真实命令（打开记忆页），验证模态重入由各自的短借用与计划检查点
/// 安全处理，且外层关闭流程在模态返回后继续生效。
unsafe fn reentrant_modal_confirm(window: HWND, _title: &str, _text: &str) -> i32 {
    let answer = SCRIPTED_CONFIRM
        .with(|queue| queue.borrow_mut().pop_front())
        .unwrap_or(IDNO);
    // 显式投递（而非仅泵既有队列）：模拟真实模态循环里系统/用户的
    // 并发输入到达。
    PostMessageW(window, WM_COMMAND, command(TAB_IDS[0], BN_CLICKED), 0);
    let _ = pump(64);
    answer
}

#[test]
fn modal_reentry_delivers_command_and_keeps_window() {
    unsafe {
        let mut window = make_window_with(reentrant_modal_confirm);
        let main = window.main;
        let input = window.state().controls.input;
        type_text(input, "未发送草稿");

        push_confirm(IDNO);
        SendMessageW(main, WM_CLOSE, 0, 0);

        assert!(IsWindow(main) != 0, "cancel keeps window");
        assert_eq!(control_text(input), "未发送草稿", "cancel keeps draft");
        assert_eq!(
            window.state().view.active_tab,
            Some(SideTab::Memory),
            "command posted during modal was really delivered"
        );
        assert_eq!(
            window_instance(main),
            Some(window.state().instance),
            "window instance stays registered across modal reentry"
        );
        let _ = pump(16);
    }
}

/// 模态期间销毁：确认桩在模态循环内投递 WM_CLOSE（空草稿直接销毁）。
/// 模态返回后实例复验失败，撤销流程立即放弃，不得再触碰已释放状态。
unsafe fn destroy_during_modal_confirm(window: HWND, _title: &str, _text: &str) -> i32 {
    PostMessageW(window, WM_CLOSE, 0, 0);
    let _ = pump(64);
    IDYES
}

#[test]
fn destroy_during_modal_confirm_aborts_revoke() {
    unsafe {
        let mut window = make_window_with(destroy_during_modal_confirm);
        let main = window.main;
        let revoke = window.state().controls.revoke;
        SendMessageW(
            main,
            WM_COMMAND,
            command(ID_REVOKE, BN_CLICKED),
            revoke as LPARAM,
        );

        assert_eq!(IsWindow(main), 0, "window destroyed inside modal");
        assert_eq!(
            window_instance(main),
            None,
            "destroyed instance is unregistered"
        );
        let _ = pump(16);
    }
}

/// 撤权回归：两个 EDIT 的 `WM_GETTEXT` 为空且撤销无法恢复；全部控件
/// 禁用；焦点不再进入聊天输入；撤权后关闭直接销毁。
#[test]
fn revoke_clears_text_undo_and_disables_everything() {
    unsafe {
        let mut window = make_window();
        let main = window.main;
        let input = window.state().controls.input;
        let mem_input = window.state().controls.mem_input;

        // 以真实键入路径填入内容：撤销缓冲由 EDIT 的用户编辑记录。
        type_text(input, "会撤销的草稿");
        SendMessageW(main, WM_COMMAND, command(TAB_IDS[0], BN_CLICKED), 0);
        type_text(mem_input, "会撤销的记忆");
        assert_ne!(
            SendMessageW(input, EM_CANUNDO, 0, 0),
            0,
            "edit has undo history"
        );
        assert_ne!(SendMessageW(mem_input, EM_CANUNDO, 0, 0), 0);

        push_confirm(IDYES);
        let revoke = window.state().controls.revoke;
        SendMessageW(
            main,
            WM_COMMAND,
            command(ID_REVOKE, BN_CLICKED),
            revoke as LPARAM,
        );

        assert_eq!(
            control_text(input),
            "",
            "chat edit reads empty via WM_GETTEXT"
        );
        assert_eq!(
            control_text(mem_input),
            "",
            "memory edit reads empty via WM_GETTEXT"
        );
        assert_eq!(
            SendMessageW(input, EM_CANUNDO, 0, 0),
            0,
            "chat undo buffer emptied"
        );
        assert_eq!(
            SendMessageW(mem_input, EM_CANUNDO, 0, 0),
            0,
            "memory undo buffer emptied"
        );
        assert_eq!(SendMessageW(input, EM_UNDO, 0, 0), 0, "undo cannot restore");
        assert_eq!(control_text(input), "");

        assert_eq!(IsWindowEnabled(input), 0, "chat input disabled");
        assert_eq!(
            IsWindowEnabled(window.state().controls.send),
            0,
            "send disabled"
        );
        assert_eq!(IsWindowEnabled(revoke), 0, "revoke disabled");
        for tab in window.state().controls.tabs {
            assert_eq!(IsWindowEnabled(tab), 0, "tabs disabled");
        }
        assert_eq!(IsWindowEnabled(window.state().controls.chat_tab), 0);

        SendMessageW(main, WM_SETFOCUS, 0, 0);
        assert_ne!(GetFocus(), input, "focus never re-enters chat input");

        let snapshot = window.state().app.snapshot();
        assert!(snapshot.revoked);
        assert_eq!(snapshot.chat.draft, "");

        SendMessageW(main, WM_CLOSE, 0, 0);
        assert_eq!(IsWindow(main), 0, "close after revoke destroys directly");
        let _ = pump(16);
    }
}

/// 阻塞项回归（阶段 B 计划修订）：外层刷新的 `EnableWindow` 同步重入
/// 子类化控件过程，注入"撤权 + 嵌套全量刷新"。旧计划必须在下一个
/// 检查点立即中止：不再重新启用任何控件，更不把阶段 A 捕获的过期
/// 草稿回写进已清空的输入框（修复前实测 revoked=true、
/// text="sensitive draft"、send_enabled=1）。
#[test]
fn wm_enable_nested_revocation_stops_stale_plan() {
    unsafe {
        let mut window = make_window();
        let main = window.main;
        let input = window.state().controls.input;
        let mem_input = window.state().controls.mem_input;

        // 真实键入建立草稿，再用程序写入制造"控件文本 ≠ 草稿"，
        // 迫使外层计划携带 draft=Some("敏感草稿")（SetWindowTextW 不
        // 回发 EN_CHANGE 是本机实测的 OS 行为）。
        type_text(input, "敏感草稿");
        SetWindowTextW(input, wide("x").as_ptr());
        // 夹具：预先禁用记忆输入，使外层计划的 EnableWindow(mem_input,1)
        // 成为真实状态变化，从而同步回发 WM_ENABLE 给子类过程。
        EnableWindow(mem_input, 0);
        arm_subclass(mem_input, WM_ENABLE, SubclassAction::RevokeAndRefresh);

        SendMessageW(main, WM_COMMAND, command(TAB_IDS[0], BN_CLICKED), 0);

        let snapshot = window.state().app.snapshot();
        assert!(snapshot.revoked, "nested reentry revoked the session");
        assert_eq!(
            control_text(input),
            "",
            "nested refresh cleared the input; stale plan must not write the draft back"
        );
        assert_eq!(
            IsWindowEnabled(window.state().controls.send),
            0,
            "send stays disabled after nested revocation"
        );
        assert_eq!(
            IsWindowVisible(window.state().controls.mem_list),
            0,
            "memory panel stays hidden after nested revocation"
        );
        assert_eq!(
            window_instance(main),
            Some(window.state().instance),
            "window instance stays registered"
        );
        let _ = pump(16);
    }
}

/// 嵌套结构性变更使旧计划中止：注入只撤权不刷新。旧计划在检查点
/// 中止后，其剩余步骤（含过期草稿回写）不再执行；随后一次显式刷新
/// 把界面收敛到撤权语义。
#[test]
fn nested_structural_change_aborts_old_plan_before_writeback() {
    unsafe {
        let mut window = make_window();
        let main = window.main;
        let input = window.state().controls.input;
        let mem_input = window.state().controls.mem_input;

        type_text(input, "敏感草稿");
        SetWindowTextW(input, wide("x").as_ptr());
        EnableWindow(mem_input, 0);
        arm_subclass(mem_input, WM_ENABLE, SubclassAction::RevokeOnly);

        SendMessageW(main, WM_COMMAND, command(TAB_IDS[0], BN_CLICKED), 0);

        assert!(window.state().app.snapshot().revoked);
        assert_eq!(
            control_text(input),
            "x",
            "old plan aborted: stale draft never written back"
        );
        assert_eq!(
            window_instance(main),
            Some(window.state().instance),
            "window instance stays registered"
        );

        // 生产撤权路径总是自带刷新；这里显式补一次验证最终收敛。
        refresh(window.pointer);
        assert_eq!(
            control_text(input),
            "",
            "later refresh converges to revoked"
        );
        assert_eq!(
            IsWindowEnabled(window.state().controls.send),
            0,
            "later refresh disables send"
        );
        let _ = pump(16);
    }
}

/// 阻塞项回归（实例存活①：字体扇出）：`WM_SETFONT` 同步重入子类化
/// 按钮并销毁主窗口。扇出返回后必须先复验实例存活并立即停止，
/// 不得再对已释放状态指针执行刷新。
#[test]
fn wm_setfont_destroy_during_fanout_stops_refresh() {
    unsafe {
        let mut window = make_window();
        let main = window.main;
        let send = window.state().controls.send;
        arm_subclass(send, WM_SETFONT, SubclassAction::DestroyOwner);

        let suggested = RECT {
            left: 40,
            top: 40,
            right: 1440,
            bottom: 940,
        };
        apply_dpi_at(window.pointer, main, 144, &suggested);

        assert_eq!(IsWindow(main), 0, "window destroyed inside WM_SETFONT");
        assert_eq!(
            window_instance(main),
            None,
            "destroyed instance is unregistered"
        );
        let _ = pump(16);
    }
}

/// 阻塞项回归（实例存活①：布局副作用）：`SetWindowPos` 同步派发的
/// WM_SIZE 重入主窗口过程并销毁窗口。`SetWindowPos` 返回后必须复验
/// 实例存活并立即停止，不得再解引用已释放指针。
#[test]
fn set_window_pos_destroy_during_resize_stops_refresh() {
    unsafe {
        let window = make_window();
        let main = window.main;
        arm_main_subclass(main, WM_SIZE, SubclassAction::DestroyOwner);

        let suggested = RECT {
            left: 40,
            top: 40,
            right: 1440,
            bottom: 940,
        };
        apply_dpi_at(window.pointer, main, 144, &suggested);

        assert_eq!(IsWindow(main), 0, "window destroyed inside nested WM_SIZE");
        assert_eq!(
            window_instance(main),
            None,
            "destroyed instance is unregistered"
        );
        let _ = pump(16);
    }
}

/// 144/192 DPI：字体从 96 基准只缩放一次——创建出的 HFONT 逐字段等
/// 于独立构造的期望 LOGFONT（不经生产 `font()` 生成期望），状态行高
/// 必须等于用该独立字体在真实 DC 上量测的结果；布局控件随真实窗口
/// 尺寸同步移动；实时 `WM_DPICHANGED` 入口收敛到窗口实际 DPI。
#[test]
fn dpi_144_and_192_rebuild_real_fonts_once() {
    unsafe {
        let mut window = make_window();
        let main = window.main;
        let baseline_line = window.state().body_line;
        assert!(baseline_line > 0);

        let suggested = RECT {
            left: 40,
            top: 40,
            right: 1440,
            bottom: 940,
        };
        apply_dpi_at(window.pointer, main, 144, &suggested);
        {
            let state = window.state();
            assert_eq!(state.dpi, 144);
            assert_eq!(state.metrics.min_client_w, theme::scale(880, 144));
            assert_logfont_matches(state.fonts.body, 144, state.metrics.font_body, false, false);
            assert_logfont_matches(state.fonts.hero, 144, 30, true, false);
            let expected = measure_with_independent_font(main, 144, state.metrics.font_body);
            assert_eq!(
                state.body_line, expected,
                "fonts scale exactly once at 144 DPI"
            );
        }
        let input = window.state().controls.input;
        let mut rect = RECT::default();
        GetWindowRect(input, &mut rect);
        let input_w_144 = rect.right - rect.left;
        assert!(input_w_144 > 0, "input laid out after DPI change");

        let suggested192 = RECT {
            left: 40,
            top: 40,
            right: 1840,
            bottom: 1140,
        };
        apply_dpi_at(window.pointer, main, 192, &suggested192);
        {
            let state = window.state();
            assert_eq!(state.dpi, 192);
            assert_logfont_matches(state.fonts.body, 192, state.metrics.font_body, false, false);
            assert_logfont_matches(state.fonts.hero, 192, 30, true, false);
            let expected = measure_with_independent_font(main, 192, state.metrics.font_body);
            assert_eq!(
                state.body_line, expected,
                "fonts scale exactly once at 192 DPI"
            );
            assert!(
                state.body_line > baseline_line,
                "192 DPI line height exceeds 96"
            );
        }

        // 实时入口：WM_DPICHANGED 用 GetDpiForWindow 的实际值收敛。
        SendMessageW(
            main,
            WM_DPICHANGED_MSG,
            0,
            &suggested as *const RECT as LPARAM,
        );
        assert_eq!(window.state().dpi, GetDpiForWindow(main));
        assert!(window.state().body_line > 0);
        let _ = pump(16);
    }
}

/// 独立构造期望 LOGFONT：字号按 96 基准在入口做一次 DPI 缩放（负
/// 字符高度）、字重、字符集与字族按模块文档的固定值重建，不调用
/// 生产 `font()`，避免"用生产函数生成期望"的自证比较。
fn expected_logfont(dpi: u32, size_px: i32, bold: bool, mono: bool) -> LOGFONTW {
    let face: &str = if mono {
        theme::FONT_MONO
    } else {
        theme::FONT_UI
    };
    let mut face_units = [0_u16; 32];
    for (slot, unit) in face_units.iter_mut().zip(face.encode_utf16()) {
        *slot = unit;
    }
    LOGFONTW {
        lfHeight: -theme::scale(size_px, dpi),
        lfWeight: if bold {
            FW_BOLD as i32
        } else {
            FW_NORMAL as i32
        },
        lfCharSet: DEFAULT_CHARSET,
        lfOutPrecision: OUT_DEFAULT_PRECIS,
        lfClipPrecision: CLIP_DEFAULT_PRECIS,
        lfQuality: CLEARTYPE_QUALITY,
        lfPitchAndFamily: DEFAULT_PITCH,
        lfFaceName: face_units,
        ..LOGFONTW::default()
    }
}

/// 断言生产 HFONT 的实际 LOGFONT（GetObjectW 取回）逐字段等于独立
/// 期望：字号、字重与字体名是字号只缩放一次的直接证据。
unsafe fn assert_logfont_matches(font: HFONT, dpi: u32, size_px: i32, bold: bool, mono: bool) {
    let mut actual = LOGFONTW::default();
    let copied = GetObjectW(
        font as _,
        size_of::<LOGFONTW>() as i32,
        &mut actual as *mut LOGFONTW as *mut _,
    );
    assert_ne!(copied, 0, "GetObjectW must read the font's LOGFONTW");
    let expected = expected_logfont(dpi, size_px, bold, mono);
    assert_eq!(actual.lfHeight, expected.lfHeight, "scaled char height");
    assert_eq!(actual.lfWeight, expected.lfWeight, "font weight");
    assert_eq!(actual.lfCharSet, expected.lfCharSet, "charset");
    assert_eq!(actual.lfFaceName, expected.lfFaceName, "face name");
}

/// 用独立 LOGFONT 现建 HFONT（`CreateFontIndirectW`），在窗口真实 DC
/// 上量测正文行高——期望路径完全不经过生产 `font()`。
unsafe fn measure_with_independent_font(main: HWND, dpi: u32, size_px: i32) -> i32 {
    let logfont = expected_logfont(dpi, size_px, false, false);
    let reference = CreateFontIndirectW(&logfont);
    assert!(!reference.is_null(), "independent reference font");
    let hdc = GetDC(main);
    let old = SelectObject(hdc, reference as _);
    let mut metric = TEXTMETRICW::default();
    GetTextMetricsW(hdc, &mut metric);
    SelectObject(hdc, old);
    ReleaseDC(main, hdc);
    DeleteObject(reference as _);
    metric.tmHeight + metric.tmExternalLeading
}

/// 超过 65535px 的历史拖动位置：滚动条已设到 70000 时，`SB_THUMBPOSITION`
/// 与 `SB_THUMBTRACK` 两种拖动通知都必须取 `GetScrollInfo(SIF_TRACKPOS)`
/// 的完整 32 位值，而不是被 `wParam` 高 16 位截断成 4464。
///
/// 与真实鼠标拖动的区别：真实拖动时系统在按住滑块期间持续更新
/// `nTrackPos` 并连发 `SB_THUMBTRACK`；自动化无法直接注入 `nTrackPos`
/// （`SetScrollInfo` 按文档忽略 `SIF_TRACKPOS`），只能依赖"未拖动时
/// `nTrackPos` 反映 `nPos`"的本机实测行为放置位置值。本测试验证的
/// 是消息处理契约（完整 32 位读取），不是真实拖动输入流；真实拖动
/// 已在真实窗口会话中人工验证。
#[test]
fn thumb_drag_beyond_65535_keeps_full_position() {
    unsafe {
        let mut window = make_window();
        let canvas = window.state().canvas;
        {
            // 测试边界夹具：超长内容不经业务通道注入。
            let state = window.state();
            state.canvas_view.layout = Some(chatlog::HistoryLayout {
                blocks: Vec::new(),
                content_height: 100_000,
            });
            state.canvas_view.messages = Vec::new();
            state.canvas_view.sig = (0, 0, 0);
        }
        let mut rect = RECT::default();
        GetClientRect(canvas, &mut rect);
        let viewport = rect.bottom - rect.top;
        assert!(viewport > 0 && viewport < 70_000);

        let truncated = (70_000 & 0xFFFF) as usize;

        // SB_THUMBPOSITION：拖动释放。
        plant_scroll_position(canvas, viewport);
        SendMessageW(
            canvas,
            WM_VSCROLL,
            (SB_THUMBPOSITION as usize) | (truncated << 16),
            0,
        );
        assert_eq!(
            window.state().canvas_view.scroll,
            70_000,
            "full 32-bit track position for SB_THUMBPOSITION"
        );
        assert_scrollbar_position(canvas, 70_000);

        // SB_THUMBTRACK：按住滑块拖动中的连续通知（同一读取路径）。
        SendMessageW(canvas, WM_VSCROLL, SB_TOP as usize, 0);
        assert_eq!(window.state().canvas_view.scroll, 0, "reset to top");
        plant_scroll_position(canvas, viewport);
        SendMessageW(
            canvas,
            WM_VSCROLL,
            (SB_THUMBTRACK as usize) | (truncated << 16),
            0,
        );
        assert_eq!(
            window.state().canvas_view.scroll,
            70_000,
            "full 32-bit track position for SB_THUMBTRACK"
        );
        assert_scrollbar_position(canvas, 70_000);
    }
}

/// 夹具：把真实滚动条放到 70000（`SIF_POS`；`SIF_TRACKPOS` 不可由
/// `SetScrollInfo` 设置），未拖动时系统使 `nTrackPos` 反映 `nPos`。
unsafe fn plant_scroll_position(canvas: HWND, viewport: i32) {
    let info = SCROLLINFO {
        cbSize: size_of::<SCROLLINFO>() as u32,
        fMask: SIF_RANGE | SIF_POS,
        nMin: 0,
        nMax: 100_000,
        nPage: viewport.max(1) as u32,
        nPos: 70_000,
        nTrackPos: 0,
    };
    SetScrollInfo(canvas, SB_VERT, &info, 1);
}

/// 用完整 `SCROLLINFO` 读取（含 `SIF_TRACKPOS`）回验滚动条位置。
unsafe fn assert_scrollbar_position(canvas: HWND, expected: i32) {
    let mut verify = SCROLLINFO {
        cbSize: size_of::<SCROLLINFO>() as u32,
        fMask: SIF_POS | SIF_TRACKPOS,
        ..SCROLLINFO::default()
    };
    GetScrollInfo(canvas, SB_VERT, &mut verify);
    assert_eq!(
        verify.nPos, expected,
        "scrollbar reflects the full position"
    );
    assert_eq!(
        verify.nTrackPos, expected,
        "track position mirrors the planted position outside a drag"
    );
}

/// 真实缩放布局收敛：用 `SetWindowPos` 真实改变客户区（同步派发
/// WM_SIZE → refresh），布局必须来自改变后的真实客户区并与纯几何
/// `layout::compute` 的期望一致，而不是消息参数或旧几何。
#[test]
fn real_resize_recomputes_layout_from_actual_client() {
    unsafe {
        let mut window = make_window();
        let main = window.main;
        let mut rect = RECT::default();
        GetClientRect(main, &mut rect);
        let (old_w, old_h) = (rect.right - rect.left, rect.bottom - rect.top);

        let (client_w, client_h) = (980, 680);
        assert_ne!(
            (old_w, old_h),
            (client_w, client_h),
            "fixture must actually change the client area"
        );
        let (frame_w, frame_h) = window_size_for(client_w, client_h);
        SetWindowPos(
            main,
            std::ptr::null_mut(),
            0,
            0,
            frame_w,
            frame_h,
            SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
        );

        GetClientRect(main, &mut rect);
        assert_eq!(
            (rect.right - rect.left, rect.bottom - rect.top),
            (client_w, client_h),
            "client area really changed"
        );
        let state = window.state();
        let layout = state.layout.expect("layout computed after resize");
        let expected = layout::compute_with_drawer(
            client_w,
            client_h,
            state.view.notice.is_some(),
            state.view.active_tab.is_some(),
            &state.metrics,
        );
        assert_eq!(layout.canvas, expected.canvas, "canvas follows real client");
        assert_eq!(layout.input, expected.input, "input follows real client");
        assert!(layout.input.bottom() <= client_h + 1, "input inside client");
        assert!(
            layout.sidebar.right() <= client_w + 1,
            "sidebar inside client"
        );

        // 缩回最小客户区：同样的真实收敛检查。
        let (min_w, min_h) =
            window_size_for(state.metrics.min_client_w, state.metrics.min_client_h);
        SetWindowPos(
            main,
            std::ptr::null_mut(),
            0,
            0,
            min_w,
            min_h,
            SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
        GetClientRect(main, &mut rect);
        let min_client_w = rect.right - rect.left;
        let min_client_h = rect.bottom - rect.top;
        assert_eq!(
            (min_client_w, min_client_h),
            (state.metrics.min_client_w, state.metrics.min_client_h),
            "minimum client area reached"
        );
        let layout = state.layout.expect("layout after shrink");
        assert!(
            layout.canvas.w > 0 && layout.canvas.h > 0,
            "layout still valid at minimum size"
        );
        assert!(layout.sidebar.right() <= min_client_w + 1);
        let _ = pump(16);
    }
}

/// 着色与自绘路径：EDIT/列表框着色从 `wParam` 取 HDC，返回面板画刷，
/// 并把文字/背景色真实写入该 DC（用 `GetTextColor`/`GetBkColor` 回读
/// 结果，防止只核对返回值、参数被互换或忽略的回归）；自绘按钮消息
/// 返回已处理；主窗口与画布的 WM_PAINT 真实路径不 panic。
#[test]
fn coloring_and_ownerdraw_paths_return_brush_and_flag() {
    unsafe {
        let mut window = make_window();
        let main = window.main;
        let input = window.state().controls.input;
        let mem_list = window.state().controls.mem_list;
        let send = window.state().controls.send;
        let panel_brush = window.state().brushes.panel as isize;

        let hdc = GetDC(input);
        let colored = SendMessageW(main, WM_CTLCOLOREDIT, hdc as WPARAM, input as LPARAM);
        assert_eq!(colored, panel_brush, "edit coloring returns panel brush");
        assert_eq!(
            GetTextColor(hdc),
            theme::INK,
            "edit text color really applied to the provided DC"
        );
        assert_eq!(
            GetBkColor(hdc),
            theme::PANEL,
            "edit background color really applied to the provided DC"
        );
        ReleaseDC(input, hdc);

        let hdc = GetDC(mem_list);
        let colored = SendMessageW(main, WM_CTLCOLORLISTBOX, hdc as WPARAM, mem_list as LPARAM);
        assert_eq!(colored, panel_brush, "listbox coloring returns panel brush");
        assert_eq!(
            GetTextColor(hdc),
            theme::INK,
            "listbox text color really applied to the provided DC"
        );
        assert_eq!(
            GetBkColor(hdc),
            theme::PANEL,
            "listbox background color really applied to the provided DC"
        );
        ReleaseDC(mem_list, hdc);

        let mut draw = DRAWITEMSTRUCT {
            CtlType: ODT_BUTTON,
            CtlID: ID_SEND as u32,
            itemID: 0,
            itemAction: 0,
            itemState: ODS_DISABLED,
            hwndItem: send,
            hDC: GetDC(send),
            rcItem: RECT {
                left: 0,
                top: 0,
                right: 60,
                bottom: 28,
            },
            itemData: 0,
        };
        let handled = SendMessageW(
            main,
            WM_DRAWITEM,
            ID_SEND as usize,
            &mut draw as *mut DRAWITEMSTRUCT as LPARAM,
        );
        assert_eq!(handled, 1, "owner-draw button reported handled");
        ReleaseDC(send, draw.hDC);

        InvalidateRect(main, null(), 0);
        InvalidateRect(window.state().canvas, null(), 0);
        SendMessageW(main, WM_PAINT, 0, 0);
        SendMessageW(window.state().canvas, WM_PAINT, 0, 0);
        let _ = pump(16);
    }
}

/// 手工探针：把当前 DPI、字体行高、布局矩形与控件状态转储到忽略的
/// `target/desktop-host-probe/`，供人工核对（不作为自动化断言）。
#[test]
#[ignore = "manual probe: writes into ignored target/ directory"]
fn probe_dump_host_metrics_to_target() {
    unsafe {
        let mut window = make_window();
        let state = window.state();
        let mut text = String::from("moe-buddy-desktop host probe\n");
        text.push_str(&format!("dpi = {}\n", state.dpi));
        text.push_str(&format!("body_line = {}\n", state.body_line));
        text.push_str(&format!("small_line = {}\n", state.small_line));
        text.push_str(&format!(
            "min_client = {}x{}\n",
            state.metrics.min_client_w, state.metrics.min_client_h
        ));
        text.push_str(&format!("focus_ring = {}\n", state.focus_ring));
        text.push_str(&format!("active_tab = {:?}\n", state.view.active_tab));
        if let Some(layout) = state.layout.as_ref() {
            text.push_str(&format!("canvas = {:?}\n", layout.canvas));
            text.push_str(&format!("input = {:?}\n", layout.input));
            text.push_str(&format!("send = {:?}\n", layout.send));
            text.push_str(&format!("sidebar = {:?}\n", layout.sidebar));
            text.push_str(&format!("status = {:?}\n", layout.status));
            text.push_str(&format!("revoke = {:?}\n", layout.revoke));
        }
        text.push_str(&format!(
            "mem_list_items = {}\n",
            SendMessageW(state.controls.mem_list, LB_GETCOUNT, 0, 0)
        ));
        text.push_str(&format!(
            "preset_list_items = {}\n",
            SendMessageW(state.controls.preset_list, LB_GETCOUNT, 0, 0)
        ));

        let manifest = std::path::PathBuf::from(
            std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"),
        );
        let dir = manifest.join("../../target/desktop-host-probe");
        std::fs::create_dir_all(&dir).expect("create probe directory");
        std::fs::write(dir.join("host-probe.txt"), text).expect("write probe file");
    }
}
