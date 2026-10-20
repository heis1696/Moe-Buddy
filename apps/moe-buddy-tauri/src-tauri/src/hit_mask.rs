//! 宠物窗口的 alpha 命中区域与鼠标穿透适配。
//!
//! 区域分段和光标反查必须使用同一套整数映射：掩码格子 `c` 覆盖
//! `[floor(c * extent / grid), floor((c + 1) * extent / grid))`，光标物理像素 `p`
//! 反查唯一覆盖它的格子。两者若不互逆，就会出现“点在猫身上却穿透/点在透明处却命中”。

#[cfg(windows)]
use tauri::Manager;

#[cfg(any(windows, test))]
const MAX_HIT_RUNS: usize = 4096;

#[cfg(any(windows, test))]
fn scale_mask_cell(edge: u16, extent: u32, mask_extent: u16) -> i32 {
    (u64::from(edge) * u64::from(extent) / u64::from(mask_extent)).min(i32::MAX as u64) as i32
}

/// 返回覆盖物理像素 `pixel` 的掩码格子；它必须是 [`scale_mask_cell`] 分段的严格反查。
#[cfg(any(windows, test))]
fn mask_cell_for_pixel(pixel: i32, extent: u32, mask_extent: u16) -> u16 {
    if extent == 0 || mask_extent == 0 {
        return 0;
    }
    let extent = u64::from(extent);
    let grid = u64::from(mask_extent);
    let pixel = u64::try_from(pixel.max(0))
        .unwrap_or(0)
        .min(extent.saturating_sub(1));
    // 找第一个满足 floor((c + 1) * extent / grid) > pixel 的格子：
    // c = ceil((pixel + 1) * grid / extent) - 1。
    let cell = ((pixel + 1) * grid).div_ceil(extent).saturating_sub(1);
    cell.min(grid - 1) as u16
}

#[cfg(any(windows, test))]
fn alpha_is_hit(alpha: u8) -> bool {
    alpha != 0
}

/// 一行的矩形 run，使用 Win32 的 `[left, right) × [top, bottom)` 半开区间。
#[cfg(any(windows, test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HitRect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

/// 在创建任何 GDI 区域之前把 alpha 网格展开成有界矩形 run。
/// 超复杂棋盘等掩码在这里直接拒绝，因此失败时窗口保留原区域。
#[cfg(any(windows, test))]
fn plan_hit_region(
    width: u16,
    height: u16,
    extent_width: u32,
    extent_height: u32,
    alpha: &[u8],
) -> Result<Vec<HitRect>, String> {
    let cells = usize::from(width)
        .checked_mul(usize::from(height))
        .ok_or("mask dimensions overflow")?;
    if width == 0 || height == 0 || extent_width == 0 || extent_height == 0 || alpha.len() != cells
    {
        return Err("invalid hit mask dimensions or length".into());
    }

    let mut rects = Vec::new();
    for y in 0..height {
        let top = scale_mask_cell(y, extent_height, height);
        let bottom = scale_mask_cell(y + 1, extent_height, height);
        if top >= bottom {
            // 窗口比网格窄时该行没有独立的物理像素；反查也不会选中它。
            continue;
        }
        let mut x = 0;
        while x < width {
            while x < width
                && !alpha_is_hit(alpha[usize::from(y) * usize::from(width) + usize::from(x)])
            {
                x += 1;
            }
            let left = x;
            while x < width
                && alpha_is_hit(alpha[usize::from(y) * usize::from(width) + usize::from(x)])
            {
                x += 1;
            }
            if left >= x {
                continue;
            }
            let left_px = scale_mask_cell(left, extent_width, width);
            let right_px = scale_mask_cell(x, extent_width, width);
            if left_px >= right_px {
                continue;
            }
            if rects.len() >= MAX_HIT_RUNS {
                return Err("alpha hit region exceeds the rectangle budget".into());
            }
            rects.push(HitRect {
                left: left_px,
                top,
                right: right_px,
                bottom,
            });
        }
    }
    Ok(rects)
}

#[cfg(windows)]
pub(crate) fn apply_hit_mask(
    handle: &tauri::AppHandle,
    width: u16,
    height: u16,
    alpha: &[u8],
) -> Result<(), String> {
    use windows_sys::Win32::Foundation::RECT;
    use windows_sys::Win32::Graphics::Gdi::{
        CombineRgn, CreateRectRgn, DeleteObject, RGN_OR, SetWindowRgn,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::GetClientRect;

    let window = handle
        .get_webview_window("pet")
        .ok_or("pet window unavailable")?;
    let hwnd = window.hwnd().map_err(|_| "pet window handle unavailable")?;
    let hwnd = hwnd.0 as _;

    // 先把掩码转成有界 run，超复杂掩码在分配 GDI 区域前就失败。
    let (width_px, height_px) = unsafe {
        let mut client = RECT::default();
        if GetClientRect(hwnd, &mut client) == 0 {
            return Err("unable to read pet client dimensions".into());
        }
        (client.right.max(0) as u32, client.bottom.max(0) as u32)
    };
    let rects = plan_hit_region(width, height, width_px, height_px, alpha)?;

    unsafe {
        let region = CreateRectRgn(0, 0, 0, 0);
        if region.is_null() {
            return Err("unable to allocate hit region".into());
        }
        for rect in rects {
            let row = CreateRectRgn(rect.left, rect.top, rect.right, rect.bottom);
            if row.is_null() {
                DeleteObject(region);
                return Err("unable to allocate hit region row".into());
            }
            let combined = CombineRgn(region, region, row, RGN_OR);
            DeleteObject(row);
            if combined == 0 {
                DeleteObject(region);
                return Err("Windows rejected the alpha hit region complexity".into());
            }
        }
        // 成功后 region 归系统所有；失败时仍由本函数删除，保持既有 GDI 所有权。
        if SetWindowRgn(hwnd, region, 1) == 0 {
            DeleteObject(region);
            return Err("Windows failed to install the alpha hit region".into());
        }
    }
    Ok(())
}

#[cfg(windows)]
const HIT_MASK_ERROR_EVENT: &str = "prototype://hit-mask-error";

#[cfg(windows)]
static REPORTED_PASSTHROUGH_ERROR: std::sync::atomic::AtomicU8 =
    std::sync::atomic::AtomicU8::new(0);

#[cfg(windows)]
fn report_passthrough_error(handle: &tauri::AppHandle, code: &'static str) {
    use std::sync::atomic::Ordering;
    use tauri::Emitter;

    let id = match code {
        "cursor_query" => 1,
        "window_style" => 2,
        _ => 3,
    };
    if REPORTED_PASSTHROUGH_ERROR.swap(id, Ordering::Relaxed) != id {
        // 只发送固定错误码，不携带宿主原始错误正文。
        let _ = handle.emit_to("pet", HIT_MASK_ERROR_EVENT, code);
        eprintln!("moe-buddy-tauri: hit mask passthrough update failed ({code})");
    }
}

#[cfg(windows)]
fn clear_passthrough_error() {
    REPORTED_PASSTHROUGH_ERROR.store(0, std::sync::atomic::Ordering::Relaxed);
}

#[cfg(windows)]
pub(crate) fn poll_cursor_passthrough(
    handle: &tauri::AppHandle,
    width: u16,
    height: u16,
    alpha: &[u8],
) {
    match update_passthrough(handle, width, height, alpha) {
        Ok(()) => clear_passthrough_error(),
        Err(code) => report_passthrough_error(handle, code),
    }
}

#[cfg(windows)]
fn update_passthrough(
    handle: &tauri::AppHandle,
    width: u16,
    height: u16,
    alpha: &[u8],
) -> Result<(), &'static str> {
    use windows_sys::Win32::Foundation::{GetLastError, POINT, RECT, SetLastError};
    use windows_sys::Win32::Graphics::Gdi::ScreenToClient;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GWL_EXSTYLE, GetClientRect, GetCursorPos, GetWindowLongPtrW, SetWindowLongPtrW,
        WS_EX_TRANSPARENT,
    };

    let window = handle.get_webview_window("pet").ok_or("cursor_query")?;
    let hwnd = window.hwnd().map_err(|_| "cursor_query")?;
    let hwnd = hwnd.0 as _;
    unsafe {
        let mut point = POINT::default();
        let mut client = RECT::default();
        if GetCursorPos(&mut point) == 0
            || ScreenToClient(hwnd, &mut point) == 0
            || GetClientRect(hwnd, &mut client) == 0
        {
            return Err("cursor_query");
        }
        let w = client.right.max(0);
        let h = client.bottom.max(0);
        let inside = point.x >= 0 && point.y >= 0 && point.x < w && point.y < h;
        let opaque = if inside && w > 0 && h > 0 {
            let x = mask_cell_for_pixel(point.x, w as u32, width);
            let y = mask_cell_for_pixel(point.y, h as u32, height);
            alpha_is_hit(alpha[usize::from(y) * usize::from(width) + usize::from(x)])
        } else {
            false
        };

        SetLastError(0);
        let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        if style == 0 && GetLastError() != 0 {
            return Err("window_style");
        }
        let transparent = WS_EX_TRANSPARENT as isize;
        let desired = if opaque {
            style & !transparent
        } else {
            style | transparent
        };
        if desired != style {
            SetLastError(0);
            let previous = SetWindowLongPtrW(hwnd, GWL_EXSTYLE, desired);
            if previous == 0 && GetLastError() != 0 {
                return Err("window_style");
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pattern_alpha(width: u16, height: u16) -> Vec<u8> {
        let mut alpha = vec![0_u8; usize::from(width) * usize::from(height)];
        for y in 0..usize::from(height) {
            for x in 0..usize::from(width) {
                // 大块不透明区域上挖出少量透明洞，既能覆盖边界，又不会超过 run 预算。
                let hole = x % 41 < 3 && y % 31 < 3;
                let value = if hole { 0 } else { 255 };
                alpha[y * usize::from(width) + x] = value;
            }
        }
        alpha
    }

    fn rect_contains(rect: &HitRect, x: i32, y: i32) -> bool {
        rect.left <= x && x < rect.right && rect.top <= y && y < rect.bottom
    }

    #[test]
    fn mask_cell_reverse_index_is_the_exact_inverse_of_scaling() {
        for (extent, grid) in [
            (320_u32, 256_u16),
            (100, 256),
            (255, 256),
            (256, 256),
            (480, 4),
            (7, 256),
            (1, 256),
            (256, 1),
            (1, 1),
        ] {
            for pixel in 0..extent {
                let cell = mask_cell_for_pixel(pixel as i32, extent, grid);
                let start = scale_mask_cell(cell, extent, grid);
                let end = scale_mask_cell(cell + 1, extent, grid);
                assert!(
                    start <= pixel as i32 && (pixel as i32) < end,
                    "pixel {pixel} of {extent}/{grid} maps to cell {cell} [{start}, {end})"
                );
            }
        }

        // 已复现的 P2 例子：320 物理像素 / 256 格时 pixel 1 属于第 1 格。
        assert_eq!(scale_mask_cell(1, 320, 256), 1);
        assert_eq!(mask_cell_for_pixel(1, 320, 256), 1);
    }

    #[test]
    fn every_physical_pixel_region_matches_the_reverse_index() {
        let cases = [
            (320_u32, 240_u32, 256_u16, 192_u16),
            (100, 80, 256, 256),
            (255, 255, 256, 256),
            (7, 5, 256, 256),
            (256, 240, 256, 240),
            (100, 240, 100, 240),
        ];
        for (extent_w, extent_h, grid_w, grid_h) in cases {
            let alpha = pattern_alpha(grid_w, grid_h);
            let rects =
                plan_hit_region(grid_w, grid_h, extent_w, extent_h, &alpha).expect("bounded plan");
            for y in 0..extent_h {
                for x in 0..extent_w {
                    let cell_x = mask_cell_for_pixel(x as i32, extent_w, grid_w);
                    let cell_y = mask_cell_for_pixel(y as i32, extent_h, grid_h);
                    let expected = alpha_is_hit(
                        alpha[usize::from(cell_y) * usize::from(grid_w) + usize::from(cell_x)],
                    );
                    let actual = rects
                        .iter()
                        .any(|rect| rect_contains(rect, x as i32, y as i32));
                    assert_eq!(
                        actual, expected,
                        "case {extent_w}x{extent_h} grid {grid_w}x{grid_h} at ({x}, {y})"
                    );
                }
            }
        }
    }

    #[test]
    fn transparent_holes_and_boundaries_are_not_hit() {
        let alpha = vec![0_u8; 256];
        let rects = plan_hit_region(16, 16, 320, 320, &alpha).expect("empty plan");
        assert!(rects.is_empty());

        let mut alpha = vec![0_u8; 256];
        alpha[0] = 255;
        alpha[255] = 255;
        let rects = plan_hit_region(16, 16, 32, 32, &alpha).expect("corner plan");
        assert!(rect_contains(&rects[0], 0, 0));
        assert!(rects.iter().any(|rect| rect_contains(rect, 31, 31)));
        assert!(!rects.iter().any(|rect| rect_contains(rect, 16, 16)));
    }

    #[test]
    fn checkerboard_beyond_the_run_budget_is_rejected_before_installation() {
        let mut too_complex = vec![0_u8; 128 * 128];
        for y in 0..128 {
            for x in 0..128 {
                too_complex[y * 128 + x] = if (x + y) % 2 == 0 { 255 } else { 0 };
            }
        }
        assert!(
            plan_hit_region(128, 128, 128, 128, &too_complex).is_err(),
            "棋盘掩码必须在创建/安装 GDI 区域前被拒绝"
        );

        let mut bounded = vec![0_u8; 64 * 64];
        for y in 0..64 {
            for x in 0..64 {
                bounded[y * 64 + x] = if (x + y) % 2 == 0 { 255 } else { 0 };
            }
        }
        assert!(plan_hit_region(64, 64, 64, 64, &bounded).is_ok());
    }

    #[test]
    fn mask_validation_and_alpha_threshold_are_explicit() {
        assert!(alpha_is_hit(1));
        assert!(alpha_is_hit(255));
        assert!(!alpha_is_hit(0));
    }
}
