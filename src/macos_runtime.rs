#![cfg(target_os = "macos")]
#![allow(non_snake_case, unused_unsafe, deprecated)]

use std::cell::RefCell;
use std::path::PathBuf;
use std::process::Command;
use std::ptr::NonNull;
use std::rc::Rc;
use std::time::Instant;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{define_class, msg_send, AnyThread, ClassType, MainThreadOnly};
use objc2_app_kit::{
    NSAlert, NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSBezierPath,
    NSColor, NSEvent, NSEventMask, NSEventModifierFlags, NSFont, NSImage, NSImageView, NSMenu,
    NSMenuDelegate, NSMenuItem, NSPanel, NSStatusBar, NSWindowCollectionBehavior,
    NSWindowStyleMask,
};
use objc2_foundation::{
    MainThreadMarker, NSBundle, NSObject, NSObjectProtocol, NSPoint,
    NSRect, NSRunLoop, NSRunLoopMode, NSSize, NSString, NSTimer,
};

use crate::macos_assets::{make_image_view, Anchor, SpriteAssets};
use crate::behavior::{BehaviorContext, BehaviorScript, Dir, Side, State, Surface, SurfaceEdge, Transition};
use crate::config::{make_shared, Config, SharedConfig};
use crate::engine::{advance_anim, vertical_offset};
use crate::manifest;
use crate::demo_behavior::DemoBehavior;
use crate::physics;
use crate::terrestrial_behavior::TerrestrialBehavior;
use crate::sprite_map::{sprite_for_state, sprite_for_turn};
use crate::macos_wm::{self, ScreenInfo, WinInfo};

// ---- FFI ----

#[link(name = "Foundation", kind = "framework")]
unsafe extern "C" {
    static NSRunLoopCommonModes: *const std::ffi::c_void;
}

// ---- Surface helpers ----

/// Returns the CGWindowID of the window this surface is anchored to, if any.
fn surface_host_win_id(surface: &Surface) -> Option<u32> {
    match surface {
        Surface::WindowTop { win_id, .. }
        | Surface::WindowWall { win_id, .. }
        | Surface::WindowUpperCorner { win_id, .. }
        | Surface::WindowBottom { win_id, .. } => Some(*win_id),
        _ => None,
    }
}

// ---- Window-list tiered cache ----

/// Reusable window-list buffer with a countdown-based refresh schedule.
///
/// The refresh interval is set after each full enumeration based on how close
/// any character is to a window:
///
/// - **IMMEDIATE** (1 tick): any character is in a dynamic state
///   (Falling, JumpRunup) — need fresh surface data every tick.
/// - **HIGH_FREQ** (15 ticks ≈ 1.5 s): a character is on a window surface
///   or within `attract_dist` of one — keep data reasonably fresh while
///   window dragging / scrolling occurs.
/// - **LOW_FREQ** (150 ticks ≈ 15 s): all characters are on the Desktop
///   with no windows nearby — window positions rarely change in this state.
struct WinListCache {
    wins: Vec<WinInfo>,
    /// Remaining ticks before the next full enumeration.
    ticks_until_refresh: u32,
}

impl WinListCache {
    fn new() -> Self {
        Self { wins: Vec::new(), ticks_until_refresh: 0 }
    }
}

const WIN_CACHE_IMMEDIATE: u32 = 1;
const WIN_CACHE_HIGH_FREQ: u32 = 5;
const WIN_CACHE_LOW_FREQ: u32 = 150;

/// Determines the next refresh interval based on character states and
/// proximity to windows.
fn next_refresh_interval(chars: &[CharState], wins: &[WinInfo], attract_dist: f64) -> u32 {
    use crate::behavior::State;
    for ch in chars {
        // Dynamic states require a fresh window list every tick.
        if matches!(&ch.anim_state, State::Falling { .. } | State::JumpRunup { .. }) {
            return WIN_CACHE_IMMEDIATE;
        }
        // Characters on a window surface need high-frequency refresh so that
        // the rendered position tracks window movement within ~1.5 s.
        if surface_host_win_id(&ch.surface).is_some() {
            return WIN_CACHE_HIGH_FREQ;
        }
    }
    // Desktop characters: check proximity to any window.
    for ch in chars {
        if let Surface::Desktop { x } = &ch.surface {
            for win in wins {
                let dist_r = win.x - x;
                let dist_l = x - win.right();
                if (dist_r >= 0.0 && dist_r < attract_dist)
                    || (dist_l >= 0.0 && dist_l < attract_dist)
                {
                    return WIN_CACHE_HIGH_FREQ;
                }
            }
        }
    }
    WIN_CACHE_LOW_FREQ
}

// ---- Per-character state ----

/// Snapshot of the values that were passed to Cocoa last tick.
/// Used to skip redundant `swap_sprite` / `setFrameOrigin` / `orderWindow` calls.
#[derive(PartialEq)]
struct RenderedState {
    sprite_name:      String,
    mirror:           bool,
    origin:           (f64, f64),
    y_offset:         f64,
    z_host_id:        Option<u32>,
    /// The CGWindowID of the first foreign window above the host in z-order,
    /// or None when the host is already at the front of all foreign windows.
    /// Mirrors Windows' GW_HWNDPREV logic: only re-order when this changes.
    z_above_host_id:  Option<u32>,
}

struct CharState {
    panel: Retained<NSPanel>,
    /// NSImageView reused for all sprite swaps — only `setImage:` is called per
    /// frame change; `setContentSize` + `setFrameSize` fire only on size change
    /// (state transitions, not within an animation loop).
    image_view: Retained<NSImageView>,
    assets: Rc<SpriteAssets>,
    config: SharedConfig,
    /// Cached effective config: `config.current` with personality applied.
    /// Recomputed only when `params.toml` or `behavior.toml` change is detected
    /// (checked on the window-list refresh boundary, not every tick).
    effective_config: Config,
    behavior: Box<dyn BehaviorScript>,
    anim_state: State,
    facing: Dir,
    surface: Surface,
    /// Character position in CG coordinates (origin = screen top-left, Y down).
    /// Used ONLY for Airborne free-flight physics. For the last rendered
    /// position use `last_screen_pos`.
    char_pos: (f64, f64),
    last_tick: Instant,
    /// Mouse offset from panel origin in NS coords when dragging; None otherwise.
    drag_offset: Option<(f64, f64)>,
    /// Pending debug forced transition: (target_state, remaining_countdown_secs).
    debug_trigger: Option<(State, f64)>,
    speech_engine: crate::speech::SpeechEngine,
    behavior_engine: crate::anim_trigger::BehaviorEngine,
    /// Active speech bubble state; None when no bubble is shown.
    bubble_state: Option<crate::speech::BubbleState>,
    /// The transparent NSPanel that renders the speech bubble; None when hidden.
    bubble_panel: Option<Retained<NSPanel>>,
    /// Width of the sprite rendered last tick (scaled display pixels).
    /// Used by the resting-overlap nudge to compute the correct at_edge buffer.
    last_sprite_w: f64,
    /// Size of the panel / NSImageView as last sent to Cocoa (width, height).
    /// Cached to avoid redundant `setContentSize` + `setFrameSize` calls.
    last_panel_sz: (f64, f64),
    /// Last rendered sprite top-left in CG coordinates (origin = screen top-left, Y down).
    /// Updated every tick (all surfaces). Used for surface-lost fallback and jump trajectory.
    last_screen_pos: (f64, f64),
    /// Snapshot of what was last sent to Cocoa. None on first tick → forces full render.
    last_rendered: Option<RenderedState>,
}

// ---- App-wide state (singletons) ----

struct AppState {
    chars: Vec<CharState>,
    bd_assets: Rc<SpriteAssets>,
    pt_assets: Rc<SpriteAssets>,
    lg_assets: Rc<SpriteAssets>,
    bd_config: SharedConfig,
    pt_config: SharedConfig,
    lg_config: SharedConfig,
    _menu_handler: Retained<MenuDelegate>,
    _status_item: Retained<objc2_app_kit::NSStatusItem>,
    _timer: Retained<NSTimer>,
    /// Keep event monitors alive for the lifetime of the app.
    _event_monitors: Vec<Retained<AnyObject>>,
    /// Character index whose debug menu is currently being shown.
    debug_menu_char: usize,
    /// Target states stored between menu construction and item selection.
    debug_menu_targets: Vec<State>,
    /// Global speech lock countdown (seconds). Prevents overlapping speech.
    speech_lock_remaining: f64,
    speech_cfg: crate::user_config::SpeechConfig,
    speech_tick: Instant,
    /// Font size for speech bubbles (from user.toml).
    font_size: f64,
    /// Character display size in logical pixels (from user.toml `sprite_size`).
    /// Single source of truth for physics clamping and collision spacing.
    sprite_size: f64,
    /// Resolved display language: "ja" or "en".
    lang: String,
    /// Shared weather cache updated by the background weather thread.
    weather: crate::weather::WeatherHandle,
    /// Weather configuration from user.toml (city/coordinates for menu display).
    weather_cfg: crate::user_config::WeatherConfig,
    /// Reusable window-list cache with tiered refresh schedule.
    win_cache: WinListCache,
    /// fps configured for AC power (from user.toml).
    tick_rate_ac: u32,
    /// fps configured for battery power (from user.toml).
    tick_rate_battery: u32,
    /// Active animation fps (resolved from tick_rate_ac/battery at startup and
    /// updated by the IOKit power-source notification callback). Base timer fires
    /// at 60 Hz; ticks are skipped until 1/tick_rate seconds have elapsed.
    tick_rate: u32,
    /// Timestamp of the last physics/rendering tick. Used for rate limiting.
    last_full_tick: Instant,
}

thread_local! {
    static APP: RefCell<Option<AppState>> = RefCell::new(None);
}

/// IOKit power-source notification callback. Runs on the main thread.
unsafe extern "C" fn power_source_changed(_: *mut std::ffi::c_void) {
    APP.with(|cell| {
        let mut b = cell.borrow_mut();
        let Some(app) = b.as_mut() else { return };
        app.tick_rate = if crate::power::on_ac_power() {
            app.tick_rate_ac
        } else {
            app.tick_rate_battery
        }.max(1);
    });
}

// ---- Panel helpers ----

fn make_char_panel(
    init_img: &NSImage,
    mt: MainThreadMarker,
) -> (Retained<NSPanel>, Retained<NSImageView>) {
    let iv = make_image_view(init_img, mt);
    let sz = unsafe { init_img.size() };
    let panel = unsafe {
        let p = NSPanel::initWithContentRect_styleMask_backing_defer(
            NSPanel::alloc(mt),
            NSRect::new(NSPoint::ZERO, sz),
            NSWindowStyleMask::from_bits_retain(128), // Borderless | NonactivatingPanel
            NSBackingStoreType::Buffered,
            false,
        );
        p.setBackgroundColor(Some(&NSColor::clearColor()));
        p.setOpaque(false);
        p.setHasShadow(false);
        p.setLevel(0); // NSNormalWindowLevel — lets other windows occlude the character
        p.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::FullScreenAuxiliary,
        );
        p.setIgnoresMouseEvents(true);
        p.setAlphaValue(1.0);
        p.setContentView(Some(&*iv));
        p
    };
    (panel, iv)
}

/// Update the sprite shown in the character's reusable NSImageView.
/// `setImage:` fires on every sprite change. `setContentSize` and `setFrameSize`
/// fire only when the sprite dimensions change (state transitions, not frame loops).
fn swap_sprite(
    image_view: &NSImageView,
    panel: &NSPanel,
    assets: &SpriteAssets,
    name: &str,
    mirror: bool,
    last_sz: &mut (f64, f64),
) {
    if let Some(img) = assets.image(name, mirror) {
        let new_sz = unsafe { img.size() };
        unsafe { image_view.setImage(Some(img)); }
        let new_sz_pair = (new_sz.width, new_sz.height);
        if new_sz_pair != *last_sz {
            unsafe {
                panel.setContentSize(new_sz);
                let _: () = msg_send![image_view, setFrameSize: new_sz];
            }
            *last_sz = new_sz_pair;
        }
    }
}

// ---- Speech bubble rendering ----

const BUBBLE_PADDING: f64 = 12.0;
const BUBBLE_CORNER:  f64 = 8.0;
const BUBBLE_TAIL_H:  f64 = 10.0;
const BUBBLE_TAIL_W:  f64 = 14.0;
const BUBBLE_MAX_W:   f64 = 240.0;
const BUBBLE_MIN_W:   f64 = 60.0;
const BUBBLE_MARGIN:  f64 = 8.0;

/// Render speech text into an `NSImage` shaped as a rounded-rect speech bubble.
///
/// `tail_at_bottom` — the tail points downward when the bubble sits *above* the
/// character and upward when it sits *below*.
fn make_bubble_image(text: &str, tail_at_bottom: bool, font_size: f64) -> Retained<NSImage> {
    unsafe {
        use objc2::runtime::AnyClass;

        let ns_text = NSString::from_str(text);
        let font    = NSFont::systemFontOfSize(font_size);
        // NSMutableParagraphStyle: word-wrap mode
        let para_cls = AnyClass::get(c"NSMutableParagraphStyle").unwrap();
        let para: *mut AnyObject = msg_send![para_cls, new];
        let _: () = msg_send![para, setLineBreakMode: 0u64]; // NSLineBreakByWordWrapping = 0
        let text_color = NSColor::colorWithWhite_alpha(0.15, 1.0);

        // Build NSDictionary of text attributes via msg_send.
        let dict_cls = AnyClass::get(c"NSMutableDictionary").unwrap();
        let attrs: *mut AnyObject = msg_send![dict_cls, new];
        let _: () = msg_send![attrs, setObject: &*font       forKey: &*NSString::from_str("NSFont")];
        let _: () = msg_send![attrs, setObject: para         forKey: &*NSString::from_str("NSParagraphStyle")];
        let _: () = msg_send![attrs, setObject: &*text_color forKey: &*NSString::from_str("NSForegroundColor")];

        // Build NSMutableAttributedString.
        let mas_cls = AnyClass::get(c"NSMutableAttributedString").unwrap();
        let ms: *mut AnyObject = msg_send![mas_cls, alloc];
        let ms: *mut AnyObject = msg_send![ms, initWithString: &*ns_text];
        // NSRange expects UTF-16 code unit count, not UTF-8 byte count.
        let full_range = objc2_foundation::NSRange::new(0, ns_text.len_utf16());
        let _: () = msg_send![ms, setAttributes: attrs range: full_range];

        // Measure text.
        let constraint = NSSize::new(BUBBLE_MAX_W - BUBBLE_PADDING * 2.0, 1000.0);
        let bounds: NSRect = msg_send![
            ms,
            boundingRectWithSize: constraint
            options: 1u64
            context: std::ptr::null_mut::<AnyObject>()
        ];
        let text_w = bounds.size.width.ceil().max(1.0);
        let text_h = bounds.size.height.ceil().max(1.0);

        let bubble_w = (text_w + BUBBLE_PADDING * 2.0).max(BUBBLE_MIN_W);
        let bubble_h = text_h + BUBBLE_PADDING * 2.0;
        let total_h  = bubble_h + BUBBLE_TAIL_H;

        // Y origin of the body in the image (NS coords = Y-up from bottom).
        let body_y = if tail_at_bottom { BUBBLE_TAIL_H } else { 0.0 };

        let img = NSImage::initWithSize(NSImage::alloc(), NSSize::new(bubble_w, total_h));
        img.lockFocus();

        // Build a single combined outer-contour path (rounded rect + tail) so
        // that fill and stroke are applied uniformly to the whole shape.  This
        // eliminates the visible seam line at the rect/tail junction and gives
        // the tail sides the same border as the rest of the bubble.
        //
        // Both branches trace the outer contour counter-clockwise (CCW) in NS
        // Y-up coordinates, which keeps the shape interior on the left.
        let cx = bubble_w / 2.0;
        let r  = BUBBLE_CORNER;
        let outer = NSBezierPath::bezierPath();

        if tail_at_bottom {
            // Start at right tail base, go CCW around the whole shape.
            outer.moveToPoint(NSPoint::new(cx + BUBBLE_TAIL_W / 2.0, BUBBLE_TAIL_H));
            outer.lineToPoint(NSPoint::new(bubble_w - r, BUBBLE_TAIL_H));
            // Bottom-right arc: 270° → 0°, CCW
            let _: () = msg_send![&*outer,
                appendBezierPathWithArcWithCenter: NSPoint::new(bubble_w - r, BUBBLE_TAIL_H + r)
                radius: r  startAngle: 270.0_f64  endAngle: 0.0_f64  clockwise: false];
            outer.lineToPoint(NSPoint::new(bubble_w, BUBBLE_TAIL_H + bubble_h - r));
            // Top-right arc: 0° → 90°, CCW
            let _: () = msg_send![&*outer,
                appendBezierPathWithArcWithCenter: NSPoint::new(bubble_w - r, BUBBLE_TAIL_H + bubble_h - r)
                radius: r  startAngle: 0.0_f64  endAngle: 90.0_f64  clockwise: false];
            outer.lineToPoint(NSPoint::new(r, BUBBLE_TAIL_H + bubble_h));
            // Top-left arc: 90° → 180°, CCW
            let _: () = msg_send![&*outer,
                appendBezierPathWithArcWithCenter: NSPoint::new(r, BUBBLE_TAIL_H + bubble_h - r)
                radius: r  startAngle: 90.0_f64  endAngle: 180.0_f64  clockwise: false];
            outer.lineToPoint(NSPoint::new(0.0, BUBBLE_TAIL_H + r));
            // Bottom-left arc: 180° → 270°, CCW
            let _: () = msg_send![&*outer,
                appendBezierPathWithArcWithCenter: NSPoint::new(r, BUBBLE_TAIL_H + r)
                radius: r  startAngle: 180.0_f64  endAngle: 270.0_f64  clockwise: false];
            outer.lineToPoint(NSPoint::new(cx - BUBBLE_TAIL_W / 2.0, BUBBLE_TAIL_H));
            outer.lineToPoint(NSPoint::new(cx, 0.0));
            // closePath draws back to the start (right tail base).
        } else {
            // tail_at_top: start at right tail base, go CCW.
            outer.moveToPoint(NSPoint::new(cx + BUBBLE_TAIL_W / 2.0, bubble_h));
            outer.lineToPoint(NSPoint::new(cx, bubble_h + BUBBLE_TAIL_H));
            outer.lineToPoint(NSPoint::new(cx - BUBBLE_TAIL_W / 2.0, bubble_h));
            outer.lineToPoint(NSPoint::new(r, bubble_h));
            // Top-left arc: 90° → 180°, CCW
            let _: () = msg_send![&*outer,
                appendBezierPathWithArcWithCenter: NSPoint::new(r, bubble_h - r)
                radius: r  startAngle: 90.0_f64  endAngle: 180.0_f64  clockwise: false];
            outer.lineToPoint(NSPoint::new(0.0, r));
            // Bottom-left arc: 180° → 270°, CCW
            let _: () = msg_send![&*outer,
                appendBezierPathWithArcWithCenter: NSPoint::new(r, r)
                radius: r  startAngle: 180.0_f64  endAngle: 270.0_f64  clockwise: false];
            outer.lineToPoint(NSPoint::new(bubble_w - r, 0.0));
            // Bottom-right arc: 270° → 0°, CCW
            let _: () = msg_send![&*outer,
                appendBezierPathWithArcWithCenter: NSPoint::new(bubble_w - r, r)
                radius: r  startAngle: 270.0_f64  endAngle: 0.0_f64  clockwise: false];
            outer.lineToPoint(NSPoint::new(bubble_w, bubble_h - r));
            // Top-right arc: 0° → 90°, CCW
            let _: () = msg_send![&*outer,
                appendBezierPathWithArcWithCenter: NSPoint::new(bubble_w - r, bubble_h - r)
                radius: r  startAngle: 0.0_f64  endAngle: 90.0_f64  clockwise: false];
            outer.lineToPoint(NSPoint::new(cx + BUBBLE_TAIL_W / 2.0, bubble_h));
        }
        outer.closePath();

        // Fill then stroke the single combined path — no seam at the junction.
        let bg = NSColor::colorWithWhite_alpha(1.0, 0.93);
        bg.setFill();
        outer.fill();
        NSColor::colorWithWhite_alpha(0.70, 0.7).setStroke();
        outer.setLineWidth(0.5);
        outer.stroke();

        // Text.
        let text_rect = NSRect::new(
            NSPoint::new(
                (bubble_w - text_w) / 2.0,
                body_y + (bubble_h - text_h) / 2.0,
            ),
            NSSize::new(text_w, text_h),
        );
        let _: () = msg_send![
            ms,
            drawWithRect: text_rect
            options: 1u64
            context: std::ptr::null_mut::<AnyObject>()
        ];
        img.unlockFocus();
        img
    }
}

/// Create or update the speech-bubble `NSPanel` for a character.
///
/// `char_ns_frame` — the character panel's frame in NS (Cocoa) screen coords.
fn show_bubble(
    existing: Option<&Retained<NSPanel>>,
    text: &str,
    font_size: f64,
    char_ns_frame: NSRect,
    si: &ScreenInfo,
    mt: MainThreadMarker,
) -> Retained<NSPanel> {
    // Choose placement: above the character if space allows, else below.
    let est_h = 60.0 + BUBBLE_TAIL_H;
    let tail_at_bottom =
        char_ns_frame.origin.y + char_ns_frame.size.height + est_h + BUBBLE_MARGIN < si.height;

    let img    = make_bubble_image(text, tail_at_bottom, font_size);
    let img_sz = unsafe { img.size() };

    let cx       = char_ns_frame.origin.x + char_ns_frame.size.width / 2.0;
    let bubble_x = (cx - img_sz.width / 2.0).max(0.0).min(si.width - img_sz.width);
    let bubble_y = if tail_at_bottom {
        char_ns_frame.origin.y + char_ns_frame.size.height + BUBBLE_MARGIN
    } else {
        char_ns_frame.origin.y - img_sz.height - BUBBLE_MARGIN
    };

    let origin = NSPoint::new(bubble_x, bubble_y);

    if let Some(p) = existing {
        unsafe {
            p.setContentSize(img_sz);
            p.setContentView(Some(&*make_image_view(&img, mt)));
            p.setFrameOrigin(origin);
        }
        p.clone()
    } else {
        unsafe {
            let p = NSPanel::initWithContentRect_styleMask_backing_defer(
                NSPanel::alloc(mt),
                NSRect::new(origin, img_sz),
                NSWindowStyleMask::from_bits_retain(128),
                NSBackingStoreType::Buffered,
                false,
            );
            p.setBackgroundColor(Some(&NSColor::clearColor()));
            p.setOpaque(false);
            p.setHasShadow(false);
            // Level 0 = NSNormalWindowLevel, same as the character panel.
            // The tick loop repositions the bubble just above the character
            // panel each frame via orderWindow:Above:relativeTo:.
            p.setLevel(0);
            p.setCollectionBehavior(
                NSWindowCollectionBehavior::CanJoinAllSpaces
                    | NSWindowCollectionBehavior::FullScreenAuxiliary,
            );
            p.setIgnoresMouseEvents(true);
            p.setAlphaValue(1.0);
            p.setContentView(Some(&*make_image_view(&img, mt)));
            p.orderFront(None);
            p
        }
    }
}

// ---- Status item ----

/// Returns a human-readable location string for the status menu info item.
fn status_location_label(
    cfg: &crate::user_config::WeatherConfig,
    geo: &crate::weather::GeoStatus,
    ja: bool,
) -> String {
    use crate::weather::GeoStatus;
    let body = if let Some(city) = &cfg.city {
        let suffix = match geo {
            GeoStatus::Ok          => " \u{2713}",  // ✓
            GeoStatus::Resolving   => if ja { " 解決中..." } else { " resolving..." },
            GeoStatus::NotFound    => if ja { " 見つかりません" } else { " not found" },
            GeoStatus::Unavailable => if ja { " 利用不可" } else { " unavailable" },
        };
        format!("{}{}", city, suffix)
    } else if let (Some(lat), Some(lon)) = (cfg.latitude, cfg.longitude) {
        format!("{:.2}\u{00b0}, {:.2}\u{00b0}", lat, lon)
    } else {
        (if ja { "未設定" } else { "not configured" }).to_string()
    };
    format!("\u{1f4cd} {}", body)  // 📍
}

fn make_status_item(
    handler: &MenuDelegate,
    mt: MainThreadMarker,
    lang: &str,
    weather_cfg: &crate::user_config::WeatherConfig,
) -> Retained<objc2_app_kit::NSStatusItem> {
    let ja = lang == "ja";
    unsafe {
        let bar = NSStatusBar::systemStatusBar();
        let item = bar.statusItemWithLength(-2.0); // NSSquareStatusItemLength
        // Fix autosave name to a stable string so Control Center does not
        // create a new entry on every launch (PID-based auto-generated names).
        let autosave_name = NSString::from_str("PetitMates");
        let (): () = objc2::msg_send![&*item, setAutosaveName: &*autosave_name];
        if let Some(btn) = item.button(mt) {
            if let Some(img) = NSImage::imageWithSystemSymbolName_accessibilityDescription(
                &NSString::from_str("lizard.fill"),
                None,
            ) {
                img.setTemplate(true);
                btn.setImage(Some(&img));
            }
        }
        let menu = NSMenu::init(NSMenu::alloc(mt));
        // Disable auto-enable so menuWillOpen: has full manual control.
        let (): () = unsafe { objc2::msg_send![&*menu, setAutoenablesItems: false] };

        // Character management items.
        let add_bd = NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mt),
            &NSString::from_str(if ja { "フトアゴを追加" } else { "Add Bearded Dragon" }),
            Some(objc2::sel!(addBeardedDragon:)),
            &NSString::from_str(""),
        );
        let (): () = unsafe { objc2::msg_send![&*add_bd, setTarget: handler] };
        menu.addItem(&add_bd);

        let add_pt = NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mt),
            &NSString::from_str(if ja { "クサガメを追加" } else { "Add Pond Turtle" }),
            Some(objc2::sel!(addPondTurtle:)),
            &NSString::from_str(""),
        );
        let (): () = unsafe { objc2::msg_send![&*add_pt, setTarget: handler] };
        menu.addItem(&add_pt);

        let add_lg = NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mt),
            &NSString::from_str(if ja { "レオパを追加" } else { "Add Leopard Gecko" }),
            Some(objc2::sel!(addLeopardGecko:)),
            &NSString::from_str(""),
        );
        let (): () = unsafe { objc2::msg_send![&*add_lg, setTarget: handler] };
        menu.addItem(&add_lg);

        let remove_item = NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mt),
            &NSString::from_str(if ja { "最後のキャラクターを削除" } else { "Remove Last" }),
            Some(objc2::sel!(removeCharacter:)),
            &NSString::from_str(""),
        );
        let (): () = unsafe { objc2::msg_send![&*remove_item, setTarget: handler] };
        let (): () = unsafe { objc2::msg_send![&*remove_item, setTag: 1_isize] };
        menu.addItem(&remove_item);
        let (): () = unsafe { objc2::msg_send![&*menu, setDelegate: handler] };

        menu.addItem(&NSMenuItem::separatorItem(mt));

        let settings = NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mt),
            &NSString::from_str(if ja { "設定…" } else { "Settings…" }),
            Some(objc2::sel!(openSettings:)),
            &NSString::from_str(""),
        );
        let (): () = unsafe { objc2::msg_send![&*settings, setTarget: handler] };
        let (): () = unsafe { objc2::msg_send![&*settings, setTag: 2_isize] };
        menu.addItem(&settings);

        menu.addItem(&NSMenuItem::separatorItem(mt));

        // Non-interactive info items: location and weather (shown only when
        // weather fetching is enabled). Titles are refreshed in menuWillOpen:.
        if weather_cfg.enabled {
            let initial_geo = if weather_cfg.latitude.is_some() && weather_cfg.longitude.is_some() {
                crate::weather::GeoStatus::Ok
            } else {
                crate::weather::GeoStatus::Resolving
            };
            let loc_str = status_location_label(weather_cfg, &initial_geo, ja);
            let location_item = NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mt),
                &NSString::from_str(&loc_str),
                None,
                &NSString::from_str(""),
            );
            let (): () = unsafe { objc2::msg_send![&*location_item, setEnabled: false] };
            let (): () = unsafe { objc2::msg_send![&*location_item, setTag: 10_isize] };
            menu.addItem(&location_item);

            let weather_item = NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mt),
                &NSString::from_str("\u{2500}"),  // ─ placeholder, updated in menuWillOpen:
                None,
                &NSString::from_str(""),
            );
            let (): () = unsafe { objc2::msg_send![&*weather_item, setEnabled: false] };
            let (): () = unsafe { objc2::msg_send![&*weather_item, setTag: 11_isize] };
            menu.addItem(&weather_item);

            menu.addItem(&NSMenuItem::separatorItem(mt));
        }

        let about = NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mt),
            &NSString::from_str(if ja { "Petit Mates について" } else { "About Petit Mates" }),
            Some(objc2::sel!(orderFrontStandardAboutPanel:)),
            &NSString::from_str(""),
        );
        menu.addItem(&about);
        menu.addItem(&NSMenuItem::separatorItem(mt));

        let quit = NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mt),
            &NSString::from_str(if ja { "終了" } else { "Quit" }),
            Some(objc2::sel!(terminate:)),
            &NSString::from_str("q"),
        );
        menu.addItem(&quit);
        item.setMenu(Some(&menu));
        item
    }
}

// ---- Asset directory ----

pub fn char_dir_for(name: &str) -> Option<PathBuf> {
    let rel = format!("assets/{name}");
    let bundle_path = unsafe {
        let bundle = NSBundle::mainBundle();
        bundle
            .resourceURL()
            .and_then(|base| {
                let r = NSString::from_str(&rel);
                base.URLByAppendingPathComponent(&r)
            })
            .and_then(|url| url.path())
            .map(|p| PathBuf::from(p.to_string()))
            .filter(|p| p.exists())
    };
    if let Some(p) = bundle_path {
        return Some(p);
    }
    let exe = std::env::current_exe().ok()?;
    exe.parent()?
        .join(format!("../../assets/{name}"))
        .canonicalize()
        .ok()
}

// ---- Position persistence ----

/// Create one character, place it in a startup-drop position, and return
/// the fully initialised `CharState`.  Caller is responsible for pushing it
/// into `AppState::chars`.
/// Compute the effective config (base `params.toml` values with the
/// character's current personality applied on top).
fn compute_effective_config(
    config: &SharedConfig,
    engine: &crate::anim_trigger::BehaviorEngine,
) -> Config {
    let mut cfg = config.lock().unwrap().current.clone();
    crate::config::apply_personality(&mut cfg, engine.personality());
    cfg
}

fn spawn_char(assets: Rc<SpriteAssets>, config: SharedConfig, si: &ScreenInfo, mt: MainThreadMarker, demo: bool, char_name: &str) -> CharState {
    let (start_cx, start_cy) = physics::startup_drop(&si.physics_screen(), assets.size("s-stand", false).0, si.menu_bar_height);
    let init_img = assets.image("s-stand", false).expect("s-stand.png missing");
    let (panel, image_view) = make_char_panel(init_img, mt);
    let init_sz = unsafe { init_img.size() };
    unsafe {
        panel.setFrameOrigin(NSPoint::new(start_cx, si.height - start_cy - init_sz.height));
        panel.orderFront(None);
    }
    let behavior_engine = {
        let behavior_data = crate::anim_trigger::load(char_name);
        let watch_path = char_dir_for(char_name).map(|d| d.join("behavior.toml"));
        let engine = crate::anim_trigger::BehaviorEngine::new(behavior_data, &assets.animations);
        if let Some(p) = watch_path { engine.with_personality_path(p) } else { engine }
    };
    let speech_engine = crate::speech::SpeechEngine::new(crate::speech::load(char_name));
    let effective_config = compute_effective_config(&config, &behavior_engine);
    CharState {
        panel,
        image_view,
        assets,
        config,
        effective_config,
        behavior: if demo {
            Box::new(DemoBehavior::new()) as Box<dyn crate::behavior::BehaviorScript>
        } else {
            Box::new(TerrestrialBehavior::new())
        },
        anim_state: State::Falling { vx: 0.0, vy: 0.0, shocked: 0.0 },
        facing: Dir::Left,
        surface: Surface::Airborne,
        char_pos: (start_cx, start_cy),
        last_tick: Instant::now(),
        drag_offset: None,
        debug_trigger: None,
        speech_engine,
        behavior_engine,
        bubble_state: None,
        bubble_panel: None,
        last_sprite_w: 150.0,
        last_panel_sz: (init_sz.width, init_sz.height),
        last_screen_pos: (start_cx, start_cy),
        last_rendered: None,
    }
}

// ---- Menu action delegate ----

define_class!(
    // SAFETY:
    // - The superclass NSObject does not have any subclassing requirements.
    // - `MenuDelegate` does not implement `Drop`.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    struct MenuDelegate;

    // SAFETY: `NSObjectProtocol` has no additional safety requirements.
    unsafe impl NSObjectProtocol for MenuDelegate {}

    unsafe impl NSMenuDelegate for MenuDelegate {}

    impl MenuDelegate {
        /// Gray out "Remove" when only one character remains, and refresh
        /// the location / weather info items with the latest data.
        #[unsafe(method(menuWillOpen:))]
        fn menu_will_open(&self, menu: &NSMenu) {
            APP.with(|cell| {
                let b = cell.borrow();
                let Some(app) = b.as_ref() else { return };
                let enabled = app.chars.len() > 1;
                let ja = app.lang == "ja";
                unsafe {
                    // Tag 1 is set on the Remove item in make_status_item.
                    let item: Option<&NSMenuItem> = msg_send![menu, itemWithTag: 1_isize];
                    if let Some(item) = item {
                        let (): () = msg_send![item, setEnabled: enabled];
                    }

                    // Tags 10 (location) and 11 (weather) are the info items.
                    let loc_item: Option<&NSMenuItem> = msg_send![menu, itemWithTag: 10_isize];
                    if let Some(item) = loc_item {
                        let geo = app.weather.geo_status();
                        let title = status_location_label(&app.weather_cfg, &geo, ja);
                        let (): () = msg_send![item, setTitle: &*NSString::from_str(&title)];
                    }

                    let wx_item: Option<&NSMenuItem> = msg_send![menu, itemWithTag: 11_isize];
                    if let Some(item) = wx_item {
                        let title = if let Some(info) = app.weather.get() {
                            let (emoji, cat) = match info.category {
                                crate::weather::WeatherCategory::Sunny  =>
                                    ("\u{2600}\u{fe0f}",  if ja { "晴れ" } else { "Sunny" }),
                                crate::weather::WeatherCategory::Cloudy =>
                                    ("\u{26c5}",           if ja { "曇り" } else { "Cloudy" }),
                                crate::weather::WeatherCategory::Rainy  =>
                                    ("\u{1f327}\u{fe0f}", if ja { "雨" }  else { "Rainy" }),
                                crate::weather::WeatherCategory::Snowy  =>
                                    ("\u{1f328}\u{fe0f}", if ja { "雪" }  else { "Snowy" }),
                            };
                            format!("{} {}, {:.1}\u{00b0}C", emoji, cat, info.temp_c)
                        } else {
                            "\u{2500}".to_string()  // ─
                        };
                        let (): () = msg_send![item, setTitle: &*NSString::from_str(&title)];
                    }

                    // Tag 2: Settings… vs Open Settings File (Option held while menu is open).
                    let flags: NSEventModifierFlags =
                        unsafe { msg_send![NSEvent::class(), modifierFlags] };
                    let option = flags.contains(NSEventModifierFlags::Option);
                    let settings_item: Option<&NSMenuItem> = msg_send![menu, itemWithTag: 2_isize];
                    if let Some(item) = settings_item {
                        let title = if option {
                            if ja { "設定ファイルを開く" } else { "Open Settings File" }
                        } else if ja {
                            "設定…"
                        } else {
                            "Settings…"
                        };
                        let (): () = msg_send![item, setTitle: &*NSString::from_str(title)];
                    }
                }
            });
        }

        /// Spawn one additional bearded dragon.
        #[unsafe(method(addBeardedDragon:))]
        fn add_bearded_dragon(&self, _sender: &AnyObject) {
            let mt = self.mtm();
            APP.with(|cell| {
                let mut b = cell.borrow_mut();
                let Some(app) = b.as_mut() else { return };
                let si = macos_wm::screen_info(mt).unwrap_or(ScreenInfo {
                    width: 1280.0, height: 800.0, dock_height: 0.0, menu_bar_height: 24.0,
                });
                let assets = Rc::clone(&app.bd_assets);
                let config = app.bd_config.clone();
                app.chars.push(spawn_char(assets, config, &si, mt, false, "bearded_dragon"));
            });
        }

        /// Spawn one additional pond turtle.
        #[unsafe(method(addPondTurtle:))]
        fn add_pond_turtle(&self, _sender: &AnyObject) {
            let mt = self.mtm();
            APP.with(|cell| {
                let mut b = cell.borrow_mut();
                let Some(app) = b.as_mut() else { return };
                let si = macos_wm::screen_info(mt).unwrap_or(ScreenInfo {
                    width: 1280.0, height: 800.0, dock_height: 0.0, menu_bar_height: 24.0,
                });
                let assets = Rc::clone(&app.pt_assets);
                let config = app.pt_config.clone();
                app.chars.push(spawn_char(assets, config, &si, mt, false, "pond_turtle"));
            });
        }

        /// Spawn one additional leopard gecko.
        #[unsafe(method(addLeopardGecko:))]
        fn add_leopard_gecko(&self, _sender: &AnyObject) {
            let mt = self.mtm();
            APP.with(|cell| {
                let mut b = cell.borrow_mut();
                let Some(app) = b.as_mut() else { return };
                let si = macos_wm::screen_info(mt).unwrap_or(ScreenInfo {
                    width: 1280.0, height: 800.0, dock_height: 0.0, menu_bar_height: 24.0,
                });
                let assets = Rc::clone(&app.lg_assets);
                let config = app.lg_config.clone();
                app.chars.push(spawn_char(assets, config, &si, mt, false, "leopard_gecko"));
            });
        }

        /// Open the settings window or user.toml in the editor when Option is held.
        #[unsafe(method(openSettings:))]
        fn open_settings(&self, _sender: &AnyObject) {
            let flags: NSEventModifierFlags =
                unsafe { msg_send![NSEvent::class(), modifierFlags] };
            if flags.contains(NSEventModifierFlags::Option) {
                crate::user_config::open_in_editor();
            } else {
                crate::user_config::launch_settings_ui();
            }
        }

        /// Remove the most recently added character (minimum 1 remains).
        #[unsafe(method(removeCharacter:))]
        fn remove_character(&self, _sender: &AnyObject) {
            APP.with(|cell| {
                let mut b = cell.borrow_mut();
                let Some(app) = b.as_mut() else { return };
                if app.chars.len() > 1 {
                    unsafe { app.chars.last().unwrap().panel.orderOut(None) };
                    app.chars.pop();
                }
            });
        }

        /// Called by "Remove This Character…" debug menu item.
        /// Shows an NSAlert for confirmation before removing the specific character.
        #[unsafe(method(debugRemoveSelect:))]
        fn debug_remove_select(&self, _sender: &NSMenuItem) {
            let char_idx = APP.with(|cell| {
                cell.borrow().as_ref().map(|a| a.debug_menu_char)
            });
            let Some(idx) = char_idx else { return };

            let mt = unsafe { MainThreadMarker::new_unchecked() };
            let confirmed = unsafe {
                let alert = NSAlert::init(NSAlert::alloc(mt));
                let ja = APP.with(|cell| cell.borrow().as_ref().map(|a| a.lang == "ja").unwrap_or(false));
                let (): () = msg_send![&*alert, setMessageText:
                    &*NSString::from_str(if ja { "このキャラクターを削除しますか？" } else { "Remove this character?" })];
                let (): () = msg_send![&*alert, setInformativeText:
                    &*NSString::from_str(if ja { "デスクトップから削除されます。" } else { "The character will be removed from the desktop." })];
                let (): () = msg_send![&*alert,
                    addButtonWithTitle: &*NSString::from_str(if ja { "削除" } else { "Remove" })];
                let (): () = msg_send![&*alert,
                    addButtonWithTitle: &*NSString::from_str(if ja { "キャンセル" } else { "Cancel" })];
                let response: isize = msg_send![&*alert, runModal];
                response == 1000 // NSAlertFirstButtonReturn
            };

            if confirmed {
                APP.with(|cell| {
                    let mut b = cell.borrow_mut();
                    let Some(app) = b.as_mut() else { return };
                    if app.chars.len() > 1 && idx < app.chars.len() {
                        unsafe { app.chars[idx].panel.orderOut(None) };
                        app.chars.remove(idx);
                    }
                });
            }
        }

        /// Called by debug context-menu items; `sender.tag()` indexes into
        /// `AppState::debug_menu_targets` to find the target state.
        #[unsafe(method(debugTriggerSelect:))]
        fn debug_trigger_select(&self, sender: &NSMenuItem) {
            APP.with(|cell| {
                let mut b = cell.borrow_mut();
                let Some(app) = b.as_mut() else { return };
                let tag: isize = unsafe { msg_send![sender, tag] };
                let idx = tag as usize;
                let char_idx = app.debug_menu_char;
                if let Some(target) = app.debug_menu_targets.get(idx) {
                    if let Some(ch) = app.chars.get_mut(char_idx) {
                        ch.debug_trigger =
                            Some((target.clone(), crate::debug_menu::COUNTDOWN_SECS));
                    }
                }
            });
        }
    }
);

impl MenuDelegate {
    fn new(mt: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mt);
        // SAFETY: NSObject's `init` signature is correct.
        unsafe { msg_send![this, init] }
    }
}

// ---- Hover alpha ----

fn update_hover_alpha(panel: &NSPanel, config: &crate::config::Config, dragging: bool) {
    // While dragging, keep the panel at full opacity.
    if dragging {
        unsafe { panel.setAlphaValue(1.0) };
        return;
    }
    let mouse = unsafe { NSEvent::mouseLocation() };
    let frame = unsafe { panel.frame() };
    let over = unsafe { panel.isVisible() }
        && mouse.x >= frame.origin.x
        && mouse.x < frame.origin.x + frame.size.width
        && mouse.y >= frame.origin.y
        && mouse.y < frame.origin.y + frame.size.height;
    let target = if over { config.display.hover_alpha } else { 1.0 };
    let cur = unsafe { panel.alphaValue() };
    if (cur - target).abs() > 0.01 {
        unsafe { panel.setAlphaValue(target) };
    }
}

/// Apply or clear a snap-zone highlight on the character's image view.
///
/// Uses `CIColorControls` (brightness boost) via `NSView.contentFilters`
/// so the effect respects the sprite's alpha channel and follows the
/// sprite shape rather than the rectangular bounding box.
fn set_snap_highlight(image_view: &NSImageView, active: bool) {
    unsafe {
        if active {
            // Layer-backing is required for contentFilters to take effect.
            let _: () = msg_send![image_view, setWantsLayer: true];

            let Some(ci_class) = objc2::runtime::AnyClass::get(c"CIFilter") else { return };
            let filter_name = NSString::from_str("CIColorControls");
            let filter: *mut AnyObject = msg_send![ci_class, filterWithName: &*filter_name];
            if filter.is_null() { return; }
            let _: () = msg_send![filter, setDefaults];
            let key = NSString::from_str("inputBrightness");
            let val = objc2_foundation::NSNumber::new_f64(0.4);
            let _: () = msg_send![filter, setValue: &*val forKey: &*key];

            let Some(arr_class) = objc2::runtime::AnyClass::get(c"NSArray") else { return };
            let arr: *mut AnyObject = msg_send![arr_class, arrayWithObject: filter];
            let _: () = msg_send![image_view, setContentFilters: arr];
        } else {
            let Some(arr_class) = objc2::runtime::AnyClass::get(c"NSArray") else { return };
            let empty: *mut AnyObject = msg_send![arr_class, array];
            let _: () = msg_send![image_view, setContentFilters: empty];
        }
    }
}

// ---- ⌘+drag event monitors ----

/// Register global event monitors for ⌘+drag.
/// Returns the monitor handles that must be kept alive.
fn setup_drag_monitors() -> Vec<Retained<AnyObject>> {
    let mut monitors = Vec::new();

    // LeftMouseDown — decide whether to start a drag.
    let mask_down = NSEventMask::LeftMouseDown;
    let blk_down = block2::RcBlock::new(move |ev: std::ptr::NonNull<NSEvent>| {
        let ev = unsafe { ev.as_ref() };
        let flags = unsafe { ev.modifierFlags() };
        if !flags.contains(NSEventModifierFlags::Command) {
            return;
        }
        let mouse_ns = unsafe { NSEvent::mouseLocation() };
        APP.with(|cell| {
            let mut b = cell.borrow_mut();
            let Some(app) = b.as_mut() else { return };
            // Find the first panel that contains the click.
            for ch in &mut app.chars {
                let frame = unsafe { ch.panel.frame() };
                if mouse_ns.x >= frame.origin.x
                    && mouse_ns.x < frame.origin.x + frame.size.width
                    && mouse_ns.y >= frame.origin.y
                    && mouse_ns.y < frame.origin.y + frame.size.height
                {
                    let offset = (mouse_ns.x - frame.origin.x, mouse_ns.y - frame.origin.y);
                    ch.drag_offset = Some(offset);
                    ch.anim_state = State::Grabbed;
                    ch.surface = Surface::Airborne;
                    break; // only grab the topmost hit
                }
            }
        });
    });
    if let Some(m) = unsafe {
        NSEvent::addGlobalMonitorForEventsMatchingMask_handler(mask_down, &*blk_down)
    } {
        monitors.push(m);
    }

    // LeftMouseDragged — follow the mouse.
    let mask_drag = NSEventMask::LeftMouseDragged;
    let blk_drag = block2::RcBlock::new(move |_ev: std::ptr::NonNull<NSEvent>| {
        APP.with(|cell| {
            let mut b = cell.borrow_mut();
            let Some(app) = b.as_mut() else { return };
            let Some(drag_idx) = app.chars.iter().position(|c| c.drag_offset.is_some())
                else { return };
            let off = app.chars[drag_idx].drag_offset.unwrap();
            let mouse_ns = unsafe { NSEvent::mouseLocation() };
            let new_ns_x = mouse_ns.x - off.0;
            let new_ns_y = mouse_ns.y - off.1;
            unsafe { app.chars[drag_idx].panel.setFrameOrigin(NSPoint::new(new_ns_x, new_ns_y)) };
            let sz = unsafe { app.chars[drag_idx].panel.frame().size };
            let si = macos_wm::screen_info_raw_full();
            app.chars[drag_idx].char_pos = (new_ns_x, si.height - new_ns_y - sz.height);

            // Snap-zone feedback: highlight the sprite when a surface is nearby.
            let foot_x = new_ns_x + sz.width / 2.0;
            let foot_y = si.height - new_ns_y;
            let wins: &[WinInfo] = &app.win_cache.wins;
            let in_snap = macos_wm::find_drop_surface(foot_x, foot_y, wins, &si).is_some();
            set_snap_highlight(&app.chars[drag_idx].image_view, in_snap);
        });
    });
    if let Some(m) = unsafe {
        NSEvent::addGlobalMonitorForEventsMatchingMask_handler(mask_drag, &*blk_drag)
    } {
        monitors.push(m);
    }

    // LeftMouseUp — release: find surface or start falling.
    let mask_up = NSEventMask::LeftMouseUp;
    let blk_up = block2::RcBlock::new(move |_ev: std::ptr::NonNull<NSEvent>| {
        APP.with(|cell| {
            let mut b = cell.borrow_mut();
            let Some(app) = b.as_mut() else { return };
            let Some(drag_idx) = app.chars.iter().position(|c| c.drag_offset.is_some())
                else { return };
            app.chars[drag_idx].drag_offset = None;
            set_snap_highlight(&app.chars[drag_idx].image_view, false);

            let si = macos_wm::screen_info_raw_full();
            let wins = macos_wm::list_windows(&si);
            let cfg = app.chars[drag_idx].config.lock().unwrap().current.clone();

            let ch = &mut app.chars[drag_idx];

            // Sprite dimensions for foot position.
            let sr = sprite_for_state(&ch.anim_state, ch.facing, &ch.assets.animations);
            let (fw, fh) = ch.assets.image(&sr.name, sr.mirror)
                .map(|img| { let sz = unsafe { img.size() }; (sz.width, sz.height) })
                .unwrap_or((150.0, 150.0));
            let foot_x = ch.char_pos.0 + fw / 2.0;
            let foot_y = ch.char_pos.1 + fh;

            // Try to snap to a nearby surface (wider wall threshold for drag drops).
            let new_surface = macos_wm::find_drop_surface(foot_x, foot_y, &wins, &si);
            match new_surface {
                Some(surf) => {
                    let ctx = BehaviorContext {
                        state: &ch.anim_state,
                        surface: &surf,
                        elapsed_secs: 0.0,
                        config: &cfg,
                        rng01: 0.0,
                        surface_progress: 0.5,
                        facing: ch.facing,
                        at_edge: false,
                        surface_edge_info: SurfaceEdge::None,
                        jump_target: None,
                        attract_target: None,
                    };
                    let new_anim = ch.behavior.on_landed(&ctx);
                    let stand_anchor = ch.assets.anchor("s-stand")
                        .unwrap_or(Anchor { x: 0.0, y: 0.0 });
                    let stand_h = ch.assets.image("s-stand", false)
                        .map(|img| unsafe { img.size() }.height)
                        .unwrap_or(fh);
                    let snap_y = match &surf {
                        Surface::WindowTop { win_id, .. } =>
                            macos_wm::find_win(*win_id, &wins).map(|w| w.y),
                        Surface::Desktop { .. } => {
                            let si2 = macos_wm::screen_info_raw_full();
                            Some(si2.floor_y())
                        }
                        _ => None,
                    };
                    if let Some(surface_y) = snap_y {
                        ch.char_pos = (foot_x - fw / 2.0, surface_y - stand_h + stand_anchor.y);
                    }
                    ch.surface = surf;
                    // Wrap landing animation in a OneShot reaction if one is defined.
                    if let Some(react_anim) = ch.behavior_engine.on_interaction(
                        crate::anim_trigger::ReactionTrigger::Dropped,
                    ) {
                        ch.anim_state = crate::behavior::State::OneShot {
                            animation: react_anim,
                            frame: 0,
                            frame_elapsed: 0.0,
                            done: false,
                            return_to: Box::new(new_anim),
                        };
                    } else {
                        ch.anim_state = new_anim;
                    }
                }
                None => {
                    ch.surface = Surface::Airborne;
                    ch.anim_state = State::Falling { vx: 0.0, vy: 0.0, shocked: 0.0 };
                }
            }
        });
    });
    if let Some(m) = unsafe {
        NSEvent::addGlobalMonitorForEventsMatchingMask_handler(mask_up, &*blk_up)
    } {
        monitors.push(m);
    }

    // RightMouseDown (local monitor) — captures right-clicks on our panels when ⌥⌘ is held.
    //
    // Using a LOCAL monitor (not global) is essential: local monitors can return nil to
    // consume the event, preventing it from reaching the underlying app (Finder/Desktop).
    // The tick loop polls ⌥⌘ state and sets ignoresMouseEvents=false on panels under the
    // cursor so that right-clicks are delivered to our app rather than passing through.
    let mask_rdown = NSEventMask::RightMouseDown;
    let blk_rdown = block2::RcBlock::new(move |_ev: std::ptr::NonNull<NSEvent>| -> *mut NSEvent {
        let flags = unsafe { _ev.as_ref().modifierFlags() };
        if !flags.contains(NSEventModifierFlags::Option)
            || !flags.contains(NSEventModifierFlags::Command)
        {
            return _ev.as_ptr(); // not our gesture — pass through unchanged
        }
        let mouse_ns = unsafe { NSEvent::mouseLocation() };

        // Gather menu info and store targets — all within a single borrow.
        struct MenuInfo {
            header: String,
            outing_str: String,
            target_labels: Vec<String>,
            can_remove: bool,
        }
        let result = APP.with(|cell| -> Option<(usize, MenuInfo)> {
            let mut b = cell.borrow_mut();
            let app = b.as_mut()?;

            // Hit-test all character panels.
            let idx = app.chars.iter().position(|ch| {
                let frame = unsafe { ch.panel.frame() };
                mouse_ns.x >= frame.origin.x
                    && mouse_ns.x < frame.origin.x + frame.size.width
                    && mouse_ns.y >= frame.origin.y
                    && mouse_ns.y < frame.origin.y + frame.size.height
            })?;

            let ch = &app.chars[idx];
            let cfg = ch.config.lock().unwrap().current.clone();

            let surface_str = crate::debug_menu::surface_name(&ch.surface);
            let state_str   = crate::debug_menu::state_name(&ch.anim_state);
            let dur_str = crate::debug_menu::state_elapsed_duration(&ch.anim_state)
                .map(|(e, d)| format!(" ({:.0}s / {:.0}s)", d - e, d))
                .unwrap_or_default();
            let header = format!("{} — {}{}", surface_str, state_str, dur_str);
            let outing_str = ch.behavior.outing_info(&cfg)
                .map(|(r, t)| if app.lang == "ja" {
                    format!("次の外出: {:.0}秒 / {:.0}秒", r, t)
                } else {
                    format!("Next outing: {:.0}s / {:.0}s", r, t)
                })
                .unwrap_or_default();

            let targets = crate::debug_menu::trigger_targets(
                &ch.surface, &ch.anim_state, ch.facing, &cfg,
            );
            if targets.is_empty() {
                return None;
            }

            let labels: Vec<String> = targets.iter().map(|t| t.label.clone()).collect();
            // Store target states for dispatching via debugTriggerSelect:.
            app.debug_menu_char    = idx;
            app.debug_menu_targets = targets.into_iter().map(|t| t.state).collect();

            Some((idx, MenuInfo { header, outing_str, target_labels: labels, can_remove: app.chars.len() > 1 }))
        });

        let Some((_idx, info)) = result else {
            return _ev.as_ptr(); // no matching panel — pass through
        };

        // Get a raw pointer to MenuDelegate (safe: AppState keeps it alive).
        let handler_ptr: *const MenuDelegate = APP.with(|cell| {
            cell.borrow()
                .as_ref()
                .map(|app| &*app._menu_handler as *const MenuDelegate)
                .unwrap_or(std::ptr::null())
        });
        if handler_ptr.is_null() { return _ev.as_ptr(); }

        let mt = unsafe { MainThreadMarker::new_unchecked() };
        unsafe {
            let menu = NSMenu::init(NSMenu::alloc(mt));
            let (): () = msg_send![&*menu, setAutoenablesItems: false];

            // Info header (disabled — display only).
            let info_item = NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mt),
                &NSString::from_str(&info.header),
                None,
                &NSString::from_str(""),
            );
            let (): () = msg_send![&*info_item, setEnabled: false];
            menu.addItem(&info_item);

            if !info.outing_str.is_empty() {
                let outing_item = NSMenuItem::initWithTitle_action_keyEquivalent(
                    NSMenuItem::alloc(mt),
                    &NSString::from_str(&info.outing_str),
                    None,
                    &NSString::from_str(""),
                );
                let (): () = msg_send![&*outing_item, setEnabled: false];
                menu.addItem(&outing_item);
            }

            menu.addItem(&NSMenuItem::separatorItem(mt));

            let handler = &*handler_ptr;
            for (i, label) in info.target_labels.iter().enumerate() {
                let item = NSMenuItem::initWithTitle_action_keyEquivalent(
                    NSMenuItem::alloc(mt),
                    &NSString::from_str(label),
                    Some(objc2::sel!(debugTriggerSelect:)),
                    &NSString::from_str(""),
                );
                let (): () = msg_send![&*item, setTag: i as isize];
                let (): () = msg_send![&*item, setTarget: handler];
                menu.addItem(&item);
            }

            // Separator + destructive Remove item (only when more than one character).
            if info.can_remove {
                menu.addItem(&NSMenuItem::separatorItem(mt));
                let rm = NSMenuItem::initWithTitle_action_keyEquivalent(
                    NSMenuItem::alloc(mt),
                    &NSString::from_str({
                        let ja = APP.with(|c| c.borrow().as_ref().map(|a| a.lang == "ja").unwrap_or(false));
                        if ja { "このキャラクターを削除…" } else { "Remove This Character\u{2026}" }
                    }),
                    Some(objc2::sel!(debugRemoveSelect:)),
                    &NSString::from_str(""),
                );
                let (): () = msg_send![&*rm, setTarget: handler];
                menu.addItem(&rm);
            }

            // Show at NS screen coordinates (nil inView → screen coords).
            let (): () = msg_send![
                &*menu,
                popUpMenuPositioningItem: std::ptr::null::<NSMenuItem>(),
                atLocation: mouse_ns,
                inView: std::ptr::null::<objc2_app_kit::NSView>()
            ];
        }

        // Return nil to consume the event — Finder never sees this right-click.
        std::ptr::null_mut()
    });
    if let Some(m) = unsafe {
        NSEvent::addLocalMonitorForEventsMatchingMask_handler(mask_rdown, &*blk_rdown)
    } {
        monitors.push(m);
    }

    monitors
}



/// Convert the character's current `surface` and local position into the
/// NS frameOrigin (bottom-left corner of the panel in NS coords).
///
/// `char_pos` is the CG position of the character (used only for `Airborne`).
/// `sprite_sz` is the panel / image size in NS points.
/// `anchor` is the attachment offset in scaled display pixels.
fn surface_to_ns_origin(
    surface: &Surface,
    char_pos: (f64, f64),
    sprite_sz: (f64, f64),
    anchor: Anchor,
    stand_anchor_y: f64,
    wins: &[WinInfo],
    si: &ScreenInfo,
) -> NSPoint {
    let (sw, sh) = sprite_sz;
    let cg_to_ns_y = |cg_y: f64, height: f64| si.height - cg_y - height;

    match surface {
        // Free-flight: char_pos is the CG top-left corner of the sprite.
        Surface::Airborne => {
            let (cx, cy) = char_pos;
            NSPoint::new(cx, cg_to_ns_y(cy, sh))
        }

        // Floor: foot at floor_y (CG), centred horizontally on x.
        // Shift up by stand_anchor_y so the bottom of the sprite sits at the
        // visible floor instead of being hidden behind the Dock.
        Surface::Desktop { x } => {
            let ns_foot = si.height - si.floor_y();
            NSPoint::new(x - sw / 2.0, ns_foot - anchor.y + stand_anchor_y)
        }

        // Window top: foot on the window's top edge, x_local from left edge.
        Surface::WindowTop { win_id, x_local } => {
            let Some(win) = macos_wm::find_win(*win_id, wins) else {
                return NSPoint::ZERO;
            };
            let ns_top = si.height - win.y;
            NSPoint::new(win.x + x_local - sw / 2.0, ns_top - anchor.y)
        }

        // Wall: grip line at y_local below window top.
        // anchor.x = distance from **right** edge to grip line (for right wall).
        // Mirrored sprite (left wall): grip is the same distance from the left edge.
        Surface::WindowWall { win_id, side, y_local } => {
            let Some(win) = macos_wm::find_win(*win_id, wins) else {
                return NSPoint::ZERO;
            };
            // Vertical: centre sprite on grip row.
            let ns_grip = si.height - (win.y + y_local);
            let ns_y = ns_grip - sh / 2.0;
            let ns_x = match side {
                Side::Right => win.right() - sw + anchor.x,
                Side::Left  => win.x - anchor.x,
            };
            NSPoint::new(ns_x, ns_y)
        }

        // Upper corner: foot on window top, grip against the wall.
        // Two cases based on anchor type:
        //   point attachment (s-hang-corner, f-hang-corner): anchor.x > 0
        //     Side::Right: sprite_left + anchor.x = win.right() → ns_x = win.right() - anchor.x
        //     Side::Left (mirrored): ns_x = win.x - sw + anchor.x
        //   line_y attachment (f-sit, f-lie, f-stand): anchor.x == 0
        //     Sprite is a front-facing symmetric sprite resting on the corner.
        //     Align sprite edge to window corner edge.
        //     Side::Right: ns_x = win.right() - sw
        //     Side::Left:  ns_x = win.x
        Surface::WindowUpperCorner { win_id, side } => {
            let Some(win) = macos_wm::find_win(*win_id, wins) else {
                return NSPoint::ZERO;
            };
            let ns_top = si.height - win.y;
            let ns_y = ns_top - anchor.y;
            let ns_x = if anchor.x > 0.0 {
                // point attachment (hang-corner sprites)
                match side {
                    Side::Right => win.right() - anchor.x,
                    Side::Left  => win.x - sw + anchor.x,
                }
            } else {
                // line_y attachment (f-sit, f-lie, …): align to corner edge
                match side {
                    Side::Right => win.right() - sw,
                    Side::Left  => win.x,
                }
            };
            NSPoint::new(ns_x, ns_y)
        }

        // Window bottom: foot on the window's bottom edge, x_local from left edge.
        Surface::WindowBottom { win_id, x_local } => {
            let Some(win) = macos_wm::find_win(*win_id, wins) else {
                return NSPoint::ZERO;
            };
            let ns_foot = si.height - win.bottom();
            NSPoint::new(win.x + x_local - sw / 2.0, ns_foot - anchor.y)
        }

    }
}

fn tick_char(
    ch: &mut CharState,
    cfg: &Config,
    si: &ScreenInfo,
    wins: &[WinInfo],
    _mt: MainThreadMarker,
    sprite_size: f64,
) {
    // While dragging, skip physics and state machine — panel position is
    // updated directly by the drag event monitor.
    if ch.drag_offset.is_some() {
        update_hover_alpha(&ch.panel, cfg, true);
        return;
    }

    // Compute dt, capped to avoid large jumps after pauses.
    let now = Instant::now();
    let dt = now.duration_since(ch.last_tick).as_secs_f64().min(0.1);
    ch.last_tick = now;

    // Surface validity check.
    if !macos_wm::surface_still_valid(&ch.surface, wins) {
        let fallback = {
            let ctx = BehaviorContext {
                state: &ch.anim_state,
                surface: &ch.surface,
                elapsed_secs: 0.0,
                config: cfg,
                rng01: 0.0,
                surface_progress: 0.5,
                facing: ch.facing,
                at_edge: false,
                surface_edge_info: SurfaceEdge::None,
                jump_target: None,
                attract_target: None,
            };
            ch.behavior.on_surface_lost(&ctx)
        };

        // Reposition char_pos so the falling sprite appears centered on the
        // same screen point as the sprite that was just visible.
        // last_screen_pos is the CG top-left of the last rendered sprite;
        // compute its center, then derive the new top-left for the falling
        // sprite so it shares that center.
        {
            let sr_cur = sprite_for_state(&ch.anim_state, ch.facing, &ch.assets.animations);
            let sr_new = sprite_for_state(&fallback, ch.facing, &ch.assets.animations);
            if let (Some(img_cur), Some(img_new)) = (
                ch.assets.image(&sr_cur.name, sr_cur.mirror),
                ch.assets.image(&sr_new.name, sr_new.mirror),
            ) {
                let cur_sz = unsafe { img_cur.size() };
                let new_sz = unsafe { img_new.size() };
                let center_cx = ch.last_screen_pos.0 + cur_sz.width  / 2.0;
                let center_cy = ch.last_screen_pos.1 + cur_sz.height / 2.0;
                ch.char_pos = (
                    center_cx - new_sz.width  / 2.0,
                    center_cy - new_sz.height / 2.0,
                );
            }
        }

        ch.anim_state = fallback;
        ch.surface = Surface::Airborne;
    }

    // Advance per-state animation timers / frame counters.
    let elapsed = advance_anim(&mut ch.anim_state, dt, cfg, &ch.assets.animations);

    // Save CG y before position update for swept landing detection.
    let prev_cy = ch.char_pos.1;

    let psi = si.physics_screen();

    // Sprite sizes needed by physics functions.
    let stand_size = ch.assets.size("s-stand", false);
    let jump_size  = ch.assets.size("s-jump", false);
    let hang_h     = ch.assets.size("s-hang-wall-0", false).1;

    physics::integrate_velocity(&ch.anim_state, &mut ch.surface, &mut ch.char_pos, cfg, &psi, sprite_size, dt, wins);
    physics::apply_gravity(&mut ch.anim_state, cfg.jump.gravity, dt);
    physics::check_airborne_arrival(&mut ch.anim_state, &mut ch.surface, &mut ch.char_pos, &mut ch.facing, wins, hang_h, stand_size.0, jump_size.1);
    physics::check_off_screen(&mut ch.anim_state, &mut ch.surface, &mut ch.char_pos, &psi, stand_size, si.menu_bar_height);

    // Landing detection (swept check).
    if let Some(li) = physics::check_landing(&ch.anim_state, prev_cy, ch.char_pos, &psi, wins, sprite_size, jump_size) {
        let new_anim = {
            let ctx = BehaviorContext {
                state: &ch.anim_state,
                surface: &li.new_surface,
                elapsed_secs: 0.0,
                config: cfg,
                rng01: 0.0,
                surface_progress: 0.5,
                facing: ch.facing,
                at_edge: false,
                surface_edge_info: SurfaceEdge::None,
                jump_target: None,
                attract_target: None,
            };
            ch.behavior.on_landed(&ctx)
        };
        let stand_anchor = ch.assets.anchor("s-stand").unwrap_or(Anchor { x: 0.0, y: 0.0 });
        ch.char_pos    = (li.foot_x - jump_size.0 / 2.0, li.surface_y - stand_size.1 + stand_anchor.y);
        ch.surface     = li.new_surface;
        ch.anim_state  = new_anim;
    }

    // Compute surface_progress, at_edge, jump_target.
    let sr_for_ctx = match &ch.anim_state {
        State::TurningAround { elapsed, .. } => {
            let progress = (*elapsed / cfg.floor.turn_duration).clamp(0.0, 1.0);
            sprite_for_turn(progress, ch.facing)
        }
        other => sprite_for_state(other, ch.facing, &ch.assets.animations),
    };
    let sprite_sz = ch.assets.size(&sr_for_ctx.name, sr_for_ctx.mirror);
    ch.last_sprite_w = sprite_sz.0;
    let (surface_progress, at_edge, jump_target, attract_target) = physics::surface_context(
        &ch.surface, sprite_sz.0, ch.facing,
        cfg.jump.wall_jump_max_dist, cfg.jump.wall_jump_floor_margin,
        cfg.jump.climb_attract_dist, cfg.corner.corner_jump_dist, wins, &psi,
    );

    // Save to_dir if a TurningAround completes this tick.
    let turn_to_dir = if let State::TurningAround { to_dir, .. } = &ch.anim_state {
        Some(*to_dir)
    } else {
        None
    };

    // Run behavior state machine.
    let transition = {
        let ctx = BehaviorContext {
            state: &ch.anim_state,
            surface: &ch.surface,
            elapsed_secs: elapsed,
            config: cfg,
            rng01: 0.0,
            surface_progress,
            facing: ch.facing,
            at_edge,
            surface_edge_info: SurfaceEdge::compute(&ch.surface, at_edge, surface_progress),
            jump_target,
            attract_target,
        };
        ch.behavior.next_state(&ctx)
    };
    match transition {
        Transition::Stay => {}
        Transition::To(new_state) => {
            let mut new_state = new_state;
            if let Some(dir) = turn_to_dir { ch.facing = dir; }
            let sz = physics::TerrestrialSizes {
                hang_h,
                stand_w:  stand_size.0,
                walk_w:   ch.assets.size("s-walk-0", false).0,
                jump_w:   jump_size.0,
                jump_h:   jump_size.1,
                sprite_w: sprite_sz.0,
            };
            physics::resolve_transition(
                &mut new_state, &ch.anim_state, &mut ch.surface,
                &mut ch.char_pos, &mut ch.facing, cfg, wins, &sz,
                ch.assets.surfaces.habitat, ch.last_screen_pos,
            );
            ch.anim_state = new_state;
        }
    }

    // Debug trigger: forced state override after countdown.
    let fired = ch.debug_trigger.as_mut()
        .map(|(_, r)| { *r -= dt; *r <= 0.0 })
        .unwrap_or(false);
    if fired {
        if let Some((target, _)) = ch.debug_trigger.take() {
            ch.anim_state = target;
        }
    }

    // Keep facing in sync with Walking/Running direction.
    if let State::Walking { dir, .. } | State::Running { dir, .. } = &ch.anim_state {
        ch.facing = *dir;
    }

    // Select sprite and update panel position.
    let sr = match &ch.anim_state {
        State::TurningAround { elapsed, .. } => {
            let progress = (*elapsed / cfg.floor.turn_duration).clamp(0.0, 1.0);
            sprite_for_turn(progress, ch.facing)
        }
        other => sprite_for_state(other, ch.facing, &ch.assets.animations),
    };
    // ---- Rendering: skip Cocoa calls that are identical to last tick ----

    let anchor = ch.assets.anchor(&sr.name).unwrap_or(Anchor { x: 0.0, y: 0.0 });
    let stand_anchor_y = ch.assets.anchor("s-stand").map(|a| a.y).unwrap_or(0.0);
    let sz = ch.assets.image(&sr.name, sr.mirror)
        .map(|img| { let sz = unsafe { img.size() }; (sz.width, sz.height) })
        .unwrap_or((150.0, 150.0));
    let origin   = surface_to_ns_origin(&ch.surface, ch.char_pos, sz, anchor, stand_anchor_y, wins, si);
    let y_offset = vertical_offset(&ch.anim_state, &ch.assets.animations);
    let z_host   = surface_host_win_id(&ch.surface).or_else(|| {
        match &ch.anim_state {
            State::JumpRunup { target_win_id, .. } |
            State::Airborne  { target_win_id, .. } => Some(*target_win_id),
            _ => None,
        }
    });

    let prev = ch.last_rendered.as_ref();

    // Sprite: only swap when name or mirror changed.
    if prev.map_or(true, |p| p.sprite_name != sr.name || p.mirror != sr.mirror) {
        swap_sprite(&ch.image_view, &ch.panel, &ch.assets, &sr.name, sr.mirror, &mut ch.last_panel_sz);
    }

    // Position: only move when origin or vertical-offset changed.
    if prev.map_or(true, |p| p.origin != (origin.x, origin.y) || p.y_offset != y_offset) {
        unsafe { ch.panel.setFrameOrigin(NSPoint::new(origin.x, origin.y + y_offset)) };
    }

    // Record the rendered position so surface-lost and jump-launch code can
    // read back where the character actually was last tick.
    ch.last_screen_pos = (origin.x, si.height - origin.y - sz.1);

    // Z-order: mirror the Windows GW_HWNDPREV approach.
    //
    // Read the first foreign window above the host from the cached wins list
    // (front-to-back order).  The cache is refreshed at HIGH_FREQ so z-order
    // changes are detected within ~500 ms — no extra IPC per tick.
    // When above_id is Some: insert panel just below it (between it and host).
    // When above_id is None: host is frontmost; panel goes directly above it.
    // On Desktop/Airborne (z_host == None): panel comes to front.
    // Only call into Cocoa when the relevant ids actually change.
    let z_above_host_id: Option<u32> = z_host.and_then(|host_id| {
        let host_pos = wins.iter().position(|w| w.id == host_id)?;
        if host_pos == 0 { None } else { wins.get(host_pos - 1).map(|w| w.id) }
    });

    let host_changed  = prev.map_or(true, |p| p.z_host_id       != z_host);
    let above_changed = prev.map_or(true, |p| p.z_above_host_id != z_above_host_id);
    if host_changed || above_changed {
        unsafe {
            match (z_host, z_above_host_id) {
                (None, _) => {
                    ch.panel.orderFront(None);
                }
                (Some(host_id), None) => {
                    // Host is the frontmost foreign window; place panel just above it.
                    let (): () = msg_send![&*ch.panel, orderWindow: 1_isize relativeTo: host_id as isize];
                }
                (Some(_), Some(above_id)) => {
                    // Place panel just below the foreign window that sits above the host.
                    // NSWindowBelow = -1.
                    let (): () = msg_send![&*ch.panel, orderWindow: -1_isize relativeTo: above_id as isize];
                }
            }
        }
    }

    ch.last_rendered = Some(RenderedState {
        sprite_name:     sr.name.to_owned(),
        mirror:          sr.mirror,
        origin:          (origin.x, origin.y),
        y_offset,
        z_host_id:       z_host,
        z_above_host_id,
    });

    // Hover alpha.
    update_hover_alpha(&ch.panel, cfg, false);
}

// ---- Debug countdown status item ----

/// Update the status-item icon to reflect an active debug countdown.
/// Shows numbered SF Symbols (3.circle.fill / 2 / 1) while counting down,
/// then restores the default lizard icon when done.
fn update_status_countdown(
    item: &objc2_app_kit::NSStatusItem,
    remaining: Option<f64>,
    mt: MainThreadMarker,
) {
    let sym = match remaining.map(|r| r.ceil() as u32) {
        Some(1)          => "1.circle.fill",
        Some(2)          => "2.circle.fill",
        Some(n) if n > 2 => "3.circle.fill",
        _                => "lizard.fill",
    };
    unsafe {
        let Some(btn) = item.button(mt) else { return };
        if let Some(img) = NSImage::imageWithSystemSymbolName_accessibilityDescription(
            &NSString::from_str(sym),
            None,
        ) {
            img.setTemplate(true);
            btn.setImage(Some(&img));
        }
    }
}

/// Relaunch with a new PID after the settings window closes. Must run on the main thread so the
/// status item is removed before exit (`exec` reuses the PID and breaks the menu bar).
fn perform_settings_restart(mt: MainThreadMarker) {
    if let Ok(exe) = std::env::current_exe() {
        let args: Vec<String> = std::env::args().skip(1).collect();
        if let Err(e) = Command::new(&exe).args(&args).spawn() {
            eprintln!("[petitmates] settings restart spawn failed: {e}");
        }
    } else {
        eprintln!("[petitmates] settings restart: could not resolve current_exe");
    }

    APP.with(|cell| {
        if let Some(app) = cell.borrow_mut().as_ref() {
            unsafe {
                NSStatusBar::systemStatusBar().removeStatusItem(&app._status_item);
            }
        }
    });

    unsafe {
        NSApplication::sharedApplication(mt).terminate(None);
    }
}

fn tick() {
    APP.with(|cell| {
        let mut b = cell.borrow_mut();
        let Some(app) = b.as_mut() else { return };
        let mt = unsafe { MainThreadMarker::new_unchecked() };

        if crate::user_config::take_restart_request() {
            perform_settings_restart(mt);
            return;
        }

        // Rate-limit: skip this 60 Hz callback when the target interval has not elapsed.
        {
            let target = 1.0 / app.tick_rate.max(1) as f64;
            if app.last_full_tick.elapsed().as_secs_f64() < target {
                return;
            }
            app.last_full_tick = Instant::now();
        }

        // Compute screen info once for all characters.
        let si = macos_wm::screen_info(mt).unwrap_or(ScreenInfo {
            width: 1280.0, height: 800.0, dock_height: 0.0, menu_bar_height: 24.0,
        });

        // Tiered window-list refresh.
        // Phase 1 — all mutations to win_cache happen here before we borrow wins.
        let full_refresh = app.win_cache.ticks_until_refresh == 0;
        if full_refresh {
            macos_wm::list_windows_into(&mut app.win_cache.wins, &si);
            let attract_dist = app.chars.first()
                .map(|ch| ch.config.lock().unwrap().current.jump.climb_attract_dist)
                .unwrap_or(600.0);
            app.win_cache.ticks_until_refresh =
                next_refresh_interval(&app.chars, &app.win_cache.wins, attract_dist);
        } else {
            app.win_cache.ticks_until_refresh -= 1;
            // Per-tick host-window update: refresh the cached entry for each
            // character's host window so rendering tracks window movement within
            // the high-frequency interval.
            for i in 0..app.chars.len() {
                if let Some(host_id) = surface_host_win_id(&app.chars[i].surface) {
                    match macos_wm::host_win_info(host_id) {
                        Some(fresh) => {
                            if let Some(entry) = app.win_cache.wins.iter_mut().find(|w| w.id == host_id) {
                                *entry = fresh;
                            }
                        }
                        None => {
                            // Window gone — evict so surface_still_valid fires this tick.
                            app.win_cache.wins.retain(|w| w.id != host_id);
                        }
                    }
                }
            }
        }

        // Phase 2 — tick each character using the (now stable) win_cache.
        // wins borrows win_cache; chars is a separate field so split-borrow is safe.
        let wins: &[WinInfo] = &app.win_cache.wins;

        for ch in &mut app.chars {
            // Hot-reload checks (file stat) and effective-config recomputation
            // are throttled to the window-list refresh boundary rather than run
            // every tick. Between refreshes the cached `effective_config` is
            // reused (a cheap stack copy — `Config` holds no heap data).
            if full_refresh {
                let params_changed = ch.config.lock().unwrap().reload_if_changed();
                let pers_changed = ch.behavior_engine.reload_personality_if_changed();
                if params_changed || pers_changed {
                    ch.effective_config =
                        compute_effective_config(&ch.config, &ch.behavior_engine);
                }
            }
            let cfg = ch.effective_config.clone();
            tick_char(ch, &cfg, &si, wins, mt, app.sprite_size);
        }

        // Post-tick: separate resting characters that are too close on the same surface.
        // Two passes ensure a third character gets nudged correctly even if it started
        // at the exact same position as the other two.
        {
            let n = app.chars.len();
            for _ in 0..2 {
                for i in 0..n {
                    for j in (i + 1)..n {
                        let resting_i = matches!(&app.chars[i].anim_state,
                            State::SitIdle { .. } | State::LieIdle { .. } |
                            State::Sleeping { .. } | State::CornerRest { .. }
                        );
                        let resting_j = matches!(&app.chars[j].anim_state,
                            State::SitIdle { .. } | State::LieIdle { .. } |
                            State::Sleeping { .. } | State::CornerRest { .. }
                        );
                        if !resting_i || !resting_j { continue; }

                        // Extract (is_window_top, win_id, position) — copies only, no refs held.
                        let info_i: Option<(bool, u32, f64)> = match &app.chars[i].surface {
                            Surface::Desktop { x }               => Some((false, 0, *x)),
                            Surface::WindowTop { win_id, x_local } => Some((true, *win_id, *x_local)),
                            _ => None,
                        };
                        let info_j: Option<(bool, u32, f64)> = match &app.chars[j].surface {
                            Surface::Desktop { x }               => Some((false, 0, *x)),
                            Surface::WindowTop { win_id, x_local } => Some((true, *win_id, *x_local)),
                            _ => None,
                        };
                        let (is_win_i, id_i, pos_i) = match info_i { Some(v) => v, None => continue };
                        let (is_win_j, id_j, pos_j) = match info_j { Some(v) => v, None => continue };

                        if is_win_i != is_win_j || id_i != id_j { continue; }

                        let half_sprite = app.sprite_size * 0.5;
                        let dist = (pos_i - pos_j).abs();
                        if dist >= half_sprite { continue; }

                        // Nudge char j away from char i by the overlap amount.
                        let nudge = if pos_j >= pos_i { half_sprite - dist } else { -(half_sprite - dist) };
                        // `at_edge` in surface_context fires when pos <= edge_margin + sprite_w/2.
                        // Use the actual last rendered sprite width (not display_width) because
                        // sprites can be wider than display_width after scaling (e.g. turtle s-sit).
                        // edge_margin is 2.0; add 1.0 extra so we stay just outside the zone.
                        let edge_buf = app.chars[j].last_sprite_w / 2.0 + 3.0;
                        match &mut app.chars[j].surface {
                            Surface::Desktop { x } => {
                                let new_x = (*x + nudge).clamp(edge_buf, si.width - edge_buf);
                                *x = new_x;
                            }
                            Surface::WindowTop { win_id, x_local } => {
                                let win_w = macos_wm::find_win(*win_id, &wins)
                                    .map(|w| w.w)
                                    .unwrap_or(f64::MAX);
                                *x_local = (*x_local + nudge).clamp(edge_buf, win_w - edge_buf);
                            }
                            _ => {}
                        }
                    }
                }
            }
        }

        // Speech trigger evaluation.
        if app.speech_cfg.enabled {
            let now = Instant::now();
            let speech_dt = now.duration_since(app.speech_tick).as_secs_f64().min(0.5);
            app.speech_tick = now;
            app.speech_lock_remaining = (app.speech_lock_remaining - speech_dt).max(0.0);
            let lock     = app.speech_lock_remaining;
            let lock_sec = app.speech_cfg.speech_lock_sec;
            let font_sz  = app.font_size;

            // Advance existing bubbles.
            for ch in &mut app.chars {
                if let Some(bs) = &mut ch.bubble_state {
                    bs.remaining_sec -= speech_dt;
                    if bs.remaining_sec <= 0.0 {
                        // Expire.
                        if let Some(p) = ch.bubble_panel.take() {
                            unsafe { p.orderOut(None) };
                        }
                        ch.bubble_state = None;
                    } else {
                        // Update alpha for fade-out.
                        if let Some(p) = &ch.bubble_panel {
                            unsafe { p.setAlphaValue(bs.alpha()) };
                        }
                    }
                }
            }

            // Check for new speech lines.
            let weather_info = app.weather.get();
            for i in 0..app.chars.len() {
                let state = app.chars[i].anim_state.clone();
                if let Some(line) = app.chars[i].speech_engine.tick(&state, lock, weather_info.as_ref()) {
                    app.speech_lock_remaining = lock_sec;

                    // Show bubble.
                    if let Some(bs) = crate::speech::BubbleState::new(&line, &app.lang) {
                        let char_frame = unsafe { app.chars[i].panel.frame() };
                        let existing   = app.chars[i].bubble_panel.as_ref();
                        let panel = show_bubble(existing, &bs.text, font_sz, char_frame, &si, mt);
                        app.chars[i].bubble_panel = Some(panel);
                        app.chars[i].bubble_state = Some(bs);
                    }
                    // Fire OneShot animation alongside speech if specified.
                    if let Some(anim_name) = line.oneshot {
                        let ch = &mut app.chars[i];
                        if ch.assets.animations.contains_key(&anim_name) {
                            let return_to = Box::new(ch.anim_state.clone());
                            ch.anim_state = crate::behavior::State::OneShot {
                                animation: anim_name,
                                frame: 0,
                                frame_elapsed: 0.0,
                                done: false,
                                return_to,
                            };
                        }
                    }
                    break;
                }
            }
        }

        // Behavior animation triggers.
        {
            let weather_info = app.weather.get();
            for i in 0..app.chars.len() {
                let has_bubble = app.chars[i].bubble_state.is_some();
                let state      = app.chars[i].anim_state.clone();
                if let Some(anim_name) = app.chars[i].behavior_engine.tick(
                    &state, has_bubble, weather_info.as_ref(),
                ) {
                    let return_to = Box::new(state);
                    app.chars[i].anim_state = crate::behavior::State::OneShot {
                        animation: anim_name,
                        frame: 0,
                        frame_elapsed: 0.0,
                        done: false,
                        return_to,
                    };
                }
            }
        }

        // Update status item icon to show countdown when a debug trigger is pending.
        let min_remaining: Option<f64> = app.chars.iter()
            .filter_map(|c| c.debug_trigger.as_ref().map(|(_, r)| *r))
            .reduce(f64::min);
        update_status_countdown(&app._status_item, min_remaining, mt);

        // Reposition active bubble panels to track their character.
        for ch in &app.chars {
            if ch.bubble_state.is_none() { continue; }
            if let Some(bp) = &ch.bubble_panel {
                let char_frame = unsafe { ch.panel.frame() };
                let est_h = 60.0 + BUBBLE_TAIL_H;
                let tail_at_bottom = char_frame.origin.y + char_frame.size.height
                    + est_h + BUBBLE_MARGIN < si.height;
                let cx = char_frame.origin.x + char_frame.size.width / 2.0;
                let bp_frame = unsafe { bp.frame() };
                let bubble_x = (cx - bp_frame.size.width / 2.0).max(0.0)
                    .min(si.width - bp_frame.size.width);
                let bubble_y = if tail_at_bottom {
                    char_frame.origin.y + char_frame.size.height + BUBBLE_MARGIN
                } else {
                    char_frame.origin.y - bp_frame.size.height - BUBBLE_MARGIN
                };
                unsafe {
                    bp.setFrameOrigin(NSPoint::new(bubble_x, bubble_y));
                    // Keep bubble above character panel.
                    let char_num: isize = msg_send![&*ch.panel, windowNumber];
                    bp.orderWindow_relativeTo(
                        objc2_app_kit::NSWindowOrderingMode::Above,
                        char_num,
                    );
                }
            }
        }

        // ⌥⌘ hover tracking: when Option+Command is held and the cursor is
        // directly over a character panel, temporarily stop ignoring mouse events
        // so that the local RightMouseDown monitor can intercept the right-click
        // before it reaches the underlying app (Finder / Desktop).
        // All other panels (and this panel outside ⌥⌘) stay transparent to clicks.
        let flags: NSEventModifierFlags = unsafe { msg_send![NSEvent::class(), modifierFlags] };
        let opt_cmd = flags.contains(NSEventModifierFlags::Option)
            && flags.contains(NSEventModifierFlags::Command);
        let mouse_ns = unsafe { NSEvent::mouseLocation() };
        for ch in &app.chars {
            let frame = unsafe { ch.panel.frame() };
            let over = opt_cmd
                && mouse_ns.x >= frame.origin.x
                && mouse_ns.x < frame.origin.x + frame.size.width
                && mouse_ns.y >= frame.origin.y
                && mouse_ns.y < frame.origin.y + frame.size.height;
            unsafe { ch.panel.setIgnoresMouseEvents(!over) };
        }
    });
}


// ---- Entry point ----

pub fn run() {
    let mt = unsafe { MainThreadMarker::new_unchecked() };
    let app = NSApplication::sharedApplication(mt);
    unsafe { app.setActivationPolicy(NSApplicationActivationPolicy::Accessory) };

    // Create the status item as early as possible so that macOS (26+) registers
    // it with Control Center before the app's heavy initialization begins.
    // All menu action handlers guard against AppState not yet being initialized.
    let user_cfg = crate::user_config::load();
    let lang = crate::user_config::resolve_display_language(&user_cfg.display.language);
    let menu_handler = MenuDelegate::new(mt);
    let status_item = make_status_item(&menu_handler, mt, &lang, &user_cfg.weather);

    let bd_cdir = char_dir_for("bearded_dragon").expect("bearded_dragon asset directory not found");
    let pt_cdir = char_dir_for("pond_turtle").expect("pond_turtle asset directory not found");
    let lg_cdir = char_dir_for("leopard_gecko").expect("leopard_gecko asset directory not found");
    let bd_mf = manifest::load(&bd_cdir).expect("bearded_dragon manifest.toml missing or invalid");
    let pt_mf = manifest::load(&pt_cdir).expect("pond_turtle manifest.toml missing or invalid");
    let lg_mf = manifest::load(&lg_cdir).expect("leopard_gecko manifest.toml missing or invalid");
    let bd_config = make_shared(&bd_cdir);
    let pt_config = make_shared(&pt_cdir);
    let lg_config = make_shared(&lg_cdir);
    let sprite_size = user_cfg.display.sprite_size as f64;
    let bd_display_w = sprite_size;
    let pt_display_w = sprite_size;
    let lg_display_w = sprite_size;
    let bd_assets = Rc::new(SpriteAssets::load(&bd_cdir, &bd_mf, bd_display_w).expect("failed to load bearded_dragon sprites"));
    let pt_assets = Rc::new(SpriteAssets::load(&pt_cdir, &pt_mf, pt_display_w).expect("failed to load pond_turtle sprites"));
    let lg_assets = Rc::new(SpriteAssets::load(&lg_cdir, &lg_mf, lg_display_w).expect("failed to load leopard_gecko sprites"));

    let si = macos_wm::screen_info(mt)
        .unwrap_or(ScreenInfo { width: 1280.0, height: 800.0, dock_height: 0.0, menu_bar_height: 24.0 });

    let demo_mode = std::env::args().any(|a| a == "--demo");
    let initial_chars: Vec<CharState> = if demo_mode {
        // Demo mode: one bearded dragon with deterministic scripted behavior.
        vec![spawn_char(Rc::clone(&bd_assets), bd_config.clone(), &si, mt, true, "bearded_dragon")]
    } else {
        user_cfg
            .characters
            .startup_species_ids()
            .into_iter()
            .map(|species| match species {
                "bearded_dragon" => spawn_char(
                    Rc::clone(&bd_assets),
                    bd_config.clone(),
                    &si,
                    mt,
                    false,
                    species,
                ),
                "pond_turtle" => spawn_char(
                    Rc::clone(&pt_assets),
                    pt_config.clone(),
                    &si,
                    mt,
                    false,
                    species,
                ),
                "leopard_gecko" => spawn_char(
                    Rc::clone(&lg_assets),
                    lg_config.clone(),
                    &si,
                    mt,
                    false,
                    species,
                ),
                _ => unreachable!("startup_species_ids only returns built-in species"),
            })
            .collect()
    };

    let weather_handle = crate::weather::spawn(&user_cfg.weather);

    // Register ⌘+drag event monitors.
    let event_monitors = setup_drag_monitors();

    // 60 Hz base timer; tick() skips frames to enforce the configured tick_rate.
    let blk = RcBlock::new(|_: NonNull<NSTimer>| tick());
    let timer =
        unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(1.0 / 60.0, true, &blk) };
    unsafe {
        let common: &NSRunLoopMode = &*(NSRunLoopCommonModes as *const NSRunLoopMode);
        NSRunLoop::mainRunLoop().addTimer_forMode(&timer, common);
    }

    // IOKit notification: update tick_rate immediately when AC/battery changes.
    crate::power::register_power_notification(power_source_changed);

    APP.with(|cell| {
        *cell.borrow_mut() = Some(AppState {
            chars: initial_chars,
            bd_assets,
            pt_assets,
            lg_assets,
            bd_config,
            pt_config,
            lg_config,
            _menu_handler: menu_handler,
            _status_item: status_item,
            _timer: timer,
            _event_monitors: event_monitors,
            debug_menu_char: 0,
            debug_menu_targets: Vec::new(),
            speech_lock_remaining: 0.0,
            speech_cfg: user_cfg.speech,
            speech_tick: Instant::now(),
            font_size: user_cfg.display.font_size as f64,
            sprite_size: user_cfg.display.sprite_size as f64,
            lang: lang,
            weather: weather_handle,
            weather_cfg: user_cfg.weather,
            win_cache: WinListCache::new(),
            tick_rate_ac: user_cfg.display.tick_rate_ac.max(1),
            tick_rate_battery: user_cfg.display.tick_rate_battery.max(1),
            tick_rate: if crate::power::on_ac_power() {
                user_cfg.display.tick_rate_ac
            } else {
                user_cfg.display.tick_rate_battery
            }.max(1),
            last_full_tick: Instant::now(),
        });
    });

    unsafe { app.run() };
}
