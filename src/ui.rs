use crate::{
    installed::{self, InstalledAgents},
    integration,
    model::*,
    monitor::Monitor,
    notifications::{Notice, Notifications},
    system,
};
use chrono::Local;
use std::{
    cell::Cell,
    collections::{HashMap, VecDeque},
    fs::File,
    io::BufWriter,
    mem::{size_of, zeroed},
    path::Path,
    ptr::{null, null_mut},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    System::{LibraryLoader::*, Registry::*, Threading::*},
    UI::{HiDpi::*, Input::KeyboardAndMouse::*, Shell::*, WindowsAndMessaging::*},
};
const CALLBACK: u32 = WM_APP + 1;
const QUEUED_CALLBACK: u32 = WM_APP + 2;
const QUEUED_TASKBAR: u32 = WM_APP + 3;
static TASKBAR_MESSAGE: AtomicU32 = AtomicU32::new(0);
const NIN_KEYSELECT: u32 = 1025;
fn copy_wide<const N: usize>(target: &mut [u16; N], text: &str) {
    let mut offset = 0;
    for ch in text.chars() {
        let mut pair = [0; 2];
        let units = ch.encode_utf16(&mut pair);
        if offset + units.len() >= N {
            break;
        }
        target[offset..offset + units.len()].copy_from_slice(units);
        offset += units.len();
    }
}
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
fn rgb(r: u8, g: u8, b: u8) -> u32 {
    r as u32 | ((g as u32) << 8) | ((b as u32) << 16)
}
fn rect(x: i32, y: i32, w: i32, h: i32) -> RECT {
    RECT {
        left: x,
        top: y,
        right: x + w,
        bottom: y + h,
    }
}
pub fn error(message: &str) {
    unsafe {
        MessageBoxW(
            null_mut(),
            wide(message).as_ptr(),
            wide("Claude + Codex").as_ptr(),
            MB_OK | MB_ICONERROR,
        );
    }
}
unsafe extern "system" fn proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    // Shell может вызывать оконную процедуру через SendMessage, минуя очередь GetMessage.
    // Пересылаем событие в очередь, не передавая процедуре изменяемое состояние App.
    if msg == CALLBACK {
        unsafe {
            PostMessageW(hwnd, QUEUED_CALLBACK, w, l);
        }
        return 0;
    }
    if msg != 0 && msg == TASKBAR_MESSAGE.load(Ordering::Relaxed) {
        unsafe {
            PostMessageW(hwnd, QUEUED_TASKBAR, 0, 0);
        }
        return 0;
    }
    match msg {
        WM_MOUSEACTIVATE => MA_NOACTIVATE as isize,
        WM_ERASEBKGND => 1,
        WM_CLOSE => {
            unsafe { PostQuitMessage(0) };
            0
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, w, l) },
    }
}
struct Canvas {
    dc: HDC,
    bitmap: HBITMAP,
    old: HGDIOBJ,
    pixels: *mut u8,
    width: i32,
    height: i32,
    scale: f64,
    offset: Cell<(i32, i32)>,
}
impl Canvas {
    unsafe fn new(width: i32, height: i32, scale: f64) -> Self {
        unsafe {
            let dc = CreateCompatibleDC(null_mut());
            let mut info: BITMAPINFO = zeroed();
            info.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
            info.bmiHeader.biWidth = width;
            info.bmiHeader.biHeight = -height;
            info.bmiHeader.biPlanes = 1;
            info.bmiHeader.biBitCount = 32;
            let mut pixels = null_mut();
            let bitmap = CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut pixels, null_mut(), 0);
            let old = SelectObject(dc, bitmap);
            SetBkMode(dc, TRANSPARENT as i32);
            Self {
                dc,
                bitmap,
                old,
                pixels: pixels.cast(),
                width,
                height,
                scale,
                offset: Cell::new((0, 0)),
            }
        }
    }
    fn n(&self, v: i32) -> i32 {
        (v as f64 * self.scale).round() as i32
    }
    fn fill(&self, x: i32, y: i32, w: i32, h: i32, color: u32) {
        unsafe {
            let brush = CreateSolidBrush(color);
            FillRect(
                self.dc,
                &rect(
                    self.n(x + self.offset.get().0),
                    self.n(y + self.offset.get().1),
                    self.n(w),
                    self.n(h),
                ),
                brush,
            );
            DeleteObject(brush);
        }
    }
    // Явные координаты и стиль соответствуют примитиву GDI.
    #[allow(clippy::too_many_arguments)]
    fn text(
        &self,
        text: &str,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        size: i32,
        color: u32,
        bold: bool,
        flags: u32,
    ) {
        unsafe {
            let font = CreateFontW(
                -self.n(size),
                0,
                0,
                0,
                if bold { 700 } else { 400 },
                0,
                0,
                0,
                DEFAULT_CHARSET as u32,
                0,
                0,
                CLEARTYPE_QUALITY as u32,
                0,
                wide("Segoe UI").as_ptr(),
            );
            let old = SelectObject(self.dc, font);
            SetTextColor(self.dc, color);
            let text = wide(text);
            DrawTextW(
                self.dc,
                text.as_ptr(),
                (text.len() - 1) as i32,
                &mut rect(
                    self.n(x + self.offset.get().0),
                    self.n(y + self.offset.get().1),
                    self.n(width),
                    self.n(height),
                ),
                flags | DT_NOPREFIX,
            );
            SelectObject(self.dc, old);
            DeleteObject(font);
        }
    }
    fn png(&self, path: &Path) -> Result<(), String> {
        let bytes = unsafe {
            std::slice::from_raw_parts(self.pixels, (self.width * self.height * 4) as usize)
        };
        let mut rgba = Vec::with_capacity(bytes.len());
        for pixel in bytes.chunks_exact(4) {
            rgba.extend_from_slice(&[pixel[2], pixel[1], pixel[0], 255]);
        }
        let mut encoder = png::Encoder::new(
            BufWriter::new(File::create(path).map_err(|e| e.to_string())?),
            self.width as u32,
            self.height as u32,
        );
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder
            .write_header()
            .map_err(|e| e.to_string())?
            .write_image_data(&rgba)
            .map_err(|e| e.to_string())
    }
}
impl Drop for Canvas {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.old);
            DeleteObject(self.bitmap);
            DeleteDC(self.dc);
        }
    }
}
fn provider_height(provider: &Provider, settings: &Appearance) -> i32 {
    if settings.compact_panel {
        292
    } else {
        334 + (provider.sessions.len().min(3) + usize::from(provider.sessions.len() > 3)) as i32
            * 24
    }
}
fn regions(providers: &[Provider], settings: &Appearance) -> Vec<(usize, i32, i32, i32)> {
    let mut result = Vec::new();
    let mut y = 0;
    for (index, provider) in providers.iter().enumerate() {
        if (provider.name == "Claude" && !settings.show_claude)
            || (provider.name == "Codex" && !settings.show_codex)
        {
            continue;
        }
        let height = provider_height(provider, settings);
        let x = if settings.panel_layout == "Vertical" {
            0
        } else {
            result.len() as i32 * 370
        };
        result.push((
            index,
            x,
            if settings.panel_layout == "Vertical" {
                y
            } else {
                0
            },
            height,
        ));
        y += height;
    }
    result
}
fn panel_size(providers: &[Provider], settings: &Appearance) -> (i32, i32) {
    let regions = regions(providers, settings);
    let width = regions
        .iter()
        .map(|(_, x, _, _)| x + 370)
        .max()
        .unwrap_or(370);
    let height = regions
        .iter()
        .map(|(_, _, y, h)| y + h)
        .max()
        .unwrap_or(292);
    (width, height)
}
fn draw_panel(canvas: &Canvas, providers: &[Provider], settings: &Appearance) {
    let bg = rgb(28, 28, 32);
    let muted = rgb(158, 158, 170);
    let white = rgb(238, 238, 242);
    let single = DT_SINGLELINE | DT_END_ELLIPSIS;
    let border = rgb(62, 62, 70);
    let (width, height) = panel_size(providers, settings);
    canvas.offset.set((0, 0));
    canvas.fill(0, 0, width, height, bg);
    for (index, x, y, block_height) in regions(providers, settings) {
        canvas.offset.set((x, y));
        let p = &providers[index];
        let selected = p.sessions.first();
        canvas.text(
            &p.name,
            18,
            14,
            334,
            26,
            18,
            if p.name == "Codex" {
                rgb(130, 188, 242)
            } else {
                rgb(218, 151, 124)
            },
            true,
            single,
        );
        canvas.text(
            selected.map(|s| s.task()).unwrap_or("Нет недавних сессий"),
            18,
            44,
            334,
            24,
            13,
            white,
            false,
            single,
        );
        let mut status = if settings.show_text {
            selected
                .map(|s| s.label.clone())
                .unwrap_or("Ожидает сессию".into())
        } else {
            String::new()
        };
        if settings.show_timer {
            if let Some(s) = selected {
                if s.started_at > 0 && s.priority() > 0 {
                    if !status.is_empty() {
                        status.push_str(" · ");
                    }
                    status.push_str(&duration(now() - s.started_at));
                }
            }
        }
        canvas.text(
            &status,
            18,
            68,
            334,
            24,
            13,
            if selected.is_some_and(|s| s.state == "permission") {
                rgb(255, 197, 69)
            } else {
                white
            },
            false,
            single,
        );
        draw_limit(
            canvas,
            "5 часов",
            p.usage.as_ref().and_then(|u| u.five_hour.as_ref()),
            0,
            108,
        );
        draw_limit(
            canvas,
            "7 дней",
            p.usage.as_ref().and_then(|u| u.seven_day.as_ref()),
            0,
            185,
        );
        let updated = p
            .usage
            .as_ref()
            .map(|u| {
                format!(
                    "Обновлено: {}",
                    u.updated_at.with_timezone(&Local).format("%d.%m %H:%M:%S")
                )
            })
            .unwrap_or(format!("Лимиты появятся после ответа {}", p.name));
        canvas.text(&updated, 18, 261, 334, 24, 12, muted, false, single);
        if !settings.compact_panel {
            canvas.text(
                p.notice.as_deref().unwrap_or(if p.name == "Codex" {
                    "Состояние по локальным событиям"
                } else {
                    "Состояние по событиям Claude Code"
                }),
                18,
                283,
                334,
                24,
                12,
                muted,
                false,
                single,
            );
            for (i, s) in p.sessions.iter().take(3).enumerate() {
                let row = if settings.show_text {
                    format!("{} · {}", s.task(), s.label)
                } else {
                    s.task().into()
                };
                canvas.text(
                    &row,
                    18,
                    313 + i as i32 * 24,
                    334,
                    24,
                    12,
                    muted,
                    false,
                    single,
                );
            }
            if p.sessions.len() > 3 {
                canvas.text(
                    &format!("Ещё сессий: {}", p.sessions.len() - 3),
                    18,
                    385,
                    334,
                    24,
                    12,
                    muted,
                    false,
                    single,
                );
            }
        }
        if x > 0 {
            canvas.fill(0, 12, 1, block_height - 24, border);
        }
        if y > 0 {
            canvas.fill(12, 0, 346, 1, border);
        }
    }
    canvas.offset.set((0, 0));
    canvas.fill(0, 0, width, 1, border);
    canvas.fill(0, 0, 1, height, border);
    canvas.fill(width - 1, 0, 1, height, border);
    canvas.fill(0, height - 1, width, 1, border);
}
fn draw_limit(c: &Canvas, label: &str, limit: Option<&Limit>, x: i32, y: i32) {
    let valid = limit.filter(|l| l.resets_at > now() && l.used.is_finite());
    let color = match valid {
        None => rgb(128, 128, 128),
        Some(l) if l.used >= 90. => rgb(243, 107, 107),
        Some(l) if l.used >= 70. => rgb(240, 189, 92),
        _ => rgb(123, 201, 165),
    };
    let percentage = valid
        .map(|l| {
            if l.used.fract() == 0. {
                format!("{:.0}%", l.used)
            } else {
                format!("{:.1}%", l.used)
            }
        })
        .unwrap_or("—".into());
    c.text(
        label,
        x + 18,
        y,
        200,
        24,
        13,
        rgb(238, 238, 242),
        false,
        DT_SINGLELINE,
    );
    c.text(
        &percentage,
        x + 260,
        y,
        90,
        24,
        13,
        color,
        false,
        DT_SINGLELINE | DT_RIGHT,
    );
    c.fill(x + 20, y + 28, 328, 8, rgb(54, 54, 63));
    if let Some(l) = valid {
        c.fill(
            x + 20,
            y + 28,
            (328. * l.used.clamp(0., 100.) / 100.) as i32,
            8,
            color,
        );
    }
    let reset = if let Some(l) = valid {
        format!("Сброс через {}", duration(l.resets_at - now()))
    } else if limit.is_some() {
        "Окно истекло · ожидаем обновления".into()
    } else {
        "Нет данных".into()
    };
    c.text(
        &reset,
        x + 18,
        y + 43,
        334,
        24,
        12,
        rgb(158, 158, 170),
        false,
        DT_SINGLELINE | DT_END_ELLIPSIS,
    );
}
fn system_color() -> [u8; 3] {
    unsafe {
        let mut light: u32 = 0;
        let mut size = 4;
        RegGetValueW(
            HKEY_CURRENT_USER,
            wide(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize").as_ptr(),
            wide("SystemUsesLightTheme").as_ptr(),
            RRF_RT_REG_DWORD,
            null_mut(),
            (&mut light as *mut u32).cast(),
            &mut size,
        );
        if light == 1 {
            [35, 35, 35]
        } else {
            [245, 245, 245]
        }
    }
}
fn icon_pixels(settings: &Appearance, state: &str, frame: u32) -> Vec<u8> {
    let color = if settings.color_mode == "System" {
        system_color()
    } else if settings.color_mode == "Codex" {
        [130, 188, 242]
    } else {
        [218, 151, 124]
    };
    let spark: Vec<_> = (0..10)
        .map(|i| {
            let angle = i as f64 * std::f64::consts::PI / 5. + frame as f64 * 0.10;
            (angle.cos(), angle.sin())
        })
        .collect();
    let orbit: Vec<_> = (0..3)
        .map(|i| {
            let angle = i as f64 * std::f64::consts::TAU / 3. + frame as f64 * 0.16;
            let radius = 7. + (frame as f64 * 0.12).sin() * 2.;
            (angle.cos() * radius, angle.sin() * radius)
        })
        .collect();
    let angle = (frame / 2 % 4) as f64 * std::f64::consts::FRAC_PI_4;
    let (spinner_cos, spinner_sin) = (angle.cos(), angle.sin());
    let mut pixels = vec![0u8; 32 * 32 * 4];
    for y in 0..32 {
        for x in 0..32 {
            let (mut red, mut green, mut blue, mut alpha) = (0u32, 0u32, 0u32, 0u32);
            for sy in 0..4 {
                for sx in 0..4 {
                    let px = x as f64 + (sx as f64 + 0.5) / 4.;
                    let py = y as f64 + (sy as f64 + 0.5) / 4.;
                    let dx = px - 16.;
                    let dy = py - 16.;
                    let on = match settings.animation.as_str() {
                        "Orbit" => orbit
                            .iter()
                            .any(|(x, y)| (dx - x).powi(2) + (dy - y).powi(2) < 10.),
                        "Clawd" => {
                            let step = if frame == 0 {
                                0.
                            } else {
                                ((frame / 2) % 2) as f64 * 2.
                            };
                            let body = (7. ..25.).contains(&px) && (10. ..23.).contains(&py);
                            let eyes = ((11. ..14.).contains(&px) || (19. ..22.).contains(&px))
                                && (14. ..18.).contains(&py);
                            let claws = ((4. ..8.).contains(&px) && (7. + step..18.).contains(&py))
                                || ((24. ..28.).contains(&px) && (9. - step..18.).contains(&py));
                            let legs = (0..3).any(|i| {
                                ((8 + i * 6) as f64..(11 + i * 6) as f64).contains(&px)
                                    && (23. ..27. + if i % 2 == (frame / 2) % 2 { 2. } else { 0. })
                                        .contains(&py)
                            });
                            (body && !eyes) || claws || legs
                        }
                        "Unicode" => {
                            let along = dx * spinner_sin + dy * spinner_cos;
                            let cross = dx * spinner_cos - dy * spinner_sin;
                            along.abs() < 12. && cross.abs() < 1.6
                        }
                        _ => spark.iter().any(|(cos, sin)| {
                            let along = dx * cos + dy * sin;
                            let cross = -dx * sin + dy * cos;
                            (5. ..13.).contains(&along) && cross.abs() < 1.45
                        }),
                    };
                    let dot =
                        state == "permission" && (px - 26.).powi(2) + (py - 26.).powi(2) < 22.;
                    if on || dot {
                        let c = if dot {
                            [255, 197, 69]
                        } else if settings.color_mode == "Split" && px >= 16. {
                            [130, 188, 242]
                        } else {
                            color
                        };
                        red += c[0] as u32;
                        green += c[1] as u32;
                        blue += c[2] as u32;
                        alpha += 255;
                    }
                }
            }
            let i = (y * 32 + x) * 4;
            pixels[i] = (blue / 16) as u8;
            pixels[i + 1] = (green / 16) as u8;
            pixels[i + 2] = (red / 16) as u8;
            pixels[i + 3] = (alpha / 16) as u8;
        }
    }
    pixels
}
unsafe fn make_icon(settings: &Appearance, state: &str, frame: u32) -> HICON {
    unsafe {
        let pixels = icon_pixels(settings, state, frame);
        let canvas = Canvas::new(32, 32, 1.);
        std::ptr::copy_nonoverlapping(pixels.as_ptr(), canvas.pixels, pixels.len());
        let mask = CreateBitmap(32, 32, 1, 1, null());
        let info = ICONINFO {
            fIcon: 1,
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: mask,
            hbmColor: canvas.bitmap,
        };
        let icon = CreateIconIndirect(&info);
        DeleteObject(mask);
        icon
    }
}
struct App {
    owner: HWND,
    panel: HWND,
    tip: HWND,
    icons: Vec<(u32, HICON)>,
    icon_signatures: HashMap<u32, String>,
    active_tray: u32,
    notifications: Notifications,
    pending_notices: VecDeque<Notice>,
    last_notice_at: Instant,
    settings: Appearance,
    installed: InstalledAgents,
    last_install_probe: Instant,
    providers: Vec<Provider>,
    shared: Arc<Mutex<Vec<Provider>>>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
    demo: bool,
    pinned: bool,
    visible: bool,
    frame: u32,
    last_update: Instant,
    last_hover: Instant,
    hover_point: POINT,
    scale: f64,
    tip_text: String,
    tip_pending: String,
    tip_since: Instant,
    taskbar: u32,
    menu_active: bool,
}
impl App {
    unsafe fn notify_one(&self, action: u32, id: u32, icon: HICON) {
        unsafe {
            let mut data: NOTIFYICONDATAW = zeroed();
            data.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
            data.hWnd = self.owner;
            data.uID = id;
            data.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
            data.uCallbackMessage = CALLBACK;
            data.hIcon = icon;
            copy_wide(
                &mut data.szTip,
                if self.settings.separate_icons {
                    if id == 1 { "Claude Code" } else { "Codex" }
                } else {
                    "Claude + Codex"
                },
            );
            Shell_NotifyIconW(action, &data);
            if action == NIM_ADD {
                data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
                Shell_NotifyIconW(NIM_SETVERSION, &data);
            }
        }
    }
    unsafe fn notify(&self, action: u32) {
        unsafe {
            for (id, icon) in &self.icons {
                self.notify_one(action, *id, *icon);
            }
        }
    }
    fn icon_settings(&self, id: u32) -> Appearance {
        let mut settings = self.settings.clone();
        if settings.color_mode == "Orange" {
            settings.color_mode = if self.settings.separate_icons {
                if id == 2 { "Codex" } else { "Orange" }
            } else {
                self.installed.brand_mode()
            }
            .into();
        }
        settings
    }
    unsafe fn rebuild_icons(&mut self) {
        unsafe {
            self.icon_signatures.clear();
            let old = std::mem::take(&mut self.icons);
            for (id, icon) in old {
                self.notify_one(NIM_DELETE, id, icon);
                DestroyIcon(icon);
            }
            let ids = if self.settings.separate_icons {
                let mut ids = Vec::new();
                if self.settings.show_claude {
                    ids.push(1);
                }
                if self.settings.show_codex {
                    ids.push(2);
                }
                ids
            } else {
                vec![1]
            };
            for id in ids {
                let icon = make_icon(&self.icon_settings(id), "idle", 0);
                self.icons.push((id, icon));
                self.notify_one(NIM_ADD, id, icon);
            }
            if !self.icons.iter().any(|(id, _)| *id == self.active_tray) {
                self.active_tray = self.icons.first().map(|(id, _)| *id).unwrap_or(1);
            }
        }
    }
    unsafe fn update_icon(&mut self) {
        unsafe {
            self.frame = self.frame.wrapping_add(1);
            for index in 0..self.icons.len() {
                let id = self.icons[index].0;
                let selected = self
                    .providers
                    .iter()
                    .filter(|p| {
                        if self.settings.separate_icons {
                            p.name == if id == 1 { "Claude" } else { "Codex" }
                        } else {
                            (p.name == "Claude" && self.settings.show_claude)
                                || (p.name == "Codex" && self.settings.show_codex)
                        }
                    })
                    .flat_map(|p| &p.sessions)
                    .max_by_key(|s| (s.priority(), s.updated_at));
                let state = selected.map(|s| s.state.as_str()).unwrap_or("idle");
                let frame = if selected.is_some_and(|s| s.busy()) {
                    self.frame
                } else {
                    0
                };
                let settings = self.icon_settings(id);
                let signature = format!(
                    "{}:{}:{}:{}:{:?}",
                    settings.animation,
                    settings.color_mode,
                    state,
                    frame,
                    if settings.color_mode == "System" {
                        system_color()
                    } else {
                        [0, 0, 0]
                    }
                );
                if self.icon_signatures.get(&id) == Some(&signature) {
                    continue;
                }
                self.icon_signatures.insert(id, signature);
                let icon = make_icon(&settings, state, frame);
                let old = self.icons[index].1;
                self.icons[index].1 = icon;
                self.notify_one(NIM_MODIFY, id, icon);
                DestroyIcon(old);
            }
        }
    }
    unsafe fn show_notice(&mut self, notice: Notice) {
        unsafe {
            let mut data: NOTIFYICONDATAW = zeroed();
            data.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
            data.hWnd = self.owner;
            data.uID = if self.settings.separate_icons
                && notice.provider == "Codex"
                && self.settings.show_codex
            {
                2
            } else {
                self.icons.first().map(|(id, _)| *id).unwrap_or(1)
            };
            data.uFlags = NIF_INFO;
            data.dwInfoFlags = NIIF_INFO | if self.settings.sound { 0 } else { NIIF_NOSOUND };
            copy_wide(&mut data.szInfoTitle, &notice.title);
            copy_wide(&mut data.szInfo, &notice.text);
            Shell_NotifyIconW(NIM_MODIFY, &data);
            self.last_notice_at = Instant::now();
        }
    }
    unsafe fn paint(&self, hwnd: HWND) {
        unsafe {
            let mut ps: PAINTSTRUCT = zeroed();
            let dc = BeginPaint(hwnd, &mut ps);
            let mut bounds: RECT = zeroed();
            GetClientRect(hwnd, &mut bounds);
            if bounds.right > 0 && bounds.bottom > 0 {
                let canvas = Canvas::new(bounds.right, bounds.bottom, self.scale);
                if hwnd == self.panel {
                    draw_panel(&canvas, &self.providers, &self.settings);
                } else {
                    canvas.fill(0, 0, 800, 400, rgb(48, 48, 54));
                    canvas.text(
                        &self.tip_text,
                        10,
                        7,
                        (bounds.right as f64 / self.scale) as i32 - 20,
                        (bounds.bottom as f64 / self.scale) as i32 - 14,
                        13,
                        rgb(245, 245, 245),
                        false,
                        DT_WORDBREAK,
                    );
                }
                BitBlt(
                    dc,
                    0,
                    0,
                    bounds.right,
                    bounds.bottom,
                    canvas.dc,
                    0,
                    0,
                    SRCCOPY,
                );
            }
            EndPaint(hwnd, &ps);
        }
    }
    unsafe fn tray_rect(&self) -> Option<RECT> {
        unsafe {
            let id = NOTIFYICONIDENTIFIER {
                cbSize: size_of::<NOTIFYICONIDENTIFIER>() as u32,
                hWnd: self.owner,
                uID: self.active_tray,
                guidItem: zeroed(),
            };
            let mut r = zeroed();
            if Shell_NotifyIconGetRect(&id, &mut r) >= 0 {
                Some(r)
            } else {
                None
            }
        }
    }
    unsafe fn show(&mut self) {
        unsafe {
            if self.menu_active || self.visible {
                return;
            }
            let mut point: POINT = zeroed();
            GetCursorPos(&mut point);
            let area = work_area(point);
            let (width, height) = panel_size(&self.providers, &self.settings);
            self.scale = (GetDpiForWindow(self.owner) as f64 / 96.
                * self.settings.panel_scale as f64
                / 100.)
                .min((area.right - area.left) as f64 / width as f64)
                .min((area.bottom - area.top) as f64 / height as f64)
                .max(0.25);
            let w = (width as f64 * self.scale) as i32;
            let h = (height as f64 * self.scale) as i32;
            let anchor = self.tray_rect().unwrap_or(rect(point.x, point.y, 1, 1));
            let x = ((anchor.left + anchor.right - w) / 2)
                .clamp(area.left, (area.right - w).max(area.left));
            let y = if point.y > (area.top + area.bottom) / 2 {
                anchor.top - h - 10
            } else {
                anchor.bottom + 10
            };
            SetWindowPos(
                self.panel,
                HWND_TOPMOST,
                x,
                y.clamp(area.top, (area.bottom - h).max(area.top)),
                w,
                h,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
            InvalidateRect(self.panel, null(), 0);
            self.visible = true;
        }
    }
    unsafe fn hide(&mut self) {
        unsafe {
            ShowWindow(self.panel, SW_HIDE);
            ShowWindow(self.tip, SW_HIDE);
            self.visible = false;
            self.tip_pending.clear();
            self.tip_text.clear();
        }
    }
    unsafe fn tick(&mut self) {
        unsafe {
            if self.last_install_probe.elapsed() >= Duration::from_secs(30) {
                self.installed = installed::detect();
                self.last_install_probe = Instant::now();
            }
            if self.last_update.elapsed() >= Duration::from_secs(1) {
                self.providers = if self.demo {
                    demo()
                } else {
                    self.shared
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .clone()
                };
                self.last_update = Instant::now();
                if !self.demo {
                    for notice in self
                        .notifications
                        .observe(&self.providers, &self.settings, now())
                    {
                        if self.pending_notices.len() < 20 {
                            self.pending_notices.push_back(notice);
                        }
                    }
                }
                if self.settings.quiet || !self.settings.notifications {
                    self.pending_notices.clear();
                }
                if self.visible {
                    InvalidateRect(self.panel, null(), 0);
                    let mut r: RECT = zeroed();
                    GetWindowRect(self.panel, &mut r);
                    let height =
                        (panel_size(&self.providers, &self.settings).1 as f64 * self.scale) as i32;
                    if r.bottom - r.top != height {
                        let point = POINT {
                            x: r.left,
                            y: r.top,
                        };
                        let area = work_area(point);
                        let y = (r.bottom - height)
                            .clamp(area.top, (area.bottom - height).max(area.top));
                        SetWindowPos(
                            self.panel,
                            null_mut(),
                            r.left,
                            y,
                            r.right - r.left,
                            height,
                            SWP_NOACTIVATE | SWP_NOZORDER,
                        );
                    }
                }
            }
            self.update_icon();
            if self.menu_active {
                return;
            }
            if self.last_notice_at.elapsed() >= Duration::from_secs(8) {
                if let Some(notice) = self.pending_notices.pop_front() {
                    self.show_notice(notice);
                }
            }
            let mut point: POINT = zeroed();
            GetCursorPos(&mut point);
            let mut panel: RECT = zeroed();
            GetWindowRect(self.panel, &mut panel);
            let mut tip: RECT = zeroed();
            GetWindowRect(self.tip, &mut tip);
            let over_tray = self.tray_rect().is_some_and(|r| inside(point, r));
            let over_panel = self.visible && inside(point, panel);
            let over_tip = IsWindowVisible(self.tip) != 0 && inside(point, tip);
            if self.pinned
                && (GetAsyncKeyState(1) as u16 & 0x8000) != 0
                && !over_tray
                && !over_panel
                && !over_tip
            {
                self.pinned = false;
            }
            if !self.pinned
                && !over_tray
                && !over_panel
                && !over_tip
                && self.last_hover.elapsed() > Duration::from_millis(650)
            {
                self.hide();
            }
            if over_panel {
                let x = ((point.x - panel.left) as f64 / self.scale) as i32;
                let y = ((point.y - panel.top) as f64 / self.scale) as i32;
                let text = self.title_at(x, y);
                if text != self.tip_pending {
                    self.tip_pending = text;
                    self.tip_since = Instant::now();
                    ShowWindow(self.tip, SW_HIDE);
                }
                if !self.tip_pending.is_empty()
                    && self.tip_since.elapsed() > Duration::from_millis(500)
                {
                    self.show_tip(point);
                }
            } else if !over_tip {
                ShowWindow(self.tip, SW_HIDE);
                self.tip_pending.clear();
            }
        }
    }
    fn session_at(&self, x: i32, y: i32) -> Option<(String, Session)> {
        for (index, left, top, height) in regions(&self.providers, &self.settings) {
            let localx = x - left;
            let localy = y - top;
            if !(18..352).contains(&localx) || !(0..height).contains(&localy) {
                continue;
            }
            let p = &self.providers[index];
            let session = if (44..68).contains(&localy) {
                p.sessions.first()
            } else if !self.settings.compact_panel && (313..385).contains(&localy) {
                p.sessions.get(((localy - 313) / 24) as usize)
            } else {
                None
            };
            if let Some(s) = session {
                return Some((p.name.clone(), s.clone()));
            }
        }
        None
    }
    fn title_at(&self, x: i32, y: i32) -> String {
        self.session_at(x, y)
            .map(|(_, s)| s.task().to_owned())
            .unwrap_or_default()
    }
    unsafe fn show_tip(&mut self, point: POINT) {
        unsafe {
            if self.tip_text == self.tip_pending && IsWindowVisible(self.tip) != 0 {
                return;
            }
            self.tip_text = self.tip_pending.clone();
            let area = work_area(point);
            let width = (580. * self.scale) as i32;
            let canvas = Canvas::new(width, 400, self.scale);
            let font = CreateFontW(
                -(13. * self.scale) as i32,
                0,
                0,
                0,
                400,
                0,
                0,
                0,
                DEFAULT_CHARSET as u32,
                0,
                0,
                0,
                0,
                wide("Segoe UI").as_ptr(),
            );
            let old = SelectObject(canvas.dc, font);
            let mut r = rect(0, 0, width - (20. * self.scale) as i32, 0);
            DrawTextW(
                canvas.dc,
                wide(&self.tip_text).as_ptr(),
                -1,
                &mut r,
                DT_CALCRECT | DT_WORDBREAK | DT_NOPREFIX,
            );
            SelectObject(canvas.dc, old);
            DeleteObject(font);
            let height = (r.bottom + (14. * self.scale) as i32).min(area.bottom - area.top);
            let x = point
                .x
                .clamp(area.left, (area.right - width).max(area.left));
            let y = (point.y + 22).min(area.bottom - height);
            SetWindowPos(
                self.tip,
                HWND_TOPMOST,
                x,
                y,
                width,
                height,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
            InvalidateRect(self.tip, null(), 0);
        }
    }
    unsafe fn menu(&mut self) {
        unsafe {
            self.menu_active = true;
            self.pinned = false;
            self.hide();
            let menu = CreatePopupMenu();
            let appearance = CreatePopupMenu();
            let animation = CreatePopupMenu();
            let color = CreatePopupMenu();
            let panel = CreatePopupMenu();
            let scale = CreatePopupMenu();
            let notifications = CreatePopupMenu();
            add(menu, 1, "Показать состояние", false);
            add(menu, 2, "Открыть папку данных", false);
            for (i, (name, key)) in [
                ("Искра", "Spark"),
                ("Символьный спиннер", "Unicode"),
                ("Краб Clawd", "Clawd"),
                ("Орбита", "Orbit"),
            ]
            .iter()
            .enumerate()
            {
                add(animation, 10 + i, name, self.settings.animation == *key);
            }
            AppendMenuW(
                appearance,
                MF_POPUP,
                animation as usize,
                wide("Анимация").as_ptr(),
            );
            add(
                color,
                20,
                "Цвета установленных агентов",
                self.settings.color_mode == "Orange",
            );
            add(
                color,
                21,
                "Системный (светлый / тёмный)",
                self.settings.color_mode == "System",
            );
            AppendMenuW(
                appearance,
                MF_POPUP,
                color as usize,
                wide("Цвет значка").as_ptr(),
            );
            AppendMenuW(appearance, MF_SEPARATOR, 0, null());
            add(
                appearance,
                30,
                "Показывать таймер",
                self.settings.show_timer,
            );
            add(
                appearance,
                31,
                "Показывать текст состояния",
                self.settings.show_text,
            );
            add(
                appearance,
                32,
                "Отдельные значки Claude и Codex",
                self.settings.separate_icons,
            );
            AppendMenuW(
                menu,
                MF_POPUP,
                appearance as usize,
                wide("Внешний вид").as_ptr(),
            );
            add(
                panel,
                40,
                "Горизонтально",
                self.settings.panel_layout == "Horizontal",
            );
            add(
                panel,
                41,
                "Вертикально",
                self.settings.panel_layout == "Vertical",
            );
            add(panel, 42, "Компактный размер", self.settings.compact_panel);
            for (i, value) in [75, 100, 125, 150].iter().enumerate() {
                add(
                    scale,
                    50 + i,
                    &format!("{value}%"),
                    self.settings.panel_scale == *value,
                );
            }
            AppendMenuW(panel, MF_POPUP, scale as usize, wide("Масштаб").as_ptr());
            AppendMenuW(panel, MF_SEPARATOR, 0, null());
            add(panel, 43, "Показывать Claude", self.settings.show_claude);
            add(panel, 44, "Показывать Codex", self.settings.show_codex);
            if !self.settings.show_codex {
                EnableMenuItem(panel, 43, MF_BYCOMMAND | MF_GRAYED);
            }
            if !self.settings.show_claude {
                EnableMenuItem(panel, 44, MF_BYCOMMAND | MF_GRAYED);
            }
            AppendMenuW(menu, MF_POPUP, panel as usize, wide("Панель").as_ptr());
            add(
                notifications,
                60,
                "Включить уведомления",
                self.settings.notifications,
            );
            AppendMenuW(notifications, MF_SEPARATOR, 0, null());
            add(
                notifications,
                61,
                "Завершение работы",
                self.settings.notify_done,
            );
            add(
                notifications,
                62,
                "Ожидание разрешения",
                self.settings.notify_permission,
            );
            add(
                notifications,
                63,
                "Лимиты 80 / 90 / 100%",
                self.settings.notify_limits,
            );
            add(
                notifications,
                64,
                "Сброс лимита",
                self.settings.notify_reset,
            );
            AppendMenuW(notifications, MF_SEPARATOR, 0, null());
            add(notifications, 65, "Звук", self.settings.sound);
            add(notifications, 66, "Тихий режим", self.settings.quiet);
            AppendMenuW(
                menu,
                MF_POPUP,
                notifications as usize,
                wide("Уведомления").as_ptr(),
            );
            add(menu, 70, "Автозапуск Windows", system::autostart_enabled());
            AppendMenuW(menu, MF_SEPARATOR, 0, null());
            add(menu, 99, "Выход", false);
            let mut point: POINT = zeroed();
            GetCursorPos(&mut point);
            SetForegroundWindow(self.owner);
            let command = TrackPopupMenu(
                menu,
                TPM_RETURNCMD | TPM_RIGHTBUTTON,
                point.x,
                point.y,
                0,
                self.owner,
                null(),
            );
            PostMessageW(self.owner, WM_NULL, 0, 0);
            DestroyMenu(menu);
            self.menu_active = false;
            self.last_hover = Instant::now();
            let old_icons = (
                self.settings.separate_icons,
                self.settings.show_claude,
                self.settings.show_codex,
            );
            match command {
                1 => {
                    self.pinned = true;
                    self.show();
                }
                2 => {
                    ShellExecuteW(
                        null_mut(),
                        wide("open").as_ptr(),
                        wide(&data_dir().to_string_lossy()).as_ptr(),
                        null(),
                        null(),
                        SW_SHOWNORMAL,
                    );
                }
                10..=13 => {
                    self.settings.animation =
                        ["Spark", "Unicode", "Clawd", "Orbit"][(command - 10) as usize].into()
                }
                20 => self.settings.color_mode = "Orange".into(),
                21 => self.settings.color_mode = "System".into(),
                30 => self.settings.show_timer = !self.settings.show_timer,
                31 => self.settings.show_text = !self.settings.show_text,
                32 => self.settings.separate_icons = !self.settings.separate_icons,
                40 => self.settings.panel_layout = "Horizontal".into(),
                41 => self.settings.panel_layout = "Vertical".into(),
                42 => self.settings.compact_panel = !self.settings.compact_panel,
                43 if self.settings.show_codex => {
                    self.settings.show_claude = !self.settings.show_claude
                }
                44 if self.settings.show_claude => {
                    self.settings.show_codex = !self.settings.show_codex
                }
                50..=53 => self.settings.panel_scale = [75, 100, 125, 150][(command - 50) as usize],
                60 => self.settings.notifications = !self.settings.notifications,
                61 => self.settings.notify_done = !self.settings.notify_done,
                62 => self.settings.notify_permission = !self.settings.notify_permission,
                63 => self.settings.notify_limits = !self.settings.notify_limits,
                64 => self.settings.notify_reset = !self.settings.notify_reset,
                65 => self.settings.sound = !self.settings.sound,
                66 => self.settings.quiet = !self.settings.quiet,
                70 => match std::env::current_exe() {
                    Ok(exe) => {
                        if let Err(e) = system::set_autostart(!system::autostart_enabled(), &exe) {
                            error(&e);
                        }
                    }
                    Err(e) => error(&e.to_string()),
                },
                99 => PostQuitMessage(0),
                _ => {}
            }
            if matches!(command,10..=13|20|21|30..=32|40..=44|50..=53|60..=66) {
                if let Err(e) = self.settings.save(&data_dir().join("appearance.json")) {
                    error(&format!("Не удалось сохранить настройки: {e}"));
                }
                self.frame = 0;
                if old_icons
                    != (
                        self.settings.separate_icons,
                        self.settings.show_claude,
                        self.settings.show_codex,
                    )
                {
                    self.rebuild_icons();
                }
                if self.settings.quiet || !self.settings.notifications {
                    self.pending_notices.clear();
                }
            }
        }
    }
}

unsafe fn add(menu: HMENU, id: usize, label: &str, checked: bool) {
    unsafe {
        AppendMenuW(
            menu,
            MF_STRING | if checked { MF_CHECKED } else { 0 },
            id,
            wide(label).as_ptr(),
        );
    }
}
fn inside(p: POINT, r: RECT) -> bool {
    p.x >= r.left && p.x < r.right && p.y >= r.top && p.y < r.bottom
}
unsafe fn work_area(point: POINT) -> RECT {
    unsafe {
        let monitor = MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST);
        let mut info: MONITORINFO = zeroed();
        info.cbSize = size_of::<MONITORINFO>() as u32;
        GetMonitorInfoW(monitor, &mut info);
        info.rcWork
    }
}
impl Drop for App {
    fn drop(&mut self) {
        unsafe {
            KillTimer(self.owner, 1);
            self.notify(NIM_DELETE);
            for (_, icon) in self.icons.drain(..) {
                DestroyIcon(icon);
            }
            DestroyWindow(self.tip);
            DestroyWindow(self.panel);
            DestroyWindow(self.owner);
        }
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
pub fn run(is_demo: bool, screenshot: Option<String>) -> Result<(), String> {
    unsafe {
        if let Some(path) = screenshot {
            let providers = demo();
            let settings = Appearance::load(&data_dir().join("appearance.json"));
            let (width, height) = panel_size(&providers, &settings);
            let canvas = Canvas::new(width, height, 1.);
            draw_panel(&canvas, &providers, &settings);
            return canvas.png(Path::new(&path));
        }
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let mutex = CreateMutexW(null(), 0, wide("Local\\ClaudeWindowsStatusBar").as_ptr());
        if mutex.is_null() {
            return Err("Не удалось создать mutex".into());
        }
        if GetLastError() == ERROR_ALREADY_EXISTS {
            CloseHandle(mutex);
            return Ok(());
        }
        let instance = GetModuleHandleW(null());
        let class = wide("ClaudeWindowsStatusBarRust");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(proc),
            hInstance: instance,
            lpszClassName: class.as_ptr(),
            hCursor: LoadCursorW(null_mut(), IDC_ARROW),
            ..zeroed()
        };
        if RegisterClassW(&wc) == 0 {
            CloseHandle(mutex);
            return Err("Не удалось зарегистрировать окно".into());
        }
        let owner = CreateWindowExW(
            WS_EX_TOOLWINDOW,
            class.as_ptr(),
            wide("Claude + Codex — Rust").as_ptr(),
            0,
            0,
            0,
            0,
            0,
            null_mut(),
            null_mut(),
            instance,
            null(),
        );
        let panel = CreateWindowExW(
            WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            class.as_ptr(),
            wide("Claude + Codex — состояние и лимиты").as_ptr(),
            WS_POPUP,
            0,
            0,
            740,
            358,
            owner,
            null_mut(),
            instance,
            null(),
        );
        let tip = CreateWindowExW(
            WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            class.as_ptr(),
            wide("Название задачи").as_ptr(),
            WS_POPUP,
            0,
            0,
            580,
            50,
            owner,
            null_mut(),
            instance,
            null(),
        );
        if owner.is_null() || panel.is_null() || tip.is_null() {
            CloseHandle(mutex);
            return Err("Не удалось создать окна приложения".into());
        }
        let shared = Arc::new(Mutex::new(if is_demo {
            demo()
        } else {
            vec![
                Provider {
                    name: "Claude".into(),
                    ..Default::default()
                },
                Provider {
                    name: "Codex".into(),
                    ..Default::default()
                },
            ]
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let worker = if is_demo {
            None
        } else {
            let shared = shared.clone();
            let stop = stop.clone();
            Some(thread::spawn(move || {
                let mut codex = Monitor::new(codex_home());
                while !stop.load(Ordering::Relaxed) {
                    let providers = vec![integration::claude(&data_dir()), codex.poll()];
                    *shared.lock().unwrap_or_else(|e| e.into_inner()) = providers;
                    for _ in 0..15 {
                        if stop.load(Ordering::Relaxed) {
                            break;
                        }
                        thread::sleep(Duration::from_millis(100));
                    }
                }
            }))
        };
        let settings = Appearance::load(&data_dir().join("appearance.json"));
        let providers = shared.lock().unwrap().clone();
        let mut app = App {
            owner,
            panel,
            tip,
            icons: Vec::new(),
            icon_signatures: HashMap::new(),
            active_tray: 1,
            notifications: Notifications::default(),
            pending_notices: VecDeque::new(),
            last_notice_at: Instant::now() - Duration::from_secs(10),
            settings,
            installed: installed::detect(),
            last_install_probe: Instant::now(),
            providers,
            shared,
            stop,
            worker,
            demo: is_demo,
            pinned: false,
            visible: false,
            frame: 0,
            last_update: Instant::now(),
            last_hover: Instant::now(),
            hover_point: POINT { x: 0, y: 0 },
            scale: 1.,
            tip_text: String::new(),
            tip_pending: String::new(),
            tip_since: Instant::now(),
            taskbar: RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()),
            menu_active: false,
        };
        TASKBAR_MESSAGE.store(app.taskbar, Ordering::Relaxed);
        app.rebuild_icons();
        SetTimer(owner, 1, 150, None);
        if is_demo {
            app.pinned = true;
            app.show();
        }
        let mut msg: MSG = zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            if msg.message == app.taskbar || msg.message == QUEUED_TASKBAR {
                TASKBAR_MESSAGE.store(app.taskbar, Ordering::Relaxed);
                app.notify(NIM_ADD);
                continue;
            }
            match msg.message {
                WM_PAINT if msg.hwnd == panel || msg.hwnd == tip => {
                    app.paint(msg.hwnd);
                    continue;
                }
                WM_TIMER if msg.hwnd == owner => {
                    app.tick();
                    continue;
                }
                CALLBACK | QUEUED_CALLBACK => {
                    let id = ((msg.lParam as u32) >> 16) & 0xffff;
                    if id > 0 {
                        app.active_tray = id;
                    }
                    let event = (msg.lParam as u32) & 0xffff;
                    match event {
                        WM_CONTEXTMENU => app.menu(),
                        NIN_SELECT | NIN_KEYSELECT => {
                            app.pinned = !app.pinned;
                            if app.pinned { app.show() } else { app.hide() }
                        }
                        WM_MOUSEMOVE | NIN_POPUPOPEN => {
                            GetCursorPos(&mut app.hover_point);
                            app.last_hover = Instant::now();
                            app.show();
                        }
                        NIN_POPUPCLOSE => app.last_hover = Instant::now(),
                        _ => {}
                    }
                    continue;
                }
                _ => {}
            }
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        drop(app);
        CloseHandle(mutex);
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_icon_styles_have_pixels() {
        for style in ["Spark", "Unicode", "Clawd", "Orbit"] {
            let a = Appearance {
                animation: style.into(),
                ..Default::default()
            };
            for frame in [0, 5] {
                let pixels = icon_pixels(&a, "permission", frame);
                assert_eq!(pixels.len(), 4096);
                assert!(pixels.chunks(4).any(|p| p[3] > 0));
            }
        }
    }
    #[test]
    fn panel_layout_visibility_and_compact_size() {
        let providers = demo();
        let mut settings = Appearance::default();
        assert_eq!(panel_size(&providers, &settings), (740, 358));
        settings.panel_layout = "Vertical".into();
        assert_eq!(panel_size(&providers, &settings), (370, 716));
        settings.compact_panel = true;
        assert_eq!(panel_size(&providers, &settings), (370, 584));
        settings.show_claude = false;
        assert_eq!(panel_size(&providers, &settings), (370, 292));
        let canvas = unsafe { Canvas::new(370, 292, 1.) };
        draw_panel(&canvas, &providers, &settings);
        let mut utf16 = [0u16; 4];
        copy_wide(&mut utf16, "😀😀");
        assert!(utf16[0] >= 0xd800 && utf16[0] <= 0xdbff);
        assert_eq!(utf16[2], 0);
    }
    #[test]
    fn installed_palette_splits_every_animation_at_the_middle() {
        let styles = ["Spark", "Unicode", "Clawd", "Orbit"];
        for style in styles {
            for frame in [0, 5] {
                for (mode, left_red, right_red) in [
                    ("Orange", true, true),
                    ("Codex", false, false),
                    ("Split", true, false),
                ] {
                    let settings = Appearance {
                        animation: style.into(),
                        color_mode: mode.into(),
                        ..Default::default()
                    };
                    let pixels = icon_pixels(&settings, "thinking", frame);
                    let (mut left, mut right) = (0, 0);
                    for (index, pixel) in pixels.chunks_exact(4).enumerate() {
                        if pixel[3] == 0 {
                            continue;
                        }
                        let x = index % 32;
                        let red = if x < 16 {
                            left += 1;
                            left_red
                        } else {
                            right += 1;
                            right_red
                        };
                        assert_eq!(pixel[2] > pixel[0], red, "{style} {mode} x={x}");
                    }
                    assert!(left > 0 && right > 0);
                }
            }
        }
        if let Some(path) = std::env::var_os("CWSB_ICON_PREVIEW") {
            let canvas = unsafe { Canvas::new(160, 48, 1.) };
            canvas.fill(0, 0, 160, 48, rgb(28, 28, 32));
            for (index, style) in styles.iter().enumerate() {
                let settings = Appearance {
                    animation: (*style).into(),
                    color_mode: "Split".into(),
                    ..Default::default()
                };
                let pixels = icon_pixels(&settings, "thinking", 0);
                for y in 0..32 {
                    for x in 0..32 {
                        let source = &pixels[(y * 32 + x) * 4..(y * 32 + x + 1) * 4];
                        let destination = ((y + 8) * 160 + index * 40 + x + 4) * 4;
                        for (channel, background) in [32u32, 28, 28].iter().enumerate() {
                            unsafe {
                                *canvas.pixels.add(destination + channel) = (source[channel] as u32
                                    + background * (255 - source[3] as u32) / 255)
                                    as u8;
                            }
                        }
                    }
                }
            }
            canvas.png(Path::new(&path)).unwrap();
        }
    }
    #[test]
    fn mouse_title_hit_and_height() {
        assert_eq!(panel_size(&demo(), &Appearance::default()), (740, 358));
        assert!(inside(POINT { x: 3, y: 4 }, rect(0, 0, 10, 10)));
    }
}
