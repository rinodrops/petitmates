#![cfg(target_os = "macos")]
#![allow(non_snake_case, unused_unsafe, deprecated)]

use std::cell::RefCell;
use std::path::PathBuf;
use std::ptr::NonNull;
use std::time::Instant;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSColor, NSEvent,
    NSEventMask, NSEventModifierFlags, NSImage, NSMenu, NSMenuItem, NSPanel,
    NSRunningApplication, NSStatusBar, NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_foundation::{
    MainThreadMarker, NSBundle, NSPoint, NSRect, NSRunLoop, NSRunLoopMode, NSString, NSTimer,
};

use crate::assets::{make_image_view, Anchor, SpriteAssets};
use crate::behavior::{BehaviorContext, BehaviorScript, Dir, Side, State, Surface, Transition};
use crate::config::{make_shared, Config, SharedConfig};
use crate::manifest;
use crate::rust_behavior::RustBehavior;
use crate::sprite_map::{sprite_for_state, sprite_for_turn};
use crate::wm::{self, ScreenInfo, WinInfo};

// ---- FFI ----

#[link(name = "Foundation", kind = "framework")]
unsafe extern "C" {
    static NSRunLoopCommonModes: *const std::ffi::c_void;
}

// ---- App state ----

struct AppState {
    panel: Retained<NSPanel>,
    assets: SpriteAssets,
    config: SharedConfig,
    behavior: Box<dyn BehaviorScript>,
    /// Current animation/behavior state.
    anim_state: State,
    /// Which direction the character faces.
    facing: Dir,
    /// Which surface the character is on.
    surface: Surface,
    /// Character position in CG coordinates (origin = screen top-left, Y down).
    /// Updated each tick; converted to NS coords and applied to panel in Step 6e.
    char_pos: (f64, f64),
    last_tick: Instant,
    /// Mouse offset from char_pos when drag started (NS coords delta), None if not dragging.
    drag_offset: Option<(f64, f64)>,
    _status_item: Retained<objc2_app_kit::NSStatusItem>,
    _timer: Retained<NSTimer>,
    /// Keep event monitors alive for the lifetime of the app.
    _event_monitors: Vec<Retained<AnyObject>>,
}

thread_local! {
    static APP: RefCell<Option<AppState>> = RefCell::new(None);
}

// ---- Panel helpers ----

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
        panel.setLevel(3); // NSFloatingWindowLevel
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

// ---- Status item ----

fn make_status_item(mt: MainThreadMarker) -> Retained<objc2_app_kit::NSStatusItem> {
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

        let about = NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mt),
            &NSString::from_str("About Petit Mates"),
            Some(objc2::sel!(orderFrontStandardAboutPanel:)),
            &NSString::from_str(""),
        );
        menu.addItem(&about);
        menu.addItem(&NSMenuItem::separatorItem(mt));

        let quit = NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mt),
            &NSString::from_str("Quit"),
            Some(objc2::sel!(terminate:)),
            &NSString::from_str("q"),
        );
        menu.addItem(&quit);
        item.setMenu(Some(&menu));
        item
    }
}

// ---- Asset directory ----

pub fn char_dir() -> Option<PathBuf> {
    let bundle_path = unsafe {
        let bundle = NSBundle::mainBundle();
        bundle
            .resourceURL()
            .and_then(|base| {
                let r = NSString::from_str("assets/bearded_dragon");
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
        .join("../../assets/bearded_dragon")
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
        // Check whether the click landed inside the panel.
        let mouse_ns = unsafe { NSEvent::mouseLocation() };
        let hit = APP.with(|cell| {
            let b = cell.borrow();
            let Some(s) = b.as_ref() else { return false };
            let frame = unsafe { s.panel.frame() };
            mouse_ns.x >= frame.origin.x
                && mouse_ns.x < frame.origin.x + frame.size.width
                && mouse_ns.y >= frame.origin.y
                && mouse_ns.y < frame.origin.y + frame.size.height
        });
        if !hit { return; }

        // Compute offset from panel origin in NS coords.
        let offset = APP.with(|cell| {
            let b = cell.borrow();
            let Some(s) = b.as_ref() else { return (0.0, 0.0) };
            let frame = unsafe { s.panel.frame() };
            (mouse_ns.x - frame.origin.x, mouse_ns.y - frame.origin.y)
        });

        APP.with(|cell| {
            let mut b = cell.borrow_mut();
            let Some(s) = b.as_mut() else { return };
            s.drag_offset = Some(offset);
            s.anim_state = State::Grabbed;
            s.surface = Surface::Airborne;
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
            let Some(s) = b.as_mut() else { return };
            let Some(off) = s.drag_offset else { return };

            let mouse_ns = unsafe { NSEvent::mouseLocation() };
            // New NS panel origin.
            let new_ns_x = mouse_ns.x - off.0;
            let new_ns_y = mouse_ns.y - off.1;
            unsafe { s.panel.setFrameOrigin(NSPoint::new(new_ns_x, new_ns_y)) };

            // Keep char_pos in CG coords in sync (top-left of sprite).
            let sz = unsafe { s.panel.frame().size };
            let si_height = wm::screen_info_raw();
            s.char_pos = (new_ns_x, si_height - new_ns_y - sz.height);
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
            let Some(s) = b.as_mut() else { return };
            if s.drag_offset.is_none() { return; }
            s.drag_offset = None;

            let si = wm::screen_info_raw_full();
            let wins = wm::list_windows(&si);

            // Sprite dimensions for foot position.
            let sr = sprite_for_state(&s.anim_state, s.facing);
            let (fw, fh) = s.assets.image(sr.name, sr.mirror)
                .map(|img| { let sz = unsafe { img.size() }; (sz.width, sz.height) })
                .unwrap_or((150.0, 150.0));
            let foot_x = s.char_pos.0 + fw / 2.0;
            let foot_y = s.char_pos.1 + fh;

            // Try to snap to a nearby surface.
            let new_surface = wm::find_surface_near(foot_x, foot_y, &wins, &si);
            match new_surface {
                Some(surf) => {
                    let cfg = s.config.lock().unwrap().current.clone();
                    let ctx = BehaviorContext {
                        state: &s.anim_state,
                        surface: &surf,
                        elapsed_secs: 0.0,
                        config: &cfg,
                        rng01: 0.0,
                        surface_progress: 0.5,
                        facing: s.facing,
                        at_edge: false,
                        jump_target: None,
                    };
                    let new_anim = s.behavior.on_landed(&ctx);
                    // Snap char_pos foot to the surface.
                    let stand_anchor = s.assets.anchor("s-stand").unwrap_or(crate::assets::Anchor { x: 0.0, y: 0.0 });
                    let stand_h = s.assets.image("s-stand", false)
                        .map(|img| unsafe { img.size() }.height)
                        .unwrap_or(fh);
                    let snap_y = match &surf {
                        Surface::WindowTop { win_id, .. } =>
                            wm::find_win(*win_id, &wins).map(|w| w.y),
                        Surface::Desktop { .. } => {
                            let si = wm::screen_info_raw_full();
                            Some(si.floor_y())
                        }
                        _ => None,
                    };
                    if let Some(surface_y) = snap_y {
                        s.char_pos = (foot_x - fw / 2.0, surface_y - stand_h + stand_anchor.y);
                    }
                    s.surface = surf;
                    s.anim_state = new_anim;
                }
                None => {
                    // Drop from current position.
                    s.surface = Surface::Airborne;
                    s.anim_state = State::Falling { vx: 0.0, vy: 0.0 };
                }
            }
        });
    });
    if let Some(m) = unsafe {
        NSEvent::addGlobalMonitorForEventsMatchingMask_handler(mask_up, &*blk_up)
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

/// Compute `surface_progress` [0..1] and `at_edge` for the current surface.
/// `char_pos` is the CG position of the character.
/// `facing` is used on the Desktop surface to restrict jump targets to the
/// direction the character is currently walking, preventing false jumps.
fn surface_context(
    surface: &Surface,
    char_pos: (f64, f64),
    sprite_w: f64,
    facing: Dir,
    jump_max_dist: f64,
    wins: &[WinInfo],
    si: &ScreenInfo,
) -> (f64, bool, Option<(u32, Side)>) {
    let edge_margin = 2.0;
    match surface {
        Surface::WindowTop { win_id, x_local } => {
            let Some(win) = wm::find_win(*win_id, wins) else {
                return (0.5, false, None);
            };
            let progress = (x_local / win.w).clamp(0.0, 1.0);
            let at_edge = *x_local <= edge_margin + sprite_w / 2.0
                || *x_local >= win.w - edge_margin - sprite_w / 2.0;
            (progress, at_edge, None)
        }
        Surface::Desktop { x } => {
            let progress = (x / si.width).clamp(0.0, 1.0);
            // Find a wall-jump target only in the direction the character is
            // walking, and only for windows whose bottom is near the floor
            // (within 150 px above the Dock, per the design spec).
            // This prevents jumping toward windows behind the character or
            // toward windows floating high on the screen.
            let (cx, _) = char_pos;
            let floor_y = si.floor_y();
            let jump_target = wins.iter().find_map(|win| {
                // Window bottom must be within 150 px above the Dock.
                if win.bottom() < floor_y - 150.0 { return None; }
                match facing {
                    Dir::Left => {
                        // Jump left: attach to the RIGHT wall of a window to our left.
                        let dist = cx - win.right();
                        if dist >= 0.0 && dist < jump_max_dist {
                            Some((win.id, Side::Right))
                        } else {
                            None
                        }
                    }
                    Dir::Right => {
                        // Jump right: attach to the LEFT wall of a window to our right.
                        let dist = win.x - cx;
                        if dist >= 0.0 && dist < jump_max_dist {
                            Some((win.id, Side::Left))
                        } else {
                            None
                        }
                    }
                }
            });
            let at_edge = *x <= edge_margin + sprite_w / 2.0
                || *x >= si.width - edge_margin - sprite_w / 2.0;
            (progress, at_edge, jump_target)
        }
        Surface::WindowWall { win_id, y_local, .. } => {
            let Some(win) = wm::find_win(*win_id, wins) else {
                return (0.5, false, None);
            };
            let progress = (y_local / win.h).clamp(0.0, 1.0);
            let at_edge = *y_local <= edge_margin || *y_local >= win.h - edge_margin;
            (progress, at_edge, None)
        }
        _ => (0.5, false, None),
    }
}

/// Advance per-state animation timers and frame counters by `dt` seconds.
/// Returns the current `elapsed` value for `BehaviorContext::elapsed_secs`.
fn advance_anim(state: &mut State, dt: f64, cfg: &Config) -> f64 {
    match state {
        State::Falling { .. } | State::Grabbed => 0.0,

        State::LandingStandUp { elapsed }
        | State::Observing { elapsed, .. }
        | State::TurningAround { elapsed, .. }
        | State::PeekDown { elapsed, .. }
        | State::JumpRunup { elapsed, .. }
        | State::WallEntry { elapsed }
        | State::WallPause { elapsed, .. }
        | State::CornerTransitionSide { elapsed, .. }
        | State::CornerTransitionFront { elapsed, .. }
        | State::CornerRest { elapsed, .. }
        | State::SitIdle { elapsed, .. }
        | State::LieIdle { elapsed, .. }
        | State::Sleeping { elapsed, .. } => {
            *elapsed += dt;
            *elapsed
        }

        State::StandIdle { elapsed, bob_elapsed, bob_phase, .. } => {
            *elapsed += dt;
            *bob_elapsed += dt;
            let period = (cfg.floor.headbob_period[0] + cfg.floor.headbob_period[1]) / 2.0;
            while *bob_elapsed >= period {
                *bob_elapsed -= period;
                *bob_phase = !*bob_phase;
            }
            *elapsed
        }

        State::Walking { frame, frame_elapsed, .. } => {
            *frame_elapsed += dt;
            while *frame_elapsed >= cfg.floor.walk_frame_secs {
                *frame_elapsed -= cfg.floor.walk_frame_secs;
                *frame = (*frame + 1) % 4;
            }
            0.0
        }

        State::ClimbingUp { frame, frame_elapsed, wall_frames }
        | State::ClimbingDown { frame, frame_elapsed, wall_frames } => {
            *frame_elapsed += dt;
            while *frame_elapsed >= cfg.wall.climb_frame_secs {
                *frame_elapsed -= cfg.wall.climb_frame_secs;
                *frame = (*frame + 1) % 4;
                *wall_frames = wall_frames.saturating_add(1);
            }
            0.0
        }
    }
}

fn tick() {
    APP.with(|cell| {
        let mut b = cell.borrow_mut();
        let Some(s) = b.as_mut() else { return };
        let mt = unsafe { MainThreadMarker::new_unchecked() };

        // While dragging, skip physics and state machine — panel position is
        // updated directly by the drag event monitor.
        if s.drag_offset.is_some() {
            update_hover_alpha(&s.panel, &s.config.lock().unwrap().current.clone(), true);
            return;
        }

        // Compute dt, capped to avoid large jumps after pauses.
        let now = Instant::now();
        let dt = now.duration_since(s.last_tick).as_secs_f64().min(0.1);
        s.last_tick = now;

        // Hot-reload config.
        s.config.lock().unwrap().reload_if_changed();
        let cfg = s.config.lock().unwrap().current.clone();

        // Surface validity check.
        let si = wm::screen_info(mt).unwrap_or(ScreenInfo { width: 1280.0, height: 800.0, dock_height: 0.0, menu_bar_height: 24.0 });
        let wins = wm::list_windows(&si);
        if !wm::surface_still_valid(&s.surface, &wins) {
            let fallback = {
                let ctx = BehaviorContext {
                    state: &s.anim_state,
                    surface: &s.surface,
                    elapsed_secs: 0.0,
                    config: &cfg,
                    rng01: 0.0,
                    surface_progress: 0.5,
                    facing: s.facing,
                    at_edge: false,
                    jump_target: None,
                };
                s.behavior.on_surface_lost(&ctx)
            };
            s.anim_state = fallback;
            s.surface = Surface::Airborne;
        }

        // Advance per-state animation timers / frame counters.
        let elapsed = advance_anim(&mut s.anim_state, dt, &cfg);

        // Save CG y before position update for swept landing detection.
        let prev_cy = s.char_pos.1;

        // Update char_pos for Airborne / Walking states.
        match &s.anim_state {
            State::Falling { vx, vy } => {
                let (vx, vy) = (*vx, *vy);
                let (cx, cy) = s.char_pos;
                s.char_pos = (cx + vx * dt, cy + vy * dt);
            }
            State::Walking { dir, .. } => {
                // Advance local position within the surface.
                let speed = cfg.floor.walk_speed;
                let delta = speed * dt;
                match &mut s.surface {
                    Surface::Desktop { x } => {
                        *x += match dir { Dir::Left => -delta, Dir::Right => delta };
                        // Clamp so the character never walks off the visible screen area.
                        let half_w = cfg.display.display_width / 2.0;
                        *x = x.clamp(half_w, si.width - half_w);
                        s.char_pos.0 = *x;
                    }
                    Surface::WindowTop { x_local, .. } => {
                        *x_local += match dir { Dir::Left => -delta, Dir::Right => delta };
                    }
                    _ => {}
                }
            }
            State::ClimbingUp { .. } => {
                if let Surface::WindowWall { y_local, .. } = &mut s.surface {
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
        if matches!(&s.anim_state, State::ClimbingDown { .. }) {
            if let Surface::WindowWall { win_id, y_local, .. } = &mut s.surface {
                if let Some(win) = wm::find_win(*win_id, &wins) {
                    *y_local += cfg.wall.climb_speed * dt;
                    *y_local = y_local.min(win.h);
                }
            }
        }

        // Gravity for Falling state (cap vy to prevent tunneling).
        if let State::Falling { vy, .. } = &mut s.anim_state {
            *vy = (*vy + cfg.jump.gravity * 60.0 * dt).min(600.0);
        }

        // Off-screen safeguard: if the character has fallen more than one
        //     sprite-height below the screen bottom (or drifted far off the
        //     sides), reset to a fresh startup drop so the run loop can
        //     continue.
        {
            let (fw, fh) = s.assets.image("s-stand", false)
                .map(|img| { let sz = unsafe { img.size() }; (sz.width, sz.height) })
                .unwrap_or((150.0, 150.0));
            let (cx, cy) = s.char_pos;
            let below_screen = cy > si.height + fh;
            let off_sides    = cx < -(fw * 3.0) || cx > si.width + fw * 3.0;
            if below_screen || off_sides {
                let (drop_x, drop_y) = startup_drop(&si, &s.assets);
                s.char_pos  = (drop_x, drop_y);
                s.surface   = Surface::Airborne;
                s.anim_state = State::Falling { vx: 0.0, vy: 0.0 };
            }
        }

        // Landing detection (swept check: did foot cross a surface boundary?).
        if let State::Falling { vy, .. } = &s.anim_state {
            if *vy >= 0.0 {
                let (fw, fh) = s.assets.image("s-jump", false)
                    .or_else(|| s.assets.image("s-stand", false))
                    .map(|img| { let sz = unsafe { img.size() }; (sz.width, sz.height) })
                    .unwrap_or((150.0, 150.0));
                let foot_x = s.char_pos.0 + fw / 2.0;
                let _foot_y_prev = prev_cy + fh;
                let foot_y_now  = s.char_pos.1 + fh;

                let floor_y = si.floor_y();

                // Check window tops first.
                // Use a full-body swept range: from the character's top edge at the
                // previous tick (cy_prev) to the bottom edge at the current tick
                // (cy_now + fh).  Any window top that falls within this range is a
                // landing candidate.  We pick the topmost one (smallest win.y) so
                // the character lands on the highest eligible window and doesn't
                // tunnel through a window it only partially crossed.
                let cy_prev = prev_cy;
                let cy_now  = s.char_pos.1;
                let landed_win = wins.iter()
                    .filter(|win| {
                        win.y < floor_y
                            && foot_x >= win.x
                            && foot_x <= win.right()
                            && cy_prev < win.y
                            && cy_now + fh >= win.y
                    })
                    .min_by(|a, b| a.y.partial_cmp(&b.y).unwrap_or(std::cmp::Ordering::Equal));
                // Desktop floor: use a clamp check (foot at or past floor_y).
                let landed_floor = landed_win.is_none()
                    && foot_y_now >= floor_y;

                let new_surface = landed_win
                    .map(|win| Surface::WindowTop {
                        win_id: win.id,
                        x_local: (foot_x - win.x).clamp(0.0, win.w),
                    })
                    .or_else(|| landed_floor.then(|| {
                    // Clamp landing x so the character stays within the visible screen.
                    let half_w = cfg.display.display_width / 2.0;
                    let clamped_x = foot_x.clamp(half_w, si.width - half_w);
                    Surface::Desktop { x: clamped_x }
                }));

                if let Some(new_surface) = new_surface {
                    let new_anim = {
                        let ctx = BehaviorContext {
                            state: &s.anim_state,
                            surface: &new_surface,
                            elapsed_secs: 0.0,
                            config: &cfg,
                            rng01: 0.0,
                            surface_progress: 0.5,
                            facing: s.facing,
                            at_edge: false,
                            jump_target: None,
                        };
                        s.behavior.on_landed(&ctx)
                    };
                    // Snap char_pos so the foot sits exactly on the surface.
                    let stand_anchor = s.assets.anchor("s-stand").unwrap_or(Anchor { x: 0.0, y: 0.0 });
                    let stand_h = s.assets.image("s-stand", false)
                        .map(|img| unsafe { img.size() }.height)
                        .unwrap_or(fh);
                    let snap_y = match &new_surface {
                        Surface::WindowTop { win_id, .. } =>
                            wm::find_win(*win_id, &wins).map(|w| w.y),
                        Surface::Desktop { .. } => Some(floor_y),
                        _ => None,
                    };
                    if let Some(surface_y) = snap_y {
                        s.char_pos = (
                            foot_x - fw / 2.0,
                            surface_y - stand_h + stand_anchor.y,
                        );
                    }
                    s.surface = new_surface;
                    s.anim_state = new_anim;
                }
            }
        }

        // Compute surface_progress, at_edge, jump_target.
        let sr_for_ctx = match &s.anim_state {
            State::TurningAround { elapsed, .. } => {
                let progress = (*elapsed / cfg.floor.turn_duration).clamp(0.0, 1.0);
                sprite_for_turn(progress, s.facing)
            }
            other => sprite_for_state(other, s.facing),
        };
        let sprite_sz = s.assets.image(sr_for_ctx.name, sr_for_ctx.mirror)
            .map(|img| { let sz = unsafe { img.size() }; (sz.width, sz.height) })
            .unwrap_or((150.0, 150.0));
        let (surface_progress, at_edge, jump_target) =
            surface_context(&s.surface, s.char_pos, sprite_sz.0, s.facing,
                            cfg.jump.wall_jump_max_dist, &wins, &si);

        // Save to_dir if a TurningAround completes this tick.
        let turn_to_dir = if let State::TurningAround { to_dir, .. } = &s.anim_state {
            Some(*to_dir)
        } else {
            None
        };

        // Run behavior state machine.
        let transition = {
            let ctx = BehaviorContext {
                state: &s.anim_state,
                surface: &s.surface,
                elapsed_secs: elapsed,
                config: &cfg,
                rng01: 0.0,
                surface_progress,
                facing: s.facing,
                at_edge,
                jump_target,
            };
            s.behavior.next_state(&ctx)
        };
        match transition {
            Transition::Stay => {}
            Transition::To(new_state) => {
                // Complete a turn: update facing before entering the new state.
                if let Some(dir) = turn_to_dir {
                    s.facing = dir;
                }
                // When transitioning to Falling from a wall surface, compute
                // char_pos (CG top-left of sprite) from the wall position now,
                // before the surface is overwritten with Airborne.
                if matches!(&new_state, State::Falling { .. }) {
                    let fall_pos: Option<(f64, f64)> = (|| {
                        let (sw, sh) = s.assets.image("s-jump", false)
                            .or_else(|| s.assets.image("s-stand", false))
                            .map(|img| { let sz = unsafe { img.size() }; (sz.width, sz.height) })
                            .map(|sz| (sz.0, sz.1))
                            .or(Some((150.0, 150.0)))?;
                        match &s.surface {
                            Surface::WindowWall { win_id, side, y_local } => {
                                let win = wm::find_win(*win_id, &wins)?;
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
                        s.char_pos = pos;
                    }
                }
                // Keep s.surface in sync when the new state implies a surface change.
                let new_surface: Option<Surface> = match (&new_state, &s.surface) {
                    // Walking off window-top edge → start corner descent → upper corner
                    (State::CornerTransitionSide { side, .. }, Surface::WindowTop { win_id, .. }) => {
                        Some(Surface::WindowUpperCorner { win_id: *win_id, side: *side })
                    }
                    // ClimbingUp reached the wall top → upper corner
                    (State::CornerTransitionSide { side, .. }, Surface::WindowWall { win_id, .. }) => {
                        Some(Surface::WindowUpperCorner { win_id: *win_id, side: *side })
                    }
                    // Upper corner → descend the wall → attach just below the top-edge
                    // threshold (edge_margin=2) so ClimbingDown doesn't immediately
                    // trigger an "at_edge" lower-corner transition.
                    // y_local = sh/2 so the sprite top aligns with the window top edge
                    // (the current formula centers the sprite on the grip row).
                    (State::ClimbingDown { .. }, Surface::WindowUpperCorner { win_id, side }) => {
                        let y_local = s.assets.image("s-hang-wall-0", false)
                            .map(|img| unsafe { img.size() }.height / 2.0)
                            .unwrap_or(4.0);
                        Some(Surface::WindowWall { win_id: *win_id, side: *side, y_local })
                    }
                    // Upper corner → step onto window top.
                    // x_local=0 or x_local=win.w both satisfy the at_edge condition
                    // (edge_margin + sprite_w/2) immediately, causing an instant warp
                    // back to the opposite corner on the very next tick.
                    // Offset by walk-sprite half-width + edge_margin + 1 to land just
                    // inside the threshold so the character actually walks across.
                    (State::Walking { .. }, Surface::WindowUpperCorner { win_id, side }) => {
                        let walk_w = s.assets.image("s-walk-0", false)
                            .map(|img| unsafe { img.size() }.width)
                            .unwrap_or(sprite_sz.0);
                        let x_offset = walk_w / 2.0 + 3.0; // > edge_margin(2) + sprite_w/2
                        let x = match side {
                            Side::Left  => x_offset,
                            Side::Right => wm::find_win(*win_id, &wins)
                                .map(|w| w.w - x_offset)
                                .unwrap_or(400.0 - x_offset),
                        };
                        Some(Surface::WindowTop { win_id: *win_id, x_local: x })
                    }
                    // Jump runup complete → snap directly to the target wall.
                    // The target window and side are stored in the JumpRunup state;
                    // we read them here before new_state overwrites anim_state.
                    (State::WallEntry { .. }, Surface::Desktop { .. })
                    | (State::WallEntry { .. }, Surface::WindowTop { .. }) => {
                        // Only snap when coming from a JumpRunup (previous state).
                        if let State::JumpRunup { target_win_id, target_side, .. } = &s.anim_state {
                            if let Some(win) = wm::find_win(*target_win_id, &wins) {
                                let side = *target_side;
                                // Attach near the bottom of the wall (jumping from the floor),
                                // just high enough that ClimbingUp doesn't immediately see at_edge.
                                let hang_h = s.assets.image("s-hang-wall-0", false)
                                    .map(|img| unsafe { img.size() }.height)
                                    .unwrap_or(150.0);
                                let y_local = (win.h - hang_h / 2.0).clamp(hang_h / 2.0, win.h - 4.0);
                                // Update char_pos to the wall position.
                                let stand_w = s.assets.image("s-stand", false)
                                    .map(|img| unsafe { img.size() }.width)
                                    .unwrap_or(150.0);
                                s.char_pos.0 = match side {
                                    Side::Right => win.right() - stand_w,
                                    Side::Left  => win.x,
                                };
                                s.char_pos.1 = win.y + y_local;
                                s.facing = match side {
                                    Side::Left  => Dir::Right,
                                    Side::Right => Dir::Left,
                                };
                                Some(Surface::WindowWall { win_id: win.id, side, y_local })
                            } else { None }
                        } else { None }
                    }
                    _ => None,
                };
                if let Some(ns) = new_surface {
                    s.surface = ns;
                }
                // Sync facing when entering wall / corner-transition states so that
                // wall_side_from_surface() returns the correct side and corner sprites
                // are rendered facing inward (left corner → right, right corner → left).
                if matches!(&new_state, State::ClimbingUp { .. } | State::ClimbingDown { .. }) {
                    if let Surface::WindowWall { side, .. }
                         | Surface::WindowUpperCorner { side, .. } = &s.surface
                    {
                        s.facing = match side {
                            Side::Left  => Dir::Right,
                            Side::Right => Dir::Left,
                        };
                    }
                }
                // CornerTransitionSide: character has just rounded the corner;
                // face inward so the corner-hang sprite looks correct.
                if let State::CornerTransitionSide { side, .. } = &new_state {
                    s.facing = match side {
                        Side::Left  => Dir::Right,
                        Side::Right => Dir::Left,
                    };
                }
                s.anim_state = new_state;
            }
        }

        // Keep facing in sync with Walking direction.
        if let State::Walking { dir, .. } = &s.anim_state {
            s.facing = *dir;
        }

        // Select sprite and update panel position.
        let sr = match &s.anim_state {
            State::TurningAround { elapsed, .. } => {
                let progress = (*elapsed / cfg.floor.turn_duration).clamp(0.0, 1.0);
                sprite_for_turn(progress, s.facing)
            }
            other => sprite_for_state(other, s.facing),
        };
        swap_sprite(&s.panel, &s.assets, sr.name, sr.mirror, mt);

        // Move panel to surface-derived NS origin.
        let anchor = s.assets.anchor(sr.name).unwrap_or(Anchor { x: 0.0, y: 0.0 });
        let stand_anchor_y = s.assets.anchor("s-stand").map(|a| a.y).unwrap_or(0.0);
        let sz = s.assets.image(sr.name, sr.mirror)
            .map(|img| { let sz = unsafe { img.size() }; (sz.width, sz.height) })
            .unwrap_or((150.0, 150.0));
        let origin = surface_to_ns_origin(&s.surface, s.char_pos, sz, anchor, stand_anchor_y, &wins, &si);
        unsafe { s.panel.setFrameOrigin(origin) };

        // Hover alpha.
        update_hover_alpha(&s.panel, &cfg, false);
    });
}

// ---- Entry point ----

pub fn run() {
    let mt = unsafe { MainThreadMarker::new_unchecked() };
    let app = NSApplication::sharedApplication(mt);
    unsafe { app.setActivationPolicy(NSApplicationActivationPolicy::Accessory) };

    // Single-instance guard
    if let Some(bid) = unsafe { NSBundle::mainBundle().bundleIdentifier() } {
        let others =
            unsafe { NSRunningApplication::runningApplicationsWithBundleIdentifier(&bid) };
        let my_pid = std::process::id() as i32;
        if unsafe { others.iter().any(|a| a.processIdentifier() != my_pid) } {
            unsafe { app.terminate(None) };
            return;
        }
    }

    let cdir = char_dir().expect("character directory not found");
    let mf = manifest::load(&cdir).expect("manifest.toml missing or invalid");
    let config = make_shared(&cdir);
    let display_w = config.lock().unwrap().current.display.display_width;
    let assets = SpriteAssets::load(&cdir, &mf, display_w).expect("failed to load sprites");

    // Startup drop position (random x within center 80% of screen).
    let si = wm::screen_info(mt)
        .unwrap_or(ScreenInfo { width: 1280.0, height: 800.0, dock_height: 0.0, menu_bar_height: 24.0 });
    let (start_cx, start_cy) = startup_drop(&si, &assets);

    let init_img = assets.image("s-stand", false).expect("s-stand.png missing");
    let panel = make_panel(init_img, mt);
    let sz = unsafe { init_img.size() };
    // Start off-screen above; tick() moves it each frame.
    unsafe {
        // Place the panel at screen top: NS y = si.height - start_cy - sprite_height.
        // With start_cy=0 this is si.height - sz.height (sprite occupies top of screen).
        panel.setFrameOrigin(NSPoint::new(start_cx, si.height - start_cy - sz.height));
        panel.orderFront(None);
    }

    let status_item = make_status_item(mt);

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
            panel,
            assets,
            config,
            behavior: Box::new(RustBehavior::new()),
            anim_state: State::Falling { vx: 0.0, vy: 0.0 },
            facing: Dir::Left,
            surface: Surface::Airborne,
            char_pos: (start_cx, start_cy),
            last_tick: Instant::now(),
            drag_offset: None,
            _status_item: status_item,
            _timer: timer,
            _event_monitors: event_monitors,
        });
    });

    unsafe { app.run() };
}
