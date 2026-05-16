//! Debug context-menu helpers (⌥⌘+right-click on macOS / Alt+Ctrl+right-click on Windows).
//!
//! Platform-independent logic:
//! - Human-readable names for Surface / State variants.
//! - Enumerate available forced-transition targets based on current surface.
//!
//! The list of trigger targets is intentionally hardcoded here in the UI layer,
//! not in `BehaviorScript`, so the trait stays minimal for Phase 2 (Lua).
//! Phase 2 can extend this via manifest declarations.

use crate::behavior::{Dir, Side, State, Surface};
use crate::config::Config;

/// Human-readable name for a `Surface` variant.
pub fn surface_name(surface: &Surface) -> &'static str {
    match surface {
        Surface::Desktop { .. }          => "Desktop",
        Surface::WindowTop { .. }        => "WindowTop",
        Surface::WindowWall { side, .. } => match side {
            Side::Left  => "WindowWall (Left)",
            Side::Right => "WindowWall (Right)",
        },
        Surface::WindowUpperCorner { side, .. } => match side {
            Side::Left  => "WindowUpperCorner (Left)",
            Side::Right => "WindowUpperCorner (Right)",
        },
        Surface::WindowBottom { .. }             => "WindowBottom",
        Surface::Airborne => "Airborne",
    }
}

/// Human-readable name for a `State` variant.
pub fn state_name(state: &State) -> &'static str {
    match state {
        State::Falling { .. }               => "Falling",
        State::Airborne { .. }              => "Airborne",
        State::LandingStandUp { .. }        => "LandingStandUp",
        State::Observing { .. }             => "Observing",
        State::Walking { .. }               => "Walking",
        State::Running { .. }               => "Running",
        State::TurningAround { .. }         => "TurningAround",
        State::StandIdle { .. }             => "StandIdle",
        State::SitIdle { .. }               => "SitIdle",
        State::LieIdle { .. }               => "LieIdle",
        State::Sleeping { .. }              => "Sleeping",
        State::SurfaceInteract { .. }           => "SurfaceInteract",
        State::JumpRunup { .. }             => "JumpRunup",
        State::ClimbingUp { .. }            => "ClimbingUp",
        State::ClimbingDown { .. }          => "ClimbingDown",
        State::WallPause { .. }             => "WallPause",
        State::WallEntry { .. }             => "WallEntry",
        State::CornerTransitionSide { .. }  => "CornerTransitionSide",
        State::CornerTransitionFront { .. } => "CornerTransitionFront",
        State::CornerRest { .. }            => "CornerRest",
        State::OneShot { .. }               => "OneShot",
        State::Grabbed                      => "Grabbed",
    }
}

/// Returns `(elapsed, duration)` for states that have a duration field,
/// so the menu can display "Xrem / Ytotal".
pub fn state_elapsed_duration(state: &State) -> Option<(f64, f64)> {
    match state {
        State::StandIdle { elapsed, duration, .. }
        | State::SitIdle  { elapsed, duration, .. }
        | State::LieIdle  { elapsed, duration, .. }
        | State::Sleeping { elapsed, duration, .. }
        | State::CornerRest { elapsed, duration, .. }
        | State::Observing  { elapsed, duration, .. }
        | State::WallPause  { elapsed, duration, .. }
        | State::SurfaceInteract { elapsed, duration, .. }
        | State::Running { elapsed, duration, .. } => Some((*elapsed, *duration)),
        _ => None,
    }
}

/// A state transition that can be triggered from the debug menu.
pub struct DebugTarget {
    /// Menu item label (includes config duration hint).
    pub label: String,
    /// Target state to enter when the item is chosen.
    pub state: State,
}

/// Formats a `[min, max]` duration range as e.g. `"3–8s"` or `"5s"` (when equal).
fn fmt_range(r: [f64; 2]) -> String {
    if (r[0] - r[1]).abs() < 0.01 {
        format!("{:.0}s", r[0])
    } else {
        format!("{:.0}–{:.0}s", r[0], r[1])
    }
}

/// Midpoint of a `[min, max]` range — used as the `duration` field for forced states.
#[inline]
fn mid(r: [f64; 2]) -> f64 { (r[0] + r[1]) / 2.0 }

/// Returns the list of states that can be forced for the given surface + current state.
///
/// The list is intentionally hardcoded here in the UI layer (not in `BehaviorScript`).
/// Only states that are valid on the given surface are offered.
/// Labels include the effective config duration range so the developer can verify
/// which values (default or config.toml override) are in effect.
pub fn trigger_targets(surface: &Surface, current: &State, facing: Dir, cfg: &Config) -> Vec<DebugTarget> {
    let mut v = Vec::new();

    match surface {
        Surface::Desktop { .. } | Surface::WindowTop { .. } | Surface::WindowBottom { .. } => {
            if !matches!(current, State::Walking { .. }) {
                v.push(DebugTarget {
                    label: "→ Walking".to_string(),
                    state: State::Walking { dir: facing, frame: 0, frame_elapsed: 0.0 },
                });
            }
            if !matches!(current, State::Running { .. }) {
                v.push(DebugTarget {
                    label: format!("→ Running ({})", fmt_range(cfg.floor.run_duration)),
                    state: State::Running {
                        dir: facing, frame: 0, frame_elapsed: 0.0,
                        elapsed: 0.0, duration: mid(cfg.floor.run_duration),
                    },
                });
            }
            if !matches!(current, State::StandIdle { .. }) {
                v.push(DebugTarget {
                    label: format!("→ StandIdle ({})", fmt_range(cfg.floor.stand_duration)),
                    state: State::StandIdle {
                        elapsed: 0.0, duration: mid(cfg.floor.stand_duration),
                        bob_elapsed: 0.0, bob_phase: false, bob_next: 8.0,
                    },
                });
            }
            if !matches!(current, State::SitIdle { .. }) {
                v.push(DebugTarget {
                    label: format!("→ SitIdle ({})", fmt_range(cfg.floor.sit_duration)),
                    state: State::SitIdle {
                        elapsed: 0.0, duration: mid(cfg.floor.sit_duration),
                        head_front: false, head_timer: mid(cfg.floor.head_side_duration),
                    },
                });
            }
            if !matches!(current, State::LieIdle { .. }) {
                v.push(DebugTarget {
                    label: format!("→ LieIdle ({})", fmt_range(cfg.floor.lie_duration)),
                    state: State::LieIdle {
                        elapsed: 0.0, duration: mid(cfg.floor.lie_duration),
                        head_front: false, head_timer: mid(cfg.floor.head_side_duration),
                    },
                });
            }
            if !matches!(current, State::Sleeping { .. }) {
                v.push(DebugTarget {
                    label: format!("→ Sleeping ({})", fmt_range(cfg.floor.sleep_duration)),
                    state: State::Sleeping {
                        elapsed: 0.0, duration: mid(cfg.floor.sleep_duration),
                        head_front: false, head_timer: mid(cfg.floor.head_side_duration),
                    },
                });
            }
            // SurfaceInteract (peek-down) only available on WindowTop.
            if matches!(surface, Surface::WindowTop { .. })
                && !matches!(current, State::SurfaceInteract { .. })
            {
                v.push(DebugTarget {
                    label: format!("→ SurfaceInteract/peek-down ({:.1}s)", cfg.floor.peek_duration),
                    state: State::SurfaceInteract {
                        animation: "peek-down".to_string(),
                        elapsed: 0.0,
                        duration: cfg.floor.peek_duration,
                        dir: facing,
                    },
                });
            }
        }

        Surface::WindowWall { .. } => {
            if !matches!(current, State::ClimbingUp { .. }) {
                v.push(DebugTarget {
                    label: "→ ClimbingUp".to_string(),
                    state: State::ClimbingUp { frame: 0, frame_elapsed: 0.0, wall_frames: 0 },
                });
            }
            if !matches!(current, State::ClimbingDown { .. }) {
                v.push(DebugTarget {
                    label: "→ ClimbingDown".to_string(),
                    state: State::ClimbingDown { frame: 2, frame_elapsed: 0.0, wall_frames: 0 },
                });
            }
            if !matches!(current, State::WallPause { .. }) {
                v.push(DebugTarget {
                    label: format!("→ WallPause ({})", fmt_range(cfg.wall.pause_duration)),
                    state: State::WallPause {
                        elapsed: 0.0, duration: mid(cfg.wall.pause_duration),
                        was_climbing_up: true,
                    },
                });
            }
        }

        Surface::WindowUpperCorner { .. } => {
            if !matches!(current, State::CornerRest { lying: false, .. }) {
                v.push(DebugTarget {
                    label: format!("→ CornerRest sit ({})", fmt_range(cfg.corner.rest_duration)),
                    state: State::CornerRest { elapsed: 0.0, duration: mid(cfg.corner.rest_duration), lying: false },
                });
            }
            if !matches!(current, State::CornerRest { lying: true, .. }) {
                v.push(DebugTarget {
                    label: format!("→ CornerRest lie ({})", fmt_range(cfg.corner.rest_duration)),
                    state: State::CornerRest { elapsed: 0.0, duration: mid(cfg.corner.rest_duration), lying: true },
                });
            }
        }

        // No triggers when airborne.
        Surface::Airborne => {}
    }

    v
}

/// Countdown duration before a forced transition takes effect.
pub const COUNTDOWN_SECS: f64 = 3.0;
