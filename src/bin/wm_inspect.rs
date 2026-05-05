//! Diagnostic binary: enumerate and classify all on-screen CGWindows.
//!
//! Build:  cargo build --bin wm_inspect
//! Run:    ./target/debug/wm_inspect
//!
//! This tool helps determine which macOS window types Petit Mates can
//! sit on, and which should be treated as the desktop floor or ignored.
//!
//! Screen Recording permission is required for kCGWindowName to be
//! populated (macOS 10.15+).
//! Grant: System Settings > Privacy & Security > Screen Recording

fn main() {
    #[cfg(target_os = "macos")]
    macos::run();

    #[cfg(not(target_os = "macos"))]
    eprintln!("wm_inspect requires macOS.");
}

// ─────────────────────────────────────────────────────────────────────────────
// macOS implementation
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(target_os = "macos")]
mod macos {
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSString};
    use std::collections::HashMap;
    use std::ffi::CStr;

    // ── CoreGraphics FFI ──────────────────────────────────────────────────────

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGWindowListCopyWindowInfo(option: u32, relative_to: u32) -> *mut AnyObject;
    }

    /// kCGWindowListOptionOnScreenOnly
    const OPT_ON_SCREEN: u32 = 1 << 0;
    /// kCGWindowListExcludeDesktopElements
    const OPT_EXCL_DESKTOP: u32 = 1 << 4;
    /// kCGNullWindowID
    const NULL_WIN: u32 = 0;

    // ── Window category ───────────────────────────────────────────────────────

    /// Classification of a CGWindow entry for Petit Mates surface detection.
    #[derive(Debug, Clone, PartialEq, Eq, Hash)]
    enum Cat {
        /// Desktop wallpaper/background (very negative layer, owner=Dock).
        /// Petit Mates uses the screen bottom as the desktop floor — not this window.
        DesktopBg,
        /// Finder desktop icon overlay (very negative layer, owner=Finder).
        DesktopIcons,
        /// Finder folder-view window (layer=0, owner=Finder).
        /// Petit Mates CAN sit on this.
        FinderWin,
        /// Dock bar (layer≈20, owner=Dock).
        Dock,
        /// Dock-owned window at an unusual layer that may be Stage Manager UI.
        StageManager,
        /// Stage Manager UI owned by WindowManager process (macOS 13+, layer=0).
        /// list_windows() excludes these.  NOT a Petit Mates surface.
        WinMgr,
        /// System menu bar (layer≈24).
        MenuBar,
        /// Status-bar items (layer≈25–99).
        StatusItem,
        /// Regular application window (layer=0, not Finder, not WindowManager).
        /// Petit Mates CAN sit on this (subject to size / fullscreen filters).
        NormalWin,
        /// Floating / utility panel (layer 1–19).
        FloatPanel,
        /// System overlay: notification center, screen saver, etc. (layer≥100).
        Overlay,
        /// Unclassified entry.
        Unknown,
    }

    impl Cat {
        fn label(&self) -> &'static str {
            match self {
                Cat::DesktopBg    => "DESKTOP_BG   ",
                Cat::DesktopIcons => "DESKTOP_ICONS",
                Cat::FinderWin    => "FINDER_WIN   ",
                Cat::Dock         => "DOCK         ",
                Cat::StageManager => "STAGE_MGR?   ",
                Cat::WinMgr       => "WIN_MGR      ",
                Cat::MenuBar      => "MENU_BAR     ",
                Cat::StatusItem   => "STATUS_ITEM  ",
                Cat::NormalWin    => "NORMAL_WIN   ",
                Cat::FloatPanel   => "FLOAT_PANEL  ",
                Cat::Overlay      => "OVERLAY      ",
                Cat::Unknown      => "UNKNOWN      ",
            }
        }

        /// True for window types that Petit Mates *might* sit on (pre-size-check).
        fn is_surface_candidate(&self) -> bool {
            matches!(self, Cat::FinderWin | Cat::NormalWin)
        }
    }

    /// Classify a window by its layer and owner process name.
    ///
    /// Layer reference (CGWindowLevel.h, macOS 14):
    ///   kCGDesktopWindowLevel     ≈ i32::MIN + 21  (≈ −2 147 483 627)
    ///   kCGDesktopIconWindowLevel ≈ i32::MIN + 41  (≈ −2 147 483 607)
    ///   kCGNormalWindowLevel      = 0
    ///   kCGDockWindowLevel        = 20
    ///   kCGMainMenuWindowLevel    = 24
    ///   kCGStatusWindowLevel      = 25
    fn classify(layer: i32, owner: &str) -> Cat {
        if layer < -20 {
            return match owner {
                "Finder" => Cat::DesktopIcons,
                _        => Cat::DesktopBg,
            };
        }
        match layer {
            i32::MIN..=-21 => Cat::DesktopBg,
            -20..=-1       => Cat::Unknown,
            0 => match owner {
                "Finder"        => Cat::FinderWin,
                "WindowManager" => Cat::WinMgr,   // Stage Manager UI (macOS 13+)
                _               => Cat::NormalWin,
            },
            1..=19 => Cat::FloatPanel,
            20 => {
                if owner == "Dock" { Cat::Dock } else { Cat::FloatPanel }
            }
            21..=23 => {
                if owner == "Dock" { Cat::StageManager } else { Cat::FloatPanel }
            }
            24      => Cat::MenuBar,
            25..=99 => Cat::StatusItem,
            _       => Cat::Overlay,
        }
    }

    // ── Window record ─────────────────────────────────────────────────────────

    struct Win {
        id:    u32,
        layer: i32,
        pid:   i32,
        owner: String,
        name:  String,
        alpha: f64,
        x: f64, y: f64, w: f64, h: f64,
        cat:   Cat,
    }

    impl Win {
        /// Reason this window is filtered by list_windows(), if any.
        /// Empty string means it passes all filters and is a surface candidate.
        fn filter_reason(&self, si_w: f64, usable_h: f64) -> &'static str {
            if self.cat == Cat::WinMgr {
                return "owner=WindowManager";
            }
            if !self.cat.is_surface_candidate() {
                return ""; // non-surface by category, not a list_windows() candidate anyway
            }
            // list_windows() only considers layer=0; non-zero already excluded by category
            if self.w < 300.0 || self.h < 150.0 {
                return "too small (<300×150)";
            }
            if self.w >= si_w * 0.90 && self.h >= usable_h * 0.90 {
                return "fullscreen/maximized (≥90% of screen)";
            }
            ""
        }
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn dict_i32(d: &NSDictionary<NSString, AnyObject>, key: &str) -> Option<i32> {
        let k = NSString::from_str(key);
        let v: Retained<AnyObject> = d.objectForKey(&k)?;
        let n: &NSNumber = v.downcast_ref()?;
        Some(n.intValue())
    }

    fn dict_f64(d: &NSDictionary<NSString, AnyObject>, key: &str) -> Option<f64> {
        let k = NSString::from_str(key);
        let v: Retained<AnyObject> = d.objectForKey(&k)?;
        let n: &NSNumber = v.downcast_ref()?;
        Some(n.doubleValue())
    }

    /// Read a string value via the Objective-C `UTF8String` method.
    /// Works for NSString and any class that implements that selector.
    unsafe fn ns_str(obj: &AnyObject) -> String {
        let ptr: *const i8 = objc2::msg_send![obj, UTF8String];
        if ptr.is_null() {
            return String::new();
        }
        unsafe { CStr::from_ptr(ptr) }.to_string_lossy().into_owned()
    }

    fn dict_str(d: &NSDictionary<NSString, AnyObject>, key: &str) -> String {
        let k = NSString::from_str(key);
        d.objectForKey(&k)
            .map(|v| unsafe { ns_str(&v) })
            .unwrap_or_default()
    }

    fn dict_bounds(d: &NSDictionary<NSString, AnyObject>) -> (f64, f64, f64, f64) {
        let k = NSString::from_str("kCGWindowBounds");
        let Some(bobj) = d.objectForKey(&k) else {
            return (0.0, 0.0, 0.0, 0.0);
        };
        let bd: &NSDictionary<NSString, AnyObject> =
            unsafe { &*(Retained::as_ptr(&bobj) as *const _) };
        (
            dict_f64(bd, "X").unwrap_or(0.0),
            dict_f64(bd, "Y").unwrap_or(0.0),
            dict_f64(bd, "Width").unwrap_or(0.0),
            dict_f64(bd, "Height").unwrap_or(0.0),
        )
    }

    fn trunc(s: &str, n: usize) -> String {
        let chars: Vec<char> = s.chars().collect();
        if chars.len() <= n {
            s.to_string()
        } else {
            chars[..n - 1].iter().collect::<String>() + "…"
        }
    }

    // ── Enumeration ───────────────────────────────────────────────────────────

    fn enumerate(opts: u32) -> Vec<Win> {
        let raw = unsafe { CGWindowListCopyWindowInfo(opts, NULL_WIN) };
        if raw.is_null() {
            return Vec::new();
        }
        let arr: Retained<NSArray<AnyObject>> =
            unsafe { Retained::from_raw(raw as *mut NSArray<AnyObject>).unwrap() };

        let n = arr.count();
        let mut result = Vec::with_capacity(n);

        for i in 0..n {
            let obj = arr.objectAtIndex(i);
            let dict: &NSDictionary<NSString, AnyObject> =
                unsafe { &*(Retained::as_ptr(&obj) as *const _) };

            let Some(id) = dict_i32(dict, "kCGWindowNumber").filter(|&v| v >= 0) else {
                continue;
            };
            let id = id as u32;
            let layer = dict_i32(dict, "kCGWindowLayer").unwrap_or(0);
            let pid   = dict_i32(dict, "kCGWindowOwnerPID").unwrap_or(-1);
            let alpha = dict_f64(dict, "kCGWindowAlpha").unwrap_or(1.0);
            let owner = dict_str(dict, "kCGWindowOwnerName");
            let name  = dict_str(dict, "kCGWindowName");
            let (x, y, w, h) = dict_bounds(dict);
            let cat = classify(layer, &owner);

            result.push(Win { id, layer, pid, owner, name, alpha, x, y, w, h, cat });
        }
        result
    }

    // ── Entry point ───────────────────────────────────────────────────────────

    pub fn run() {
        println!("╔══════════════════════════════════════════════════════════╗");
        println!("║       Petit Mates — Window Inspector (wm_inspect)        ║");
        println!("╚══════════════════════════════════════════════════════════╝");
        println!();

        // Screen geometry for fullscreen-detection thresholds.
        use objc2_app_kit::NSScreen;
        use objc2_foundation::MainThreadMarker;
        let (si_w, si_h, dock_h, menu_h) = unsafe {
            let mt = MainThreadMarker::new_unchecked();
            NSScreen::mainScreen(mt).map(|s| {
                let fr = s.frame();
                let vf = s.visibleFrame();
                let h = fr.size.height;
                let dock = vf.origin.y.max(0.0);
                let menu = (h - dock - vf.size.height).max(0.0);
                (fr.size.width, h, dock, menu)
            }).unwrap_or((1280.0, 800.0, 0.0, 24.0))
        };
        let usable_h = (si_h - dock_h - menu_h).max(1.0);

        println!("Screen : {:.0}×{:.0}  dock={:.0}  menubar={:.0}  usable_h={:.0}",
            si_w, si_h, dock_h, menu_h, usable_h);
        println!("Filters: owner≠WindowManager  |  width≥300  height≥150  |  NOT (w≥{:.0} AND h≥{:.0})",
            si_w * 0.90, usable_h * 0.90);
        println!();

        // Pass 1: all on-screen windows (includes desktop elements).
        let full = enumerate(OPT_ON_SCREEN);

        // Pass 2: ExclDesktop flag (same as list_windows()).
        let excl_desktop_ids: std::collections::HashSet<u32> =
            enumerate(OPT_ON_SCREEN | OPT_EXCL_DESKTOP)
                .iter()
                .map(|w| w.id)
                .collect();

        let hidden_excl = full.iter().filter(|w| !excl_desktop_ids.contains(&w.id)).count();

        // Count windows that survive all list_windows() filters.
        let surface_count = full.iter().filter(|w| {
            excl_desktop_ids.contains(&w.id)
                && w.cat.is_surface_candidate()
                && w.filter_reason(si_w, usable_h).is_empty()
        }).count();

        println!("On-screen total            : {}", full.len());
        println!("After ExclDesktop flag     : {} ({} removed)", excl_desktop_ids.len(), hidden_excl);
        println!("After all list_windows()   : {} surface candidates", surface_count);
        println!();
        println!("  * = excluded by ExclDesktop flag");
        println!("  ✗ = excluded by list_windows() filter (reason shown after size)");
        println!("  ◀ = Petit Mates surface candidate");
        println!();

        // ── Table ─────────────────────────────────────────────────────────────
        let col = (8, 7, 6, 5, 22, 28, 13);
        println!(
            " {:<w0$} {:>w1$} {:>w2$} {:>w3$}  {:<w4$} {:<w5$} {:<w6$}  x,y  w×h  [filter]",
            "ID", "LAYER", "PID", "α", "OWNER", "NAME", "CATEGORY",
            w0=col.0, w1=col.1, w2=col.2, w3=col.3, w4=col.4, w5=col.5, w6=col.6,
        );
        println!("{}", "─".repeat(130));

        for w in &full {
            let excl_flag  = if !excl_desktop_ids.contains(&w.id) { "*" } else { " " };
            let reason     = w.filter_reason(si_w, usable_h);
            let surf_flag  = if excl_desktop_ids.contains(&w.id)
                && w.cat.is_surface_candidate()
                && reason.is_empty() { " ◀" } else { "  " };
            let filter_str = if !excl_desktop_ids.contains(&w.id) {
                "ExclDesktop".to_string()
            } else if !reason.is_empty() {
                format!("✗ {reason}")
            } else {
                String::new()
            };
            let name = if w.name.is_empty() { "(needs Screen Recording perm)" } else { &w.name };

            println!(
                "{}{:<w0$} {:>w1$} {:>w2$} {:>w3$.2}  {:<w4$} {:<w5$} {}{}  {:.0},{:.0}  {:.0}×{:.0}  {}",
                excl_flag,
                w.id, w.layer, w.pid, w.alpha,
                trunc(&w.owner, col.4), trunc(name, col.5),
                w.cat.label(), surf_flag,
                w.x, w.y, w.w, w.h, filter_str,
                w0=col.0, w1=col.1, w2=col.2, w3=col.3, w4=col.4, w5=col.5,
            );
        }

        // ── Summary ───────────────────────────────────────────────────────────
        println!();
        println!("{}", "─".repeat(130));
        println!("SUMMARY (all on-screen windows)");
        println!();

        let mut counts: HashMap<&'static str, usize> = HashMap::new();
        for w in &full {
            *counts.entry(w.cat.label()).or_insert(0) += 1;
        }
        let mut sorted: Vec<_> = counts.iter().collect();
        sorted.sort_by_key(|&(label, _)| *label);

        for (label, count) in &sorted {
            let note = match **label {
                "FINDER_WIN   " | "NORMAL_WIN   " =>
                    "  ◀ candidate (subject to size/fullscreen filter)",
                "WIN_MGR      " => "  ✗ always excluded (Stage Manager UI)",
                _ => "",
            };
            println!("  {} : {:>3}{}", label, count, note);
        }

        // ── Notes ─────────────────────────────────────────────────────────────
        println!();
        println!("{}", "─".repeat(130));
        println!("NOTES");
        println!();
        println!("  DESKTOP_BG    Wallpaper / WindowServer at very negative layer.");
        println!("                Not a surface — Petit Mates uses NSScreen.visibleFrame");
        println!("                bottom edge as the desktop floor.");
        println!();
        println!("  DESKTOP_ICONS Finder desktop icon shelf at slightly higher negative layer.");
        println!("                Excluded by kCGWindowListExcludeDesktopElements.");
        println!();
        println!("  FINDER_WIN    Finder folder-view window (layer=0, owner=Finder).");
        println!("                Petit Mates CAN sit on these (if ≥300×150 and not fullscreen).");
        println!();
        println!("  WIN_MGR       WindowManager-owned window at layer=0 (macOS 13+).");
        println!("                These are Stage Manager chrome: Recent Apps strip,");
        println!("                app-group containers.  ALWAYS excluded by list_windows().");
        println!();
        println!("  DOCK          Dock bar (layer≈20). Not a surface.");
        println!("                dock_height comes from NSScreen.visibleFrame.origin.y.");
        println!();
        println!("  STAGE_MGR?    Dock-owned windows at layer 21–23 (heuristic).");
        println!("                Stage Manager Recent Apps / group frames at non-zero layers.");
        println!("                Excluded by the layer≠0 filter in list_windows().");
        println!();
        println!("  NORMAL_WIN    Regular app window (layer=0, owner≠Finder, ≠WindowManager).");
        println!("                Primary Petit Mates surface, subject to size and");
        println!("                fullscreen filters.");
        println!();
        println!("  FLOAT_PANEL   Floating/utility panels (layer 1–19). Not currently used.");
        println!();
        println!("  Size filter    : w < 300 OR h < 150 → excluded.");
        println!("  Fullscreen fltr: w ≥ {:.0} AND h ≥ {:.0} (90% of screen/usable) → excluded.",
            si_w * 0.90, usable_h * 0.90);
        println!();
        println!("  kCGWindowName is empty without Screen Recording permission.");
        println!("  Grant: System Settings > Privacy & Security > Screen Recording");
    }
}
