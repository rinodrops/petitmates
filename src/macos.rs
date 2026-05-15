#![cfg(target_os = "macos")]
#![allow(non_snake_case, unused_unsafe, deprecated)]

use std::cell::RefCell;
use std::path::PathBuf;
use std::ptr::NonNull;
use std::rc::Rc;
use std::time::Instant;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{define_class, msg_send, AnyThread, ClassType, MainThreadOnly};
use objc2_app_kit::{
    NSAlert, NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSBezierPath,
    NSColor, NSEvent, NSEventMask, NSEventModifierFlags, NSFont, NSImage, NSMenu, NSMenuDelegate,
    NSMenuItem, NSPanel, NSStatusBar, NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_foundation::{
    MainThreadMarker, NSBundle, NSObject, NSObjectProtocol, NSPoint,
    NSRect, NSRunLoop, NSRunLoopMode, NSSize, NSString, NSTimer,
};

use crate::assets::{make_image_view, Anchor, SpriteAssets};
use crate::behavior::{BehaviorContext, BehaviorScript, Dir, LandingMode, Side, State, Surface, Transition};
use crate::config::{make_shared, Config, SharedConfig};
use crate::engine::advance_anim;
use crate::manifest;
use crate::demo_behavior::DemoBehavior;
use crate::rust_behavior::RustBehavior;
use crate::sprite_map::{sprite_for_state, sprite_for_turn};
use crate::wm::{self, ScreenInfo, WinInfo};

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
        | Surface::WindowUpperCorner { win_id, .. } => Some(*win_id),
        _ => None,
    }
}

// ---- Per-character state ----

struct CharState {
    panel: Retained<NSPanel>,
    assets: Rc<SpriteAssets>,
    config: SharedConfig,
    behavior: Box<dyn BehaviorScript>,
    anim_state: State,
    facing: Dir,
    surface: Surface,
    /// Character position in CG coordinates (origin = screen top-left, Y down).
    char_pos: (f64, f64),
    last_tick: Instant,
    /// Mouse offset from panel origin in NS coords when dragging; None otherwise.
    drag_offset: Option<(f64, f64)>,
    /// Pending debug forced transition: (target_state, remaining_countdown_secs).
    debug_trigger: Option<(State, f64)>,
    speech_engine: crate::speech::SpeechEngine,
    /// Active speech bubble state; None when no bubble is shown.
    bubble_state: Option<crate::speech::BubbleState>,
    /// The transparent NSPanel that renders the speech bubble; None when hidden.
    bubble_panel: Option<Retained<NSPanel>>,
}

// ---- App-wide state (singletons) ----

struct AppState {
    chars: Vec<CharState>,
    bd_assets: Rc<SpriteAssets>,
    pt_assets: Rc<SpriteAssets>,
    bd_config: SharedConfig,
    pt_config: SharedConfig,
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
    /// Resolved display language: "ja" or "en".
    lang: String,
    /// Shared weather cache updated by the background weather thread.
    weather: crate::weather::WeatherHandle,
}

thread_local! {
    static APP: RefCell<Option<AppState>> = RefCell::new(None);
}

// ---- Panel helpers ----

/// Detect the OS preferred language, returning `"ja"` or `"en"`.
///
/// Checks `NSLocale.preferredLanguages` in order; returns `"ja"` when
/// the first language whose tag starts with `"ja"` appears before any
/// English tag.  Falls back to `"en"`.
fn detect_system_language() -> String {
    use objc2_foundation::NSLocale;
    let langs = unsafe { NSLocale::preferredLanguages() };
    for i in 0..langs.len() {
        let tag: String = unsafe { langs.objectAtIndex(i).to_string() };
        if tag.starts_with("ja") {
            return "ja".to_owned();
        }
        if tag.starts_with("en") {
            return "en".to_owned();
        }
    }
    "en".to_owned()
}

fn make_panel(image: &NSImage, mt: MainThreadMarker) -> Retained<NSPanel> {
    let sz = unsafe { image.size() };
    unsafe {
        let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
            NSPanel::alloc(mt),
            NSRect::new(NSPoint::ZERO, sz),
            NSWindowStyleMask::from_bits_retain(128), // Borderless | NonactivatingPanel
            NSBackingStoreType::Buffered,
            false,
        );
        panel.setBackgroundColor(Some(&NSColor::clearColor()));
        panel.setOpaque(false);
        panel.setHasShadow(false);
        panel.setLevel(0); // NSNormalWindowLevel — lets other windows occlude the character
        panel.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::FullScreenAuxiliary,
        );
        panel.setIgnoresMouseEvents(true);
        panel.setAlphaValue(1.0);
        panel.setContentView(Some(&*make_image_view(image, mt)));
        panel
    }
}

/// Replace the panel's content with a new sprite.
fn swap_sprite(
    panel: &NSPanel,
    assets: &SpriteAssets,
    name: &str,
    mirror: bool,
    mt: MainThreadMarker,
) {
    if let Some(img) = assets.image(name, mirror) {
        let sz = unsafe { img.size() };
        unsafe {
            // Resize panel BEFORE setting content view so NSImageView
            // does not autoresize to the old panel dimensions.
            panel.setContentSize(sz);
            panel.setContentView(Some(&*make_image_view(img, mt)));
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

fn make_status_item(
    handler: &MenuDelegate,
    mt: MainThreadMarker,
    lang: &str,
) -> Retained<objc2_app_kit::NSStatusItem> {
    let ja = lang == "ja";
    unsafe {
        let bar = NSStatusBar::systemStatusBar();
        let item = bar.statusItemWithLength(-2.0); // NSSquareStatusItemLength
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
            &NSString::from_str(if ja { "フトアゴヒゲトカゲを追加" } else { "Add Bearded Dragon" }),
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
            &NSString::from_str(if ja { "設定ファイルを開く" } else { "Open Settings File" }),
            Some(objc2::sel!(openSettingsFile:)),
            &NSString::from_str(""),
        );
        let (): () = unsafe { objc2::msg_send![&*settings, setTarget: handler] };
        menu.addItem(&settings);

        menu.addItem(&NSMenuItem::separatorItem(mt));

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

// ---- Startup drop position ----

/// Choose the initial drop position in CG coordinates (top-left of sprite).
/// Always picks a random X within the center 80% of the screen so every
/// launch starts fresh — no persistent state that could lock in bugs.
fn startup_drop(si: &ScreenInfo, assets: &SpriteAssets) -> (f64, f64) {
    use rand::{SeedableRng, Rng};
    let stand_w = assets
        .image("s-stand", false)
        .map(|img| unsafe { img.size() }.width)
        .unwrap_or(150.0);
    // Start at the top of the visible screen area (below the menu bar).
    let start_cg_y = si.menu_bar_height;
    // Random x within the center 80% of the screen width.
    let margin = si.width * 0.10;
    let usable = (si.width - margin * 2.0 - stand_w).max(0.0);
    let offset = rand::rngs::SmallRng::from_os_rng().random::<f64>() * usable;
    (margin + offset, start_cg_y)
}

// ---- Spawn helper ----

/// Create one character, place it in a startup-drop position, and return
/// the fully initialised `CharState`.  Caller is responsible for pushing it
/// into `AppState::chars`.
fn spawn_char(assets: Rc<SpriteAssets>, config: SharedConfig, si: &ScreenInfo, mt: MainThreadMarker, demo: bool, char_name: &str) -> CharState {
    let (start_cx, start_cy) = startup_drop(si, &assets);
    let init_img = assets.image("s-stand", false).expect("s-stand.png missing");
    let panel = make_panel(init_img, mt);
    let sz = unsafe { init_img.size() };
    unsafe {
        panel.setFrameOrigin(NSPoint::new(start_cx, si.height - start_cy - sz.height));
        panel.orderFront(None);
    }
    CharState {
        panel,
        assets,
        config,
        behavior: if demo {
            Box::new(DemoBehavior::new()) as Box<dyn crate::behavior::BehaviorScript>
        } else {
            Box::new(RustBehavior::new())
        },
        anim_state: State::Falling { vx: 0.0, vy: 0.0, shocked: 0.0 },
        facing: Dir::Left,
        surface: Surface::Airborne,
        char_pos: (start_cx, start_cy),
        last_tick: Instant::now(),
        drag_offset: None,
        debug_trigger: None,
        speech_engine: crate::speech::SpeechEngine::new(crate::speech::load(char_name)),
        bubble_state: None,
        bubble_panel: None,
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
        /// Gray out "Remove" when only one character remains.
        #[unsafe(method(menuWillOpen:))]
        fn menu_will_open(&self, menu: &NSMenu) {
            APP.with(|cell| {
                let b = cell.borrow();
                let Some(app) = b.as_ref() else { return };
                let enabled = app.chars.len() > 1;
                unsafe {
                    // Tag 1 is set on the Remove item in make_status_item.
                    let item: Option<&NSMenuItem> = msg_send![menu, itemWithTag: 1_isize];
                    if let Some(item) = item {
                        let (): () = msg_send![item, setEnabled: enabled];
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
                let si = wm::screen_info(mt).unwrap_or(ScreenInfo {
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
                let si = wm::screen_info(mt).unwrap_or(ScreenInfo {
                    width: 1280.0, height: 800.0, dock_height: 0.0, menu_bar_height: 24.0,
                });
                let assets = Rc::clone(&app.pt_assets);
                let config = app.pt_config.clone();
                app.chars.push(spawn_char(assets, config, &si, mt, false, "pond_turtle"));
            });
        }

        /// Open user.toml in the system default text editor.
        #[unsafe(method(openSettingsFile:))]
        fn open_settings_file(&self, _sender: &AnyObject) {
            crate::user_config::open_in_editor();
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
            let si_height = wm::screen_info_raw();
            app.chars[drag_idx].char_pos = (new_ns_x, si_height - new_ns_y - sz.height);
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

            let si = wm::screen_info_raw_full();
            let wins = wm::list_windows(&si);
            let cfg = app.chars[drag_idx].config.lock().unwrap().current.clone();

            let ch = &mut app.chars[drag_idx];

            // Sprite dimensions for foot position.
            let sr = sprite_for_state(&ch.anim_state, ch.facing, &ch.assets.animations);
            let (fw, fh) = ch.assets.image(&sr.name, sr.mirror)
                .map(|img| { let sz = unsafe { img.size() }; (sz.width, sz.height) })
                .unwrap_or((150.0, 150.0));
            let foot_x = ch.char_pos.0 + fw / 2.0;
            let foot_y = ch.char_pos.1 + fh;

            // Try to snap to a nearby surface.
            let new_surface = wm::find_surface_near(foot_x, foot_y, &wins, &si);
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
                        jump_target: None,
                        attract_target: None,
                    };
                    let new_anim = ch.behavior.on_landed(&ctx);
                    let stand_anchor = ch.assets.anchor("s-stand")
                        .unwrap_or(crate::assets::Anchor { x: 0.0, y: 0.0 });
                    let stand_h = ch.assets.image("s-stand", false)
                        .map(|img| unsafe { img.size() }.height)
                        .unwrap_or(fh);
                    let snap_y = match &surf {
                        Surface::WindowTop { win_id, .. } =>
                            wm::find_win(*win_id, &wins).map(|w| w.y),
                        Surface::Desktop { .. } => {
                            let si2 = wm::screen_info_raw_full();
                            Some(si2.floor_y())
                        }
                        _ => None,
                    };
                    if let Some(surface_y) = snap_y {
                        ch.char_pos = (foot_x - fw / 2.0, surface_y - stand_h + stand_anchor.y);
                    }
                    ch.surface = surf;
                    ch.anim_state = new_anim;
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
            let Some(win) = wm::find_win(*win_id, wins) else {
                return NSPoint::ZERO;
            };
            let ns_top = si.height - win.y;
            NSPoint::new(win.x + x_local - sw / 2.0, ns_top - anchor.y)
        }

        // Wall: grip line at y_local below window top.
        // anchor.x = distance from **right** edge to grip line (for right wall).
        // Mirrored sprite (left wall): grip is the same distance from the left edge.
        Surface::WindowWall { win_id, side, y_local } => {
            let Some(win) = wm::find_win(*win_id, wins) else {
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
            let Some(win) = wm::find_win(*win_id, wins) else {
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

    }
}

/// Compute `surface_progress`, `at_edge`, `jump_target`, and `attract_target`
/// for the current surface.
/// `jump_floor_margin` — windows above this many px from the Dock are excluded.
/// `attract_dist` — detection radius for spontaneous floor→window attraction.
/// `corner_attract_dist` — detection radius for corner-to-window jump (corner_jump_dist).
fn surface_context(
    surface: &Surface,
    char_pos: (f64, f64),
    sprite_w: f64,
    facing: Dir,
    jump_max_dist: f64,
    jump_floor_margin: f64,
    attract_dist: f64,
    corner_attract_dist: f64,
    wins: &[WinInfo],
    si: &ScreenInfo,
) -> (f64, bool, Option<(u32, Side)>, Option<(u32, Side, LandingMode)>) {
    let edge_margin = 2.0;
    match surface {
        Surface::WindowTop { win_id, x_local } => {
            let Some(win) = wm::find_win(*win_id, wins) else {
                return (0.5, false, None, None);
            };
            let progress = (x_local / win.w).clamp(0.0, 1.0);
            let at_edge = *x_local <= edge_margin + sprite_w / 2.0
                || *x_local >= win.w - edge_margin - sprite_w / 2.0;
            (progress, at_edge, None, None)
        }
        Surface::WindowUpperCorner { win_id, side } => {
            let Some(win) = wm::find_win(*win_id, wins) else {
                return (0.5, false, None, None);
            };
            let corner_cx = match side {
                Side::Left  => win.x,
                Side::Right => win.right(),
            };
            let corner_cy = win.y;
            let attract_target = wins.iter()
                .filter_map(|w| {
                    if w.id == *win_id { return None; }
                    let dist_r = w.x - corner_cx;
                    let dist_l = corner_cx - w.right();
                    // Determine how the character lands on the target wall based on
                    // the vertical relationship between the current corner and the target window.
                    let landing_mode = if w.y > corner_cy {
                        // Target window starts below current corner → step down onto its top.
                        LandingMode::TopLanding
                    } else if w.y + w.h > corner_cy {
                        // Target window straddles current corner height → start from bottom, climb up.
                        LandingMode::ClimbFromBottom
                    } else {
                        // Target window is entirely above → snap to character's current Y (clamped).
                        LandingMode::ClimbFromCurrent
                    };
                    if dist_r >= 0.0 && dist_r < corner_attract_dist {
                        Some((w.id, Side::Left, dist_r, landing_mode))
                    } else if dist_l >= 0.0 && dist_l < corner_attract_dist {
                        Some((w.id, Side::Right, dist_l, landing_mode))
                    } else {
                        None
                    }
                })
                .min_by(|a, b| a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal))
                .map(|(id, s, _, lm)| (id, s, lm));
            let progress = if *side == Side::Left { 0.0 } else { 1.0 };
            (progress, false, None, attract_target)
        }
        Surface::Desktop { x } => {
            let progress = (x / si.width).clamp(0.0, 1.0);
            let (cx, _) = char_pos;
            let floor_y = si.floor_y();
            // jump_target: only in current walking direction, within jump_max_dist.
            let jump_target = wins.iter().find_map(|win| {
                if win.bottom() < floor_y - jump_floor_margin { return None; }
                match facing {
                    Dir::Left => {
                        let dist = cx - win.right();
                        if dist >= 0.0 && dist < jump_max_dist { Some((win.id, Side::Right)) }
                        else { None }
                    }
                    Dir::Right => {
                        let dist = win.x - cx;
                        if dist >= 0.0 && dist < jump_max_dist { Some((win.id, Side::Left)) }
                        else { None }
                    }
                }
            });
            // attract_target: nearest window in either direction within attract_dist.
            let attract_target = wins.iter()
                .filter_map(|win| {
                    if win.bottom() < floor_y - jump_floor_margin { return None; }
                    let dist_r = win.x - cx;
                    let dist_l = cx - win.right();
                    if dist_r >= 0.0 && dist_r < attract_dist {
                        Some((win.id, Side::Left, dist_r))
                    } else if dist_l >= 0.0 && dist_l < attract_dist {
                        Some((win.id, Side::Right, dist_l))
                    } else {
                        None
                    }
                })
                .min_by(|a, b| a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal))
                .map(|(id, side, _)| (id, side, LandingMode::ClimbFromBottom));
            let at_edge = *x <= edge_margin + sprite_w / 2.0
                || *x >= si.width - edge_margin - sprite_w / 2.0;
            (progress, at_edge, jump_target, attract_target)
        }
        Surface::WindowWall { win_id, y_local, .. } => {
            let Some(win) = wm::find_win(*win_id, wins) else {
                return (0.5, false, None, None);
            };
            let progress = (y_local / win.h).clamp(0.0, 1.0);
            let at_edge = *y_local <= edge_margin || *y_local >= win.h - edge_margin;
            (progress, at_edge, None, None)
        }
        _ => (0.5, false, None, None),
    }
}

fn tick_char(
    ch: &mut CharState,
    cfg: &Config,
    si: &ScreenInfo,
    wins: &[WinInfo],
    mt: MainThreadMarker,
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
    if !wm::surface_still_valid(&ch.surface, wins) {
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
                jump_target: None,
                attract_target: None,
            };
            ch.behavior.on_surface_lost(&ctx)
        };

        // Reposition char_pos so the falling sprite appears centered on the
        // same screen point as the sprite that was just visible.
        // char_pos (kept in sync each tick) is the CG top-left of the
        // current sprite; compute its center, then derive the new top-left
        // for the falling sprite so it shares that center.
        {
            let sr_cur = sprite_for_state(&ch.anim_state, ch.facing, &ch.assets.animations);
            let sr_new = sprite_for_state(&fallback, ch.facing, &ch.assets.animations);
            if let (Some(img_cur), Some(img_new)) = (
                ch.assets.image(&sr_cur.name, sr_cur.mirror),
                ch.assets.image(&sr_new.name, sr_new.mirror),
            ) {
                let cur_sz = unsafe { img_cur.size() };
                let new_sz = unsafe { img_new.size() };
                let center_cx = ch.char_pos.0 + cur_sz.width  / 2.0;
                let center_cy = ch.char_pos.1 + cur_sz.height / 2.0;
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

    // Update char_pos for Airborne / Walking states.
    match &ch.anim_state {
        State::Falling { vx, vy, .. } => {
            let (vx, vy) = (*vx, *vy);
            let (cx, cy) = ch.char_pos;
            ch.char_pos = (cx + vx * dt, cy + vy * dt);
        }
        State::Walking { dir, .. } => {
            // Advance local position within the surface.
            let speed = cfg.floor.walk_speed;
            let delta = speed * dt;
            match &mut ch.surface {
                Surface::Desktop { x } => {
                    *x += match dir { Dir::Left => -delta, Dir::Right => delta };
                    // Clamp so the character never walks off the visible screen area.
                    let half_w = cfg.display.display_width / 2.0;
                    *x = x.clamp(half_w, si.width - half_w);
                    ch.char_pos.0 = *x;
                }
                Surface::WindowTop { x_local, .. } => {
                    *x_local += match dir { Dir::Left => -delta, Dir::Right => delta };
                }
                _ => {}
            }
        }
        State::ClimbingUp { .. } => {
            if let Surface::WindowWall { y_local, .. } = &mut ch.surface {
                *y_local -= cfg.wall.climb_speed * dt;
                *y_local = y_local.max(0.0);
            }
        }
        State::ClimbingDown { .. } => {
            // y_local update done via re-borrow below (borrow checker limitation)
        }
        _ => {}
    }
    // ClimbingDown separate borrow
    if matches!(&ch.anim_state, State::ClimbingDown { .. }) {
        if let Surface::WindowWall { win_id, y_local, .. } = &mut ch.surface {
            if let Some(win) = wm::find_win(*win_id, wins) {
                *y_local += cfg.wall.climb_speed * dt;
                *y_local = y_local.min(win.h);
            }
        }
    }

    // Gravity for Falling state (cap vy to prevent tunneling).
    if let State::Falling { vy, .. } = &mut ch.anim_state {
        *vy = (*vy + cfg.jump.gravity * 60.0 * dt).min(600.0);
    }

    // Off-screen safeguard: if the character has fallen more than one
    //     sprite-height below the screen bottom (or drifted far off the
    //     sides), reset to a fresh startup drop so the run loop can
    //     continue.
    {
        let (fw, fh) = ch.assets.image("s-stand", false)
            .map(|img| { let sz = unsafe { img.size() }; (sz.width, sz.height) })
            .unwrap_or((150.0, 150.0));
        let (cx, cy) = ch.char_pos;
        let below_screen = cy > si.height + fh;
        let off_sides    = cx < -(fw * 3.0) || cx > si.width + fw * 3.0;
        if below_screen || off_sides {
            let (drop_x, drop_y) = startup_drop(si, &ch.assets);
            ch.char_pos  = (drop_x, drop_y);
            ch.surface   = Surface::Airborne;
            ch.anim_state = State::Falling { vx: 0.0, vy: 0.0, shocked: 0.0 };
        }
    }

    // Landing detection (swept check: did foot cross a surface boundary?).
    if let State::Falling { vy, .. } = &ch.anim_state {
        if *vy >= 0.0 {
            let (fw, fh) = ch.assets.image("s-jump", false)
                .or_else(|| ch.assets.image("s-stand", false))
                .map(|img| { let sz = unsafe { img.size() }; (sz.width, sz.height) })
                .unwrap_or((150.0, 150.0));
            let foot_x = ch.char_pos.0 + fw / 2.0;
            let _foot_y_prev = prev_cy + fh;
            let foot_y_now  = ch.char_pos.1 + fh;

            let floor_y = si.floor_y();

            let cy_prev = prev_cy;
            let cy_now  = ch.char_pos.1;
            let landed_win = wins.iter()
                .filter(|win| {
                    win.y < floor_y
                        && foot_x >= win.x
                        && foot_x <= win.right()
                        && cy_prev < win.y
                        && cy_now + fh >= win.y
                })
                .min_by(|a, b| a.y.partial_cmp(&b.y).unwrap_or(std::cmp::Ordering::Equal));
            let landed_floor = landed_win.is_none()
                && foot_y_now >= floor_y;

            let new_surface = landed_win
                .map(|win| Surface::WindowTop {
                    win_id: win.id,
                    x_local: (foot_x - win.x).clamp(0.0, win.w),
                })
                .or_else(|| landed_floor.then(|| {
                let half_w = cfg.display.display_width / 2.0;
                let clamped_x = foot_x.clamp(half_w, si.width - half_w);
                Surface::Desktop { x: clamped_x }
            }));

            if let Some(new_surface) = new_surface {
                let new_anim = {
                    let ctx = BehaviorContext {
                        state: &ch.anim_state,
                        surface: &new_surface,
                        elapsed_secs: 0.0,
                        config: cfg,
                        rng01: 0.0,
                        surface_progress: 0.5,
                        facing: ch.facing,
                        at_edge: false,
                        jump_target: None,
                        attract_target: None,
                    };
                    ch.behavior.on_landed(&ctx)
                };
                let stand_anchor = ch.assets.anchor("s-stand").unwrap_or(Anchor { x: 0.0, y: 0.0 });
                let stand_h = ch.assets.image("s-stand", false)
                    .map(|img| unsafe { img.size() }.height)
                    .unwrap_or(fh);
                let snap_y = match &new_surface {
                    Surface::WindowTop { win_id, .. } =>
                        wm::find_win(*win_id, wins).map(|w| w.y),
                    Surface::Desktop { .. } => Some(floor_y),
                    _ => None,
                };
                if let Some(surface_y) = snap_y {
                    ch.char_pos = (
                        foot_x - fw / 2.0,
                        surface_y - stand_h + stand_anchor.y,
                    );
                }
                ch.surface = new_surface;
                ch.anim_state = new_anim;
            }
        }
    }

    // Compute surface_progress, at_edge, jump_target.
    let sr_for_ctx = match &ch.anim_state {
        State::TurningAround { elapsed, .. } => {
            let progress = (*elapsed / cfg.floor.turn_duration).clamp(0.0, 1.0);
            sprite_for_turn(progress, ch.facing)
        }
        other => sprite_for_state(other, ch.facing, &ch.assets.animations),
    };
    let sprite_sz = ch.assets.image(&sr_for_ctx.name, sr_for_ctx.mirror)
        .map(|img| { let sz = unsafe { img.size() }; (sz.width, sz.height) })
        .unwrap_or((150.0, 150.0));
    let (surface_progress, at_edge, jump_target, attract_target) =
        surface_context(&ch.surface, ch.char_pos, sprite_sz.0, ch.facing,
                        cfg.jump.wall_jump_max_dist, cfg.jump.wall_jump_floor_margin,
                        cfg.jump.climb_attract_dist, cfg.corner.corner_jump_dist, wins, si);

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
            jump_target,
            attract_target,
        };
        ch.behavior.next_state(&ctx)
    };
    match transition {
        Transition::Stay => {}
        Transition::To(new_state) => {
            let mut new_state = new_state;
            // Complete a turn: update facing before entering the new state.
            if let Some(dir) = turn_to_dir {
                ch.facing = dir;
            }
            // When transitioning to Falling from a wall surface, compute
            // char_pos (CG top-left of sprite) from the wall position now,
            // before the surface is overwritten with Airborne.
            if matches!(&new_state, State::Falling { .. }) {
                let fall_pos: Option<(f64, f64)> = (|| {
                    let (sw, sh) = ch.assets.image("s-jump", false)
                        .or_else(|| ch.assets.image("s-stand", false))
                        .map(|img| { let sz = unsafe { img.size() }; (sz.width, sz.height) })
                        .map(|sz| (sz.0, sz.1))
                        .or(Some((150.0, 150.0)))?;
                    match &ch.surface {
                        Surface::WindowWall { win_id, side, y_local } => {
                            let win = wm::find_win(*win_id, wins)?;
                            let cg_y = win.y + y_local - sh / 2.0;
                            let cg_x = match side {
                                Side::Right => win.right() - sw,
                                Side::Left  => win.x,
                            };
                            Some((cg_x, cg_y))
                        }
                        _ => None,
                    }
                })();
                if let Some(pos) = fall_pos {
                    ch.char_pos = pos;
                }
            }
            // Keep ch.surface in sync when the new state implies a surface change.
            let new_surface: Option<Surface> = match (&new_state, &ch.surface) {
                // Falling always means the character is airborne, regardless of
                // which surface it was on before.  Without this, the panel
                // position would continue to be derived from wall coordinates
                // (y_local) instead of char_pos, so the character would appear
                // frozen despite physics updating char_pos.
                (State::Falling { .. }, _) => Some(Surface::Airborne),
                (State::CornerTransitionSide { side, .. }, Surface::WindowTop { win_id, .. }) => {
                    Some(Surface::WindowUpperCorner { win_id: *win_id, side: *side })
                }
                (State::CornerTransitionSide { side, .. }, Surface::WindowWall { win_id, .. }) => {
                    Some(Surface::WindowUpperCorner { win_id: *win_id, side: *side })
                }
                (State::ClimbingDown { .. }, Surface::WindowUpperCorner { win_id, side }) => {
                    let y_local = ch.assets.image("s-hang-wall-0", false)
                        .map(|img| unsafe { img.size() }.height / 2.0)
                        .unwrap_or(4.0);
                    Some(Surface::WindowWall { win_id: *win_id, side: *side, y_local })
                }
                (State::Walking { .. }, Surface::WindowUpperCorner { win_id, side }) => {
                    let walk_w = ch.assets.image("s-walk-0", false)
                        .map(|img| unsafe { img.size() }.width)
                        .unwrap_or(sprite_sz.0);
                    let x_offset = walk_w / 2.0 + 3.0;
                    let x = match side {
                        Side::Left  => x_offset,
                        Side::Right => wm::find_win(*win_id, wins)
                            .map(|w| w.w - x_offset)
                            .unwrap_or(400.0 - x_offset),
                    };
                    Some(Surface::WindowTop { win_id: *win_id, x_local: x })
                }
                (State::WallEntry { .. }, Surface::Desktop { .. })
                | (State::WallEntry { .. }, Surface::WindowTop { .. })
                | (State::WallEntry { .. }, Surface::WindowUpperCorner { .. }) => {
                    if let State::JumpRunup { target_win_id, target_side, landing_mode, .. } = &ch.anim_state {
                        if let Some(win) = wm::find_win(*target_win_id, wins) {
                            let side = *target_side;
                            let hang_h = ch.assets.image("s-hang-wall-0", false)
                                .map(|img| unsafe { img.size() }.height)
                                .unwrap_or(150.0);
                            let stand_w = ch.assets.image("s-stand", false)
                                .map(|img| unsafe { img.size() }.width)
                                .unwrap_or(150.0);
                            match landing_mode {
                                LandingMode::TopLanding => {
                                    // Step directly onto the target window's top edge.
                                    // Arrive near the edge the character jumped from.
                                    let x_local = match side {
                                        Side::Left  => stand_w / 2.0 + 4.0,
                                        Side::Right => win.w - stand_w / 2.0 - 4.0,
                                    };
                                    ch.char_pos.0 = win.x + x_local;
                                    ch.char_pos.1 = win.y;
                                    ch.facing = match side {
                                        Side::Left  => Dir::Right,
                                        Side::Right => Dir::Left,
                                    };
                                    // Override: go directly to Observing instead of WallEntry.
                                    new_state = State::Observing { elapsed: 0.0, duration: 3.0 };
                                    Some(Surface::WindowTop { win_id: win.id, x_local })
                                }
                                LandingMode::ClimbFromCurrent => {
                                    // Snap to the character's current Y projected onto the target wall.
                                    let cur_y = ch.char_pos.1;
                                    let y_local = (cur_y - win.y).clamp(hang_h / 2.0, win.h - 4.0);
                                    ch.char_pos.0 = match side {
                                        Side::Right => win.right() - stand_w,
                                        Side::Left  => win.x,
                                    };
                                    ch.char_pos.1 = win.y + y_local;
                                    ch.facing = match side {
                                        Side::Left  => Dir::Right,
                                        Side::Right => Dir::Left,
                                    };
                                    Some(Surface::WindowWall { win_id: win.id, side, y_local })
                                }
                                LandingMode::ClimbFromBottom => {
                                    // Snap near the bottom of the target wall.
                                    let y_local = (win.h - hang_h / 2.0).clamp(hang_h / 2.0, win.h - 4.0);
                                    ch.char_pos.0 = match side {
                                        Side::Right => win.right() - stand_w,
                                        Side::Left  => win.x,
                                    };
                                    ch.char_pos.1 = win.y + y_local;
                                    ch.facing = match side {
                                        Side::Left  => Dir::Right,
                                        Side::Right => Dir::Left,
                                    };
                                    Some(Surface::WindowWall { win_id: win.id, side, y_local })
                                }
                            }
                        } else { None }
                    } else { None }
                }
                _ => None,
            };
            if let Some(ns) = new_surface {
                ch.surface = ns;
            }
            if matches!(&new_state, State::ClimbingUp { .. } | State::ClimbingDown { .. }) {
                if let Surface::WindowWall { side, .. }
                     | Surface::WindowUpperCorner { side, .. } = &ch.surface
                {
                    ch.facing = match side {
                        Side::Left  => Dir::Right,
                        Side::Right => Dir::Left,
                    };
                }
            }
            if let State::CornerTransitionSide { side, .. } = &new_state {
                ch.facing = match side {
                    Side::Left  => Dir::Right,
                    Side::Right => Dir::Left,
                };
            }
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

    // Keep facing in sync with Walking direction.
    if let State::Walking { dir, .. } = &ch.anim_state {
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
    swap_sprite(&ch.panel, &ch.assets, &sr.name, sr.mirror, mt);

    // Move panel to surface-derived NS origin.
    let anchor = ch.assets.anchor(&sr.name).unwrap_or(Anchor { x: 0.0, y: 0.0 });
    let stand_anchor_y = ch.assets.anchor("s-stand").map(|a| a.y).unwrap_or(0.0);
    let sz = ch.assets.image(&sr.name, sr.mirror)
        .map(|img| { let sz = unsafe { img.size() }; (sz.width, sz.height) })
        .unwrap_or((150.0, 150.0));
    let origin = surface_to_ns_origin(&ch.surface, ch.char_pos, sz, anchor, stand_anchor_y, wins, si);
    unsafe { ch.panel.setFrameOrigin(origin) };

    // Keep char_pos in sync with the rendered panel position so that when a
    // window surface is lost the character starts falling from the correct
    // location rather than from the stale position of its last airborne state.
    if !matches!(ch.surface, Surface::Airborne) {
        ch.char_pos = (origin.x, si.height - origin.y - sz.1);
    }

    // Z-order: place the panel just above its host window so windows in front
    // of the host naturally occlude the character.
    unsafe {
        if let Some(wid) = surface_host_win_id(&ch.surface) {
            // NSWindowAbove = 1; relativeTo: takes the CGWindowNumber of the target.
            let (): () = msg_send![&*ch.panel, orderWindow: 1_isize relativeTo: wid as isize];
        } else {
            // Desktop / Airborne: bring to front within the normal level.
            ch.panel.orderFront(None);
        }
    }

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

fn tick() {
    APP.with(|cell| {
        let mut b = cell.borrow_mut();
        let Some(app) = b.as_mut() else { return };
        let mt = unsafe { MainThreadMarker::new_unchecked() };

        // Compute screen info and window list once for all characters.
        let si = wm::screen_info(mt).unwrap_or(ScreenInfo {
            width: 1280.0, height: 800.0, dock_height: 0.0, menu_bar_height: 24.0,
        });
        let wins = wm::list_windows(&si);

        for ch in &mut app.chars {
            ch.config.lock().unwrap().reload_if_changed();
            let cfg = ch.config.lock().unwrap().current.clone();
            tick_char(ch, &cfg, &si, &wins, mt);
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
                    break;
                }
            }
        }

        // Update status item icon to show countdown when a debug trigger is pending.
        let min_remaining: Option<f64> = app.chars.iter()
            .filter_map(|c| c.debug_trigger.as_ref().map(|(_, r)| *r))
            .reduce(f64::min);
        update_status_countdown(&app._status_item, min_remaining, mt);

        // Reposition active bubble panels to track their character.
        let font_sz = app.font_size;
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
                let _ = font_sz; // suppress unused warning
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

    let bd_cdir = char_dir_for("bearded_dragon").expect("bearded_dragon asset directory not found");
    let pt_cdir = char_dir_for("pond_turtle").expect("pond_turtle asset directory not found");
    let bd_mf = manifest::load(&bd_cdir).expect("bearded_dragon manifest.toml missing or invalid");
    let pt_mf = manifest::load(&pt_cdir).expect("pond_turtle manifest.toml missing or invalid");
    let bd_config = make_shared(&bd_cdir);
    let pt_config = make_shared(&pt_cdir);
    let user_cfg = crate::user_config::load();
    let sprite_size = user_cfg.display.sprite_size as f64;
    let bd_display_w = sprite_size;
    let pt_display_w = sprite_size;
    let bd_assets = Rc::new(SpriteAssets::load(&bd_cdir, &bd_mf, bd_display_w).expect("failed to load bearded_dragon sprites"));
    let pt_assets = Rc::new(SpriteAssets::load(&pt_cdir, &pt_mf, pt_display_w).expect("failed to load pond_turtle sprites"));

    let si = wm::screen_info(mt)
        .unwrap_or(ScreenInfo { width: 1280.0, height: 800.0, dock_height: 0.0, menu_bar_height: 24.0 });

    let demo_mode = std::env::args().any(|a| a == "--demo");
    let initial_chars: Vec<CharState> = if demo_mode {
        // Demo mode: one bearded dragon with deterministic scripted behavior.
        vec![spawn_char(Rc::clone(&bd_assets), bd_config.clone(), &si, mt, true, "bearded_dragon")]
    } else {
        vec![
            spawn_char(Rc::clone(&bd_assets), bd_config.clone(), &si, mt, false, "bearded_dragon"),
            spawn_char(Rc::clone(&pt_assets), pt_config.clone(), &si, mt, false, "pond_turtle"),
        ]
    };

    let weather_handle = crate::weather::spawn(&user_cfg.weather);

    let menu_handler = MenuDelegate::new(mt);
    let lang = user_cfg.display.language.clone()
        .unwrap_or_else(detect_system_language);
    let status_item = make_status_item(&menu_handler, mt, &lang);

    // Register ⌘+drag event monitors.
    let event_monitors = setup_drag_monitors();

    // 10 Hz timer
    let blk = RcBlock::new(|_: NonNull<NSTimer>| tick());
    let timer =
        unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(0.1, true, &blk) };
    unsafe {
        let common: &NSRunLoopMode = &*(NSRunLoopCommonModes as *const NSRunLoopMode);
        NSRunLoop::mainRunLoop().addTimer_forMode(&timer, common);
    }

    APP.with(|cell| {
        *cell.borrow_mut() = Some(AppState {
            chars: initial_chars,
            bd_assets,
            pt_assets,
            bd_config,
            pt_config,
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
            lang: lang,
            weather: weather_handle,
        });
    });

    unsafe { app.run() };
}
