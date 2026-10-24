//! 桌面状态查询（Win32）：光标、可见窗口、各块屏幕的工作区、全屏检测。
//! 全部只读，不移动、不修改任何其他程序的窗口。

use std::mem::size_of;

use windows::core::BOOL;
use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT};
use windows::Win32::Graphics::Dwm::{
    DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS,
};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, MonitorFromWindow, HDC, HMONITOR, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Shell::{
    SHQueryUserNotificationState, QUNS_PRESENTATION_MODE, QUNS_RUNNING_D3D_FULL_SCREEN,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClassNameW, GetCursorPos, GetForegroundWindow, GetWindow, GetWindowLongPtrW,
    GetWindowRect, GetWindowThreadProcessId, IsIconic, IsWindowVisible, IsZoomed, GWL_EXSTYLE, GWL_STYLE,
    GW_OWNER, WS_CAPTION, WS_EX_APPWINDOW, WS_EX_TOOLWINDOW, WS_EX_TRANSPARENT,
};

use crate::desktop::{Rect, Screen, WinInfo};

impl From<RECT> for Rect {
    fn from(r: RECT) -> Self {
        Rect { left: r.left, top: r.top, right: r.right, bottom: r.bottom }
    }
}

fn hwnd(id: isize) -> HWND {
    HWND(id as *mut core::ffi::c_void)
}

pub fn cursor_pos() -> Option<(i32, i32)> {
    let mut p = POINT::default();
    unsafe { GetCursorPos(&mut p) }.ok()?;
    Some((p.x, p.y))
}

const MONITORINFOF_PRIMARY: u32 = 1;

unsafe extern "system" fn collect_monitor(m: HMONITOR, _: HDC, _: *mut RECT, lparam: LPARAM) -> BOOL {
    let out = &mut *(lparam.0 as *mut Vec<Screen>);
    let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
    if GetMonitorInfoW(m, &mut mi).as_bool() {
        out.push(Screen {
            monitor: mi.rcMonitor.into(),
            work: mi.rcWork.into(),
            primary: mi.dwFlags & MONITORINFOF_PRIMARY != 0,
        });
    }
    BOOL(1)
}

/// 所有显示器（物理像素），主屏排第一个。复制模式（两块屏显示同样内容）系统只报一块。
pub fn screens() -> Vec<Screen> {
    let mut out: Vec<Screen> = Vec::new();
    let _ = unsafe { EnumDisplayMonitors(None, None, Some(collect_monitor), LPARAM(&mut out as *mut Vec<Screen> as isize)) };
    out.sort_by_key(|s| !s.primary);
    out
}

pub fn window_scale(id: isize) -> f64 {
    let dpi = unsafe { GetDpiForWindow(hwnd(id)) };
    if dpi == 0 {
        1.0
    } else {
        dpi as f64 / 96.0
    }
}

fn class_name(h: HWND) -> String {
    let mut buf = [0u16; 128];
    let n = unsafe { GetClassNameW(h, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

fn is_cloaked(h: HWND) -> bool {
    let mut cloaked: u32 = 0;
    unsafe {
        DwmGetWindowAttribute(
            h,
            DWMWA_CLOAKED,
            &mut cloaked as *mut u32 as *mut _,
            size_of::<u32>() as u32,
        )
    }
    .map(|_| cloaked != 0)
    .unwrap_or(false)
}

/// 窗口肉眼可见的边框（不含 Win10/11 的透明阴影边）。窗口没了/最小化/隐藏时返回 None。
pub fn frame_bounds(id: isize) -> Option<Rect> {
    let h = hwnd(id);
    unsafe {
        if !IsWindowVisible(h).as_bool() || IsIconic(h).as_bool() {
            return None;
        }
    }
    if is_cloaked(h) {
        return None;
    }
    let mut r = RECT::default();
    let ok = unsafe {
        DwmGetWindowAttribute(
            h,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut r as *mut RECT as *mut _,
            size_of::<RECT>() as u32,
        )
    };
    if ok.is_err() {
        unsafe { GetWindowRect(h, &mut r) }.ok()?;
    }
    let r: Rect = r.into();
    (r.width() > 0 && r.height() > 0).then_some(r)
}

/// 用户此刻正拖着的顶层窗口（系统移动/调整大小循环里，GetGUIThreadInfo 的 GUI_INMOVESIZE）。
/// idThread = 0 查前台线程，拖标题栏时鼠标捕获就在它那里。没有人在拖、拖的是 `skip`（我们自己）
/// 或者窗口不可见时返回 None。
pub fn moving_window(skip: isize) -> Option<(isize, Rect)> {
    use windows::Win32::UI::WindowsAndMessaging::{GetGUIThreadInfo, GUITHREADINFO, GUI_INMOVESIZE};
    let mut info = GUITHREADINFO { cbSize: size_of::<GUITHREADINFO>() as u32, ..Default::default() };
    unsafe { GetGUIThreadInfo(0, &mut info) }.ok()?;
    if info.flags.0 & GUI_INMOVESIZE.0 == 0 || info.hwndMoveSize.0.is_null() {
        return None;
    }
    let id = info.hwndMoveSize.0 as isize;
    if id == skip {
        return None;
    }
    frame_bounds(id).map(|r| (id, r))
}

/// 桌面、任务栏、通知区域之类：不能站，也不当遮挡物
const SHELL_CLASSES: &[&str] = &["Progman", "WorkerW", "Shell_TrayWnd", "Shell_SecondaryTrayWnd"];
/// 系统弹出层（开始菜单、搜索、托盘溢出）：会遮挡，但不能站
const NON_PLATFORM_CLASSES: &[&str] = &[
    "Windows.UI.Core.CoreWindow",
    "XamlExplorerHostIslandWindow",
    "TopLevelWindowForOverflowXamlIsland",
    "NotifyIconOverflowWindow",
];

unsafe extern "system" fn collect(h: HWND, lparam: LPARAM) -> BOOL {
    let out = &mut *(lparam.0 as *mut Vec<isize>);
    out.push(h.0 as isize);
    BOOL(1)
}

/// 所有可见的顶层窗口，按 Z 序从上到下排列。`skip` 是我们自己的覆盖层。
pub fn visible_windows(skip: isize) -> Vec<WinInfo> {
    let mut handles: Vec<isize> = Vec::with_capacity(256);
    let _ = unsafe { EnumWindows(Some(collect), LPARAM(&mut handles as *mut Vec<isize> as isize)) };

    let mut out = Vec::new();
    for id in handles {
        if id == skip {
            continue;
        }
        let h = hwnd(id);
        let Some(rect) = frame_bounds(id) else { continue };
        let ex = unsafe { GetWindowLongPtrW(h, GWL_EXSTYLE) } as u32;
        // 点击穿透的透明覆盖层（显卡叠加层、其他桌宠……）既不能站也不遮挡
        if ex & WS_EX_TRANSPARENT.0 != 0 {
            continue;
        }
        let class = class_name(h);
        if SHELL_CLASSES.contains(&class.as_str()) {
            continue;
        }
        let tool = ex & WS_EX_TOOLWINDOW.0 != 0 && ex & WS_EX_APPWINDOW.0 == 0;
        let platform = !tool
            && !NON_PLATFORM_CLASSES.contains(&class.as_str())
            && rect.width() >= 120
            && rect.height() >= 60;
        out.push(WinInfo { id, rect, platform });
    }
    out
}

/// 按顺序看这些进程（通常是某个进程的父、祖父……），返回第一个拥有可见主窗口的进程的那个窗口。
/// 同一个进程有好几个窗口时取 Z 序最上面的。用来找"在跑 Claude Code 的是哪个终端/编辑器窗口"。
pub fn window_for_pids(pids: &[u32]) -> Option<isize> {
    if pids.is_empty() {
        return None;
    }
    let mut handles: Vec<isize> = Vec::with_capacity(256);
    let _ = unsafe { EnumWindows(Some(collect), LPARAM(&mut handles as *mut Vec<isize> as isize)) };
    let mut by_pid: std::collections::HashMap<u32, isize> = std::collections::HashMap::new();
    for id in handles {
        let h = hwnd(id);
        if frame_bounds(id).is_none() {
            continue;
        }
        let ex = unsafe { GetWindowLongPtrW(h, GWL_EXSTYLE) } as u32;
        let tool = ex & WS_EX_TOOLWINDOW.0 != 0 && ex & WS_EX_APPWINDOW.0 == 0;
        let owned = unsafe { GetWindow(h, GW_OWNER) }.is_ok_and(|o| !o.0.is_null());
        if tool || owned || ex & WS_EX_TRANSPARENT.0 != 0 {
            continue;
        }
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
        by_pid.entry(pid).or_insert(id);
    }
    pids.iter().find_map(|p| by_pid.get(p).copied())
}

fn monitor_rect(h: HWND) -> Option<Rect> {
    let mon = unsafe { MonitorFromWindow(h, MONITOR_DEFAULTTONEAREST) };
    let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
    unsafe { GetMonitorInfoW(mon, &mut mi) }.as_bool().then(|| mi.rcMonitor.into())
}

/// 哪几块屏幕上有全屏程序（游戏、全屏视频、演示模式），返回那几块屏幕的整屏矩形。
/// 不只看前台窗口：在副屏全屏放着视频、回主屏干活时，副屏的视频也得算。
/// 每块屏幕看盖在最上面的那个大窗口：盖满整块屏幕（不只是工作区）就是全屏。
pub fn fullscreen_monitors(skip: isize) -> Vec<Rect> {
    let mut out: Vec<Rect> = Vec::new();
    unsafe {
        // 独占全屏的游戏、演示模式：前台窗口所在的那块
        if let Ok(state) = SHQueryUserNotificationState() {
            if state == QUNS_RUNNING_D3D_FULL_SCREEN || state == QUNS_PRESENTATION_MODE {
                if let Some(m) = monitor_rect(GetForegroundWindow()) {
                    out.push(m);
                }
            }
        }
    }
    let mut undecided: Vec<Rect> = screens().iter().map(|s| s.monitor).filter(|m| !out.contains(m)).collect();
    let mut handles: Vec<isize> = Vec::with_capacity(256);
    let _ = unsafe { EnumWindows(Some(collect), LPARAM(&mut handles as *mut Vec<isize> as isize)) };
    for id in handles {
        if undecided.is_empty() {
            break;
        }
        if id == skip {
            continue;
        }
        let h = hwnd(id);
        unsafe {
            if !IsWindowVisible(h).as_bool() || IsIconic(h).as_bool() {
                continue;
            }
        }
        if is_cloaked(h) {
            continue;
        }
        let ex = unsafe { GetWindowLongPtrW(h, GWL_EXSTYLE) } as u32;
        if ex & WS_EX_TRANSPARENT.0 != 0 || (ex & WS_EX_TOOLWINDOW.0 != 0 && ex & WS_EX_APPWINDOW.0 == 0) {
            continue;
        }
        if SHELL_CLASSES.contains(&class_name(h).as_str()) {
            continue;
        }
        let mut r = RECT::default();
        if unsafe { GetWindowRect(h, &mut r) }.is_err() {
            continue;
        }
        let r: Rect = r.into();
        // 任务栏自动隐藏时，普通最大化窗口也会铺满屏幕：有标题栏的最大化窗口不算全屏
        let zoomed = unsafe {
            IsZoomed(h).as_bool() && GetWindowLongPtrW(h, GWL_STYLE) as u32 & WS_CAPTION.0 == WS_CAPTION.0
        };
        undecided.retain(|m| {
            let w = (r.right.min(m.right) - r.left.max(m.left)).max(0) as i64;
            let ht = (r.bottom.min(m.bottom) - r.top.max(m.top)).max(0) as i64;
            // 小窗口（画中画、便签、浮动工具条）不算数，接着往下看
            if w * ht * 2 < m.width() as i64 * m.height() as i64 {
                return true;
            }
            let covers = r.left <= m.left && r.top <= m.top && r.right >= m.right && r.bottom >= m.bottom;
            if covers && !zoomed {
                out.push(*m);
            }
            false
        });
    }
    out
}
