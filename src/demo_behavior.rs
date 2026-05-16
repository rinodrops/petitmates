//! Demo recording behavior — deterministic scripted cycle for GIF capture.
//!
//! Activated with the `--demo` CLI flag.  A single character is spawned with
//! this behavior and cycles through every animation in a fixed, short-duration
//! loop so that any state can be recorded within one cycle.
//!
//! ## Approximate cycle (≈ 90 s with default bearded-dragon settings)
//!
//! 1. Fall → land → observe (2 s) → walk toward corner
//! 2. Edge 0: SitIdle (4 s, head turn → f-sit) → LieIdle (5 s, head turn → f-lie) → walk
//! 3. Edge 1: CornerTransitionSide(down) → CornerRest lying (5 s) → descend wall
//! 4. Fall to desktop → walk → jump to window → ClimbingUp → CornerRest sitting (5 s) → walk inward
//! 5. Edge 2: SurfaceInteract/peek-down (2 s) → walk back
//! 6. Edge 3: CornerTransitionSide(down) → CornerRest (5 s) → jump to nearby window (or walk inward)
//! 7. Edge 4: shocked fall
//! 8. → back to step 1
//!
//! State transitions are printed to stderr for recording reference.

#![cfg(target_os = "macos")]

use std::sync::Mutex;

use crate::behavior::{BehaviorContext, BehaviorScript, Dir, LandingMode, Side, State, Surface, Transition};

// ── Helpers ────────────────────────────────────────────────────────────────

fn dir_to_side(dir: Dir) -> Side {
    match dir {
        Dir::Left  => Side::Left,
        Dir::Right => Side::Right,
    }
}

fn toward_corner(surface_progress: f64) -> Dir {
    if surface_progress < 0.5 { Dir::Left } else { Dir::Right }
}

// ── Counter constants ───────────────────────────────────────────────────────

/// Number of distinct window-top edge choices before the cycle repeats.
///   0 → SitIdle (→ LieIdle) to show idle + head-turn animations
///   1 → CornerTransitionSide(down) to show corner/descent path
///   2 → SurfaceInteract (peek-down) to show peek animation
///   3 → CornerTransitionSide(down) to show corner/jump path
///   4 → shocked fall
const EDGE_CYCLE: u8 = 5;

/// Number of distinct CornerRest exit choices before the cycle repeats.
///   0 → descend wall  (shows ClimbingDown + floor walk)
///   1 → walk inward   (shows window-top walking before next edge)
///   2 → jump to nearby window (shows window-to-window; falls back to walk inward if no target)
const CORNER_CYCLE: u8 = 3;

// ── DemoBehavior ────────────────────────────────────────────────────────────

/// Deterministic behavior driver for demo/recording mode.
pub struct DemoBehavior {
    /// Window-top edge decision counter (mod [`EDGE_CYCLE`]).
    edge_counter:   Mutex<u8>,
    /// CornerRest exit decision counter (mod [`CORNER_CYCLE`]).
    corner_counter: Mutex<u8>,
}

impl DemoBehavior {
    pub fn new() -> Self {
        eprintln!("[demo] ── Demo mode active ──────────────────────────────────");
        eprintln!("[demo] Cycle: sit/lie idle | corner+descend | peek | corner+jump | shocked fall");
        eprintln!("[demo] State changes are logged here for recording reference.");
        Self {
            edge_counter:   Mutex::new(0),
            corner_counter: Mutex::new(0),
        }
    }

    // ── Counter helpers ──────────────────────────────────────────────────────

    fn take_edge(&self) -> u8 {
        let mut c = self.edge_counter.lock().unwrap();
        let v = *c;
        *c = (v + 1) % EDGE_CYCLE;
        eprintln!("[demo] edge_choice={v}  (next={})", *c);
        v
    }

    fn take_corner(&self) -> u8 {
        let mut c = self.corner_counter.lock().unwrap();
        let v = *c;
        *c = (v + 1) % CORNER_CYCLE;
        eprintln!("[demo] corner_choice={v}  (next={})", *c);
        v
    }

    /// Peek at the current corner counter without advancing it.
    fn peek_corner(&self) -> u8 {
        *self.corner_counter.lock().unwrap()
    }

    // ── State constructors with fixed demo durations ─────────────────────────

    fn make_stand_idle() -> State {
        // 2 s standing idle — brief pause so it is visually clear before walking inward.
        let bob_next = 30.0_f64; // headbob won't fire in 2 s anyway
        State::StandIdle { elapsed: 0.0, duration: 2.0, bob_elapsed: 0.0, bob_phase: false, bob_next }
    }

    fn make_sit_idle() -> State {
        // 4 s total; head turns to front after 2 s, back after 2 more s.
        State::SitIdle { elapsed: 0.0, duration: 4.0, head_front: false, head_timer: 2.0 }
    }

    fn make_lie_idle() -> State {
        // 5 s total; head turns to front after 2.5 s, back after 2 more s.
        State::LieIdle { elapsed: 0.0, duration: 5.0, head_front: false, head_timer: 2.5 }
    }

    fn make_corner_rest(lying: bool) -> State {
        State::CornerRest { elapsed: 0.0, duration: 5.0, lying }
    }
}

// ── BehaviorScript implementation ───────────────────────────────────────────

impl BehaviorScript for DemoBehavior {
    fn next_state(&self, ctx: &BehaviorContext) -> Transition {
        let cfg = ctx.config;
        let e   = ctx.elapsed_secs;

        match ctx.state {

            // ── Airborne ─────────────────────────────────────────────
            State::Falling { .. } | State::Airborne { .. } => Transition::Stay,

            // ── Landing ──────────────────────────────────────────────
            State::LandingStandUp { .. } => {
                if e >= cfg.floor.standup_duration {
                    eprintln!("[demo] land → observe");
                    Transition::To(State::Observing { elapsed: 0.0, duration: 2.0 })
                } else {
                    Transition::Stay
                }
            }

            // ── Observation ───────────────────────────────────────────
            State::Observing { duration, .. } => {
                if e < *duration { return Transition::Stay; }
                // On desktop, walk toward a nearby window if one exists.
                if let Surface::Desktop { .. } = ctx.surface {
                    if let Some((_, side, _)) = ctx.attract_target {
                        let dir = match side { Side::Right => Dir::Left, Side::Left => Dir::Right };
                        eprintln!("[demo] observe → walk {:?} (attract window)", dir);
                        return Transition::To(State::Walking { dir, frame: 0, frame_elapsed: 0.0 });
                    }
                }
                let dir = toward_corner(ctx.surface_progress);
                eprintln!("[demo] observe → walk {:?}", dir);
                Transition::To(State::Walking { dir, frame: 0, frame_elapsed: 0.0 })
            }

            // ── SurfaceInteract ───────────────────────────────────────
            State::SurfaceInteract { dir, duration, .. } => {
                if e < *duration { return Transition::Stay; }
                eprintln!("[demo] surface_interact → walk {:?}", dir);
                Transition::To(State::Walking { dir: *dir, frame: 0, frame_elapsed: 0.0 })
            }

            // ── Walking ───────────────────────────────────────────────
            State::Walking { dir, .. } => {
                // Desktop: jump toward a nearby window as soon as one is in range.
                if matches!(ctx.surface, Surface::Desktop { .. }) && !ctx.at_edge {
                    if let Some((win_id, side)) = &ctx.jump_target {
                        eprintln!("[demo] walk → jump_runup");
                        return Transition::To(State::JumpRunup {
                            elapsed: 0.0, target_win_id: *win_id, target_side: *side,
                            landing_mode: LandingMode::ClimbFromBottom,
                        });
                    }
                }
                if !ctx.at_edge { return Transition::Stay; }

                match ctx.surface {
                    Surface::WindowTop { .. } => {
                        match self.take_edge() {
                            // 0: idle at edge — shows SitIdle then LieIdle with head turns
                            0 => {
                                eprintln!("[demo] edge → sit_idle (→ lie_idle cycle)");
                                Transition::To(Self::make_sit_idle())
                            }
                            // 1 & 3: round the corner (exit decided later in CornerRest)
                            1 | 3 => {
                                eprintln!("[demo] edge → corner_transition_side (down)");
                                Transition::To(State::CornerTransitionSide {
                                    elapsed: 0.0, going_up: false, side: dir_to_side(*dir),
                                })
                            }
                            // 2: peek down over the edge
                            2 => {
                                eprintln!("[demo] edge → surface_interact/peek_down");
                                Transition::To(State::SurfaceInteract {
                                    animation: "peek-down".to_string(),
                                    elapsed: 0.0,
                                    duration: 2.0,
                                    dir: *dir,
                                })
                            }
                            // 4: shocked fall off the edge
                            _ => {
                                eprintln!("[demo] edge → shocked fall");
                                Transition::To(State::Falling {
                                    vx: 0.0, vy: 0.0,
                                    shocked: cfg.floor.shocked_duration,
                                })
                            }
                        }
                    }
                    // Desktop screen boundary: idle briefly, then walk inward.
                    Surface::Desktop { .. } => {
                        eprintln!("[demo] desktop edge → stand_idle (will walk inward)");
                        Transition::To(Self::make_stand_idle())
                    }
                    _ => Transition::Stay,
                }
            }

            // ── TurningAround ─────────────────────────────────────────
            State::TurningAround { to_dir, .. } => {
                if e < cfg.floor.turn_duration { return Transition::Stay; }
                eprintln!("[demo] turn → walk {:?}", to_dir);
                Transition::To(State::Walking { dir: *to_dir, frame: 0, frame_elapsed: 0.0 })
            }

            // ── StandIdle (should be rare in demo; transition quickly) ─
            State::StandIdle { duration, .. } => {
                if e < *duration { return Transition::Stay; }
                // Desktop edge: walk inward to escape the boundary.
                if ctx.at_edge {
                    if let Surface::Desktop { .. } = ctx.surface {
                        let dir = if ctx.surface_progress < 0.5 { Dir::Right } else { Dir::Left };
                        eprintln!("[demo] stand_idle (edge) → walk {:?} (inward)", dir);
                        return Transition::To(State::Walking { dir, frame: 0, frame_elapsed: 0.0 });
                    }
                }
                eprintln!("[demo] stand_idle → sit_idle");
                Transition::To(Self::make_sit_idle())
            }

            // ── SitIdle ──────────────────────────────────────────────
            State::SitIdle { duration, head_front, head_timer, .. } => {
                // Head-turn animation (timer decremented by the engine).
                if *head_timer <= 0.0 {
                    let (new_front, new_timer) = if *head_front {
                        (false, 3.0_f64) // look sideways for 3 s
                    } else {
                        (true,  2.0_f64) // look forward for 2 s
                    };
                    return Transition::To(State::SitIdle {
                        elapsed: e, duration: *duration,
                        head_front: new_front, head_timer: new_timer,
                    });
                }
                if e < *duration { return Transition::Stay; }
                eprintln!("[demo] sit_idle → lie_idle");
                Transition::To(Self::make_lie_idle())
            }

            // ── LieIdle ──────────────────────────────────────────────
            State::LieIdle { duration, head_front, head_timer, .. } => {
                if *head_timer <= 0.0 {
                    let (new_front, new_timer) = if *head_front {
                        (false, 3.5_f64)
                    } else {
                        (true,  2.5_f64)
                    };
                    return Transition::To(State::LieIdle {
                        elapsed: e, duration: *duration,
                        head_front: new_front, head_timer: new_timer,
                    });
                }
                if e < *duration { return Transition::Stay; }
                // Walk away from the edge (inward toward center).
                let dir = if ctx.surface_progress < 0.5 { Dir::Right } else { Dir::Left };
                eprintln!("[demo] lie_idle → walk {:?} (inward)", dir);
                Transition::To(State::Walking { dir, frame: 0, frame_elapsed: 0.0 })
            }

            // ── Sleeping (fallback; not normally entered in demo) ─────
            State::Sleeping { duration, head_front, head_timer, .. } => {
                if *head_timer <= 0.0 {
                    let (new_front, new_timer) = if *head_front {
                        (false, 4.0_f64)
                    } else {
                        (true,  2.0_f64)
                    };
                    return Transition::To(State::Sleeping {
                        elapsed: e, duration: *duration,
                        head_front: new_front, head_timer: new_timer,
                    });
                }
                if e < *duration { return Transition::Stay; }
                Transition::To(Self::make_lie_idle())
            }

            // ── JumpRunup ────────────────────────────────────────────
            State::JumpRunup { .. } => {
                if e >= cfg.jump.runup_duration {
                    Transition::To(State::WallEntry { elapsed: 0.0 })
                } else {
                    Transition::Stay
                }
            }

            // ── WallEntry ────────────────────────────────────────────
            State::WallEntry { .. } => {
                if e >= cfg.wall.entry_hold {
                    eprintln!("[demo] wall_entry → climbing_up");
                    Transition::To(State::ClimbingUp { frame: 0, frame_elapsed: 0.0, wall_frames: 0 })
                } else {
                    Transition::Stay
                }
            }

            // ── ClimbingUp ───────────────────────────────────────────
            // No mid-climb pauses in demo — keeps the cycle predictable.
            State::ClimbingUp { .. } => {
                if ctx.at_edge {
                    if let Surface::WindowWall { side, .. } = ctx.surface {
                        eprintln!("[demo] climb_up → corner_side (top)");
                        return Transition::To(State::CornerTransitionSide {
                            elapsed: 0.0, going_up: true, side: *side,
                        });
                    }
                }
                Transition::Stay
            }

            // ── ClimbingDown ─────────────────────────────────────────
            State::ClimbingDown { .. } => {
                if ctx.at_edge {
                    eprintln!("[demo] climb_down → fall (bottom)");
                    return Transition::To(State::Falling { vx: 0.0, vy: 0.0, shocked: 0.0 });
                }
                Transition::Stay
            }

            // ── WallPause (not triggered in demo, but handle gracefully) ─
            State::WallPause { duration, was_climbing_up, .. } => {
                if e < *duration { return Transition::Stay; }
                if *was_climbing_up {
                    Transition::To(State::ClimbingUp  { frame: 0, frame_elapsed: 0.0, wall_frames: 0 })
                } else {
                    Transition::To(State::ClimbingDown { frame: 0, frame_elapsed: 0.0, wall_frames: 0 })
                }
            }

            // ── Corner transitions ────────────────────────────────────
            State::CornerTransitionSide { going_up, side, .. } => {
                if e >= cfg.corner.side_corner_secs {
                    Transition::To(State::CornerTransitionFront {
                        elapsed: 0.0, going_up: *going_up, side: *side,
                    })
                } else {
                    Transition::Stay
                }
            }

            State::CornerTransitionFront { going_up, side: _side, .. } => {
                if e < cfg.corner.front_corner_secs { return Transition::Stay; }
                if *going_up {
                    // Arrived from wall: always rest.
                    // Alternate lying / sitting via the (un-advanced) corner counter.
                    let lying = self.peek_corner() % 2 == 0;
                    eprintln!("[demo] corner_front(up) → corner_rest (lying={})", lying);
                    Transition::To(Self::make_corner_rest(lying))
                } else {
                    // From window top: descend.
                    eprintln!("[demo] corner_front(down) → climbing_down");
                    Transition::To(State::ClimbingDown { frame: 0, frame_elapsed: 0.0, wall_frames: 0 })
                }
            }

            // ── CornerRest ───────────────────────────────────────────
            State::CornerRest { duration, .. } => {
                if e < *duration { return Transition::Stay; }
                match self.take_corner() {
                    // 0: descend the wall
                    0 => {
                        if let Surface::WindowUpperCorner { side, .. } = ctx.surface {
                            eprintln!("[demo] corner_rest → descend");
                            Transition::To(State::CornerTransitionFront {
                                elapsed: 0.0, going_up: false, side: *side,
                            })
                        } else {
                            Transition::To(Self::make_sit_idle())
                        }
                    }
                    // 1: walk inward along the window top
                    1 => {
                        let dir = match ctx.surface {
                            Surface::WindowUpperCorner { side: Side::Left, .. } => Dir::Right,
                            _ => Dir::Left,
                        };
                        eprintln!("[demo] corner_rest → walk {:?} (inward)", dir);
                        Transition::To(State::Walking { dir, frame: 0, frame_elapsed: 0.0 })
                    }
                    // 2: jump to a nearby window (walk inward as fallback)
                    _ => {
                        if let Some((win_id, side, landing_mode)) = ctx.attract_target {
                            eprintln!("[demo] corner_rest → jump_runup (window-to-window)");
                            Transition::To(State::JumpRunup {
                                elapsed: 0.0, target_win_id: win_id, target_side: side,
                                landing_mode,
                            })
                        } else {
                            // No nearby window — walk inward so the cycle can continue.
                            let dir = match ctx.surface {
                                Surface::WindowUpperCorner { side: Side::Left, .. } => Dir::Right,
                                _ => Dir::Left,
                            };
                            eprintln!("[demo] corner_rest → walk {:?} (no jump target)", dir);
                            Transition::To(State::Walking { dir, frame: 0, frame_elapsed: 0.0 })
                        }
                    }
                }
            }

            // ── Grabbed ──────────────────────────────────────────────
            State::Grabbed => Transition::Stay,

            // ── Running ──────────────────────────────────────────────
            State::Running { dir, duration, .. } => {
                if e >= *duration || ctx.at_edge {
                    Transition::To(State::Walking { dir: *dir, frame: 0, frame_elapsed: 0.0 })
                } else {
                    Transition::Stay
                }
            }

            // ── OneShot ──────────────────────────────────────────────
            State::OneShot { done, return_to, .. } => {
                if *done {
                    eprintln!("[demo] oneshot done → return_to");
                    Transition::To(*return_to.clone())
                } else {
                    Transition::Stay
                }
            }
        }
    }

    fn on_surface_lost(&self, ctx: &BehaviorContext) -> State {
        eprintln!("[demo] surface_lost → fall (shocked)");
        State::Falling { vx: 0.0, vy: 0.0, shocked: ctx.config.floor.shocked_duration }
    }

    fn on_landed(&self, _ctx: &BehaviorContext) -> State {
        eprintln!("[demo] landed → landing_stand_up");
        State::LandingStandUp { elapsed: 0.0 }
    }
}
