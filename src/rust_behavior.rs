//! Built-in Phase-1 behavior driver.
//!
//! All timing thresholds come from `Config` (hot-reloadable TOML).
//! Frame advancement and physics are handled by the engine (`macos.rs`);
//! this module only decides *which state to enter next*.

use std::sync::Mutex;

use rand::rngs::SmallRng;
use rand::{Rng, SeedableRng};

use crate::behavior::{
    BehaviorContext, BehaviorScript, Dir, Side, State, Surface, Transition,
};

// ---- Helpers ----

fn dir_to_side(dir: Dir) -> Side {
    match dir {
        Dir::Left => Side::Left,
        Dir::Right => Side::Right,
    }
}

// ---- RustBehavior ----

pub struct RustBehavior {
    rng: Mutex<SmallRng>,
}

impl RustBehavior {
    pub fn new() -> Self {
        Self { rng: Mutex::new(SmallRng::from_os_rng()) }
    }

    fn rnd(&self) -> f64 {
        self.rng.lock().unwrap().random::<f64>()
    }

    fn rnd_range(&self, range: [f64; 2]) -> f64 {
        range[0] + self.rnd() * (range[1] - range[0])
    }

    fn rnd_bool(&self, prob: f64) -> bool {
        self.rnd() < prob
    }

    /// Direction toward the nearer corner (used when choosing where to walk).
    fn toward_corner(surface_progress: f64) -> Dir {
        if surface_progress < 0.5 { Dir::Left } else { Dir::Right }
    }

    fn make_stand_idle(&self, ctx: &BehaviorContext) -> State {
        let dur = self.rnd_range(ctx.config.floor.stand_duration);
        let bob_next = self.rnd_range(ctx.config.floor.headbob_period);
        State::StandIdle { elapsed: 0.0, duration: dur, bob_elapsed: 0.0, bob_phase: false, bob_next }
    }

    fn make_sit_idle(&self, ctx: &BehaviorContext) -> State {
        let dur   = self.rnd_range(ctx.config.floor.sit_duration);
        let timer = self.rnd_range(ctx.config.floor.head_side_duration);
        State::SitIdle { elapsed: 0.0, duration: dur, head_front: false, head_timer: timer }
    }

    fn make_lie_idle(&self, ctx: &BehaviorContext) -> State {
        let dur   = self.rnd_range(ctx.config.floor.lie_duration);
        let timer = self.rnd_range(ctx.config.floor.head_side_duration);
        State::LieIdle { elapsed: 0.0, duration: dur, head_front: false, head_timer: timer }
    }

    fn make_sleeping(&self, ctx: &BehaviorContext) -> State {
        let dur   = self.rnd_range(ctx.config.floor.sleep_duration);
        let timer = self.rnd_range(ctx.config.floor.head_side_duration);
        State::Sleeping { elapsed: 0.0, duration: dur, head_front: false, head_timer: timer }
    }

    /// Advance the head-turn timer. Returns a `Transition::To` with the same
    /// state type but updated `head_front`/`head_timer` if the timer has fired,
    /// or `None` if the character should keep looking the same way.
    fn tick_head_sit(&self, elapsed: f64, duration: f64, head_front: bool, head_timer: f64, cfg: &crate::config::Config) -> Option<State> {
        if head_timer > 0.0 { return None; }
        let (new_front, new_timer) = if head_front {
            (false, self.rnd_range(cfg.floor.head_side_duration))
        } else {
            (true, self.rnd_range(cfg.floor.head_front_duration))
        };
        Some(State::SitIdle { elapsed, duration, head_front: new_front, head_timer: new_timer })
    }

    fn tick_head_lie(&self, elapsed: f64, duration: f64, head_front: bool, head_timer: f64, cfg: &crate::config::Config) -> Option<State> {
        if head_timer > 0.0 { return None; }
        let (new_front, new_timer) = if head_front {
            (false, self.rnd_range(cfg.floor.head_side_duration))
        } else {
            (true, self.rnd_range(cfg.floor.head_front_duration))
        };
        Some(State::LieIdle { elapsed, duration, head_front: new_front, head_timer: new_timer })
    }

    fn tick_head_sleep(&self, elapsed: f64, duration: f64, head_front: bool, head_timer: f64, cfg: &crate::config::Config) -> Option<State> {
        if head_timer > 0.0 { return None; }
        let (new_front, new_timer) = if head_front {
            (false, self.rnd_range(cfg.floor.head_side_duration))
        } else {
            (true, self.rnd_range(cfg.floor.head_front_duration))
        };
        Some(State::Sleeping { elapsed, duration, head_front: new_front, head_timer: new_timer })
    }

    fn walk_to_corner(&self, ctx: &BehaviorContext) -> State {
        let dir = Self::toward_corner(ctx.surface_progress);
        State::Walking { dir, frame: 0, frame_elapsed: 0.0 }
    }
}

impl BehaviorScript for RustBehavior {
    fn next_state(&self, ctx: &BehaviorContext) -> Transition {
        let cfg = ctx.config;
        let e = ctx.elapsed_secs;

        match ctx.state {
            // ── Airborne ─────────────────────────────────────────────
            // Physics (vx/vy, position) is updated by the engine.
            // Transition to a new state is triggered by on_landed().
            State::Falling { .. } => Transition::Stay,

            // ── Landing ──────────────────────────────────────────────
            State::LandingStandUp { .. } => {
                if e >= cfg.floor.standup_duration {
                    let dur = self.rnd_range(cfg.floor.observe_duration);
                    Transition::To(State::Observing { elapsed: 0.0, duration: dur })
                } else {
                    Transition::Stay
                }
            }

            // ── Observation phase ─────────────────────────────────────
            State::Observing { duration, .. } => {
                if e < *duration { return Transition::Stay; }
                // Spontaneous window attraction: jump toward a nearby window before
                // defaulting to the usual corner-walk / peek cycle.
                if let Surface::Desktop { .. } = ctx.surface {
                    if let Some((win_id, side)) = ctx.attract_target {
                        if self.rnd_bool(cfg.jump.climb_attract_prob) {
                            let dir = match side {
                                Side::Right => Dir::Left,
                                Side::Left  => Dir::Right,
                            };
                            return Transition::To(State::Walking { dir, frame: 0, frame_elapsed: 0.0 });
                        }
                        let _ = win_id; // suppress unused warning
                    }
                }
                let dir = Self::toward_corner(ctx.surface_progress);
                if self.rnd_bool(cfg.floor.peek_prob) {
                    Transition::To(State::PeekDown { elapsed: 0.0, dir })
                } else {
                    Transition::To(State::Walking { dir, frame: 0, frame_elapsed: 0.0 })
                }
            }

            // ── PeekDown ─────────────────────────────────────────────
            State::PeekDown { dir, .. } => {
                if e < cfg.floor.peek_duration { return Transition::Stay; }
                if self.rnd_bool(cfg.floor.peek_walk_prob) {
                    Transition::To(State::Walking { dir: *dir, frame: 0, frame_elapsed: 0.0 })
                } else {
                    Transition::To(State::TurningAround { elapsed: 0.0, to_dir: dir.opposite() })
                }
            }

            // ── Walking ───────────────────────────────────────────────
            // Frame advancement is handled by the engine.
            // Here we only react to reaching an edge.
            State::Walking { dir, .. } => {
                // On the desktop, trigger a wall jump as soon as a target window
                // comes within wall_jump_max_dist — but only if NOT at the screen
                // edge (at_edge=true means we are at the screen boundary; jumping
                // toward an off-screen or edge-hugging window would send the
                // character off-screen).
                if matches!(ctx.surface, Surface::Desktop { .. }) && !ctx.at_edge {
                    if let Some((win_id, side)) = &ctx.jump_target {
                        return Transition::To(State::JumpRunup {
                            elapsed: 0.0,
                            target_win_id: *win_id,
                            target_side: *side,
                        });
                    }
                }
                if !ctx.at_edge { return Transition::Stay; }
                match ctx.surface {
                    Surface::WindowTop { .. } => {
                        // Possibly idle at the edge before rounding the corner.
                        if self.rnd_bool(cfg.floor.edge_idle_prob) {
                            let r = self.rnd();
                            if r < cfg.floor.edge_arrive_stand_prob {
                                Transition::To(self.make_stand_idle(ctx))
                            } else if r < cfg.floor.edge_arrive_sit_prob {
                                Transition::To(self.make_sit_idle(ctx))
                            } else if r < cfg.floor.edge_arrive_lie_prob {
                                Transition::To(self.make_lie_idle(ctx))
                            } else {
                                Transition::To(self.make_sleeping(ctx))
                            }
                        } else {
                            Transition::To(State::CornerTransitionSide {
                                elapsed: 0.0,
                                going_up: false,
                                side: dir_to_side(*dir),
                            })
                        }
                    }
                    Surface::Desktop { .. } => {
                        if let Some((win_id, side)) = &ctx.jump_target {
                            Transition::To(State::JumpRunup {
                                elapsed: 0.0,
                                target_win_id: *win_id,
                                target_side: *side,
                            })
                        } else {
                            Transition::To(State::TurningAround {
                                elapsed: 0.0,
                                to_dir: dir.opposite(),
                            })
                        }
                    }
                    _ => Transition::Stay,
                }
            }

            // ── TurningAround ─────────────────────────────────────────
            State::TurningAround { to_dir, .. } => {
                if e < cfg.floor.turn_duration { return Transition::Stay; }
                if self.rnd_bool(cfg.floor.turn_walk_prob) {
                    Transition::To(State::Walking { dir: *to_dir, frame: 0, frame_elapsed: 0.0 })
                } else {
                    Transition::To(self.make_stand_idle(ctx))
                }
            }

            // ── StandIdle ─────────────────────────────────────────────
            // Head-bob advancement (bob_elapsed / bob_phase) is done by the engine.
            State::StandIdle { duration, .. } => {
                if e < *duration { return Transition::Stay; }
                // At a window-top edge: either deepen the idle chain or round the corner.
                if ctx.at_edge {
                    if let Surface::WindowTop { .. } = ctx.surface {
                        return if self.rnd_bool(cfg.floor.edge_stand_to_sit_prob) {
                            Transition::To(self.make_sit_idle(ctx))
                        } else {
                            Transition::To(State::CornerTransitionSide {
                                elapsed: 0.0,
                                going_up: false,
                                side: dir_to_side(ctx.facing),
                            })
                        };
                    }
                }
                // Spontaneous window attraction (Desktop only).
                if let Surface::Desktop { .. } = ctx.surface {
                    if let Some((win_id, side)) = ctx.attract_target {
                        if self.rnd_bool(cfg.jump.climb_attract_prob) {
                            let dir = match side {
                                Side::Right => Dir::Left,
                                Side::Left  => Dir::Right,
                            };
                            let _ = win_id;
                            return Transition::To(State::Walking { dir, frame: 0, frame_elapsed: 0.0 });
                        }
                    }
                }
                let r = self.rnd();
                if r < cfg.floor.stand_idle_sit_prob {
                    Transition::To(self.make_sit_idle(ctx))
                } else if r < cfg.floor.stand_idle_walk_prob {
                    Transition::To(self.walk_to_corner(ctx))
                } else if r < cfg.floor.stand_idle_turn_prob {
                    Transition::To(State::TurningAround {
                        elapsed: 0.0,
                        to_dir: ctx.facing.opposite(),
                    })
                } else {
                    Transition::To(State::PeekDown { elapsed: 0.0, dir: ctx.facing })
                }
            }

            // ── SitIdle ──────────────────────────────────────────────
            State::SitIdle { duration, head_front, head_timer, .. } => {
                // Head-turn tick (timer decremented by engine via elapsed).
                if let Some(s) = self.tick_head_sit(e, *duration, *head_front, *head_timer, cfg) {
                    return Transition::To(s);
                }
                if e < *duration { return Transition::Stay; }
                // At a window-top edge: deeper idle (lie) or back to stand.
                if ctx.at_edge {
                    if let Surface::WindowTop { .. } = ctx.surface {
                        return if self.rnd_bool(cfg.floor.edge_sit_to_lie_prob) {
                            Transition::To(self.make_lie_idle(ctx))
                        } else {
                            Transition::To(self.make_stand_idle(ctx))
                        };
                    }
                }
                // Spontaneous window attraction (Desktop only).
                if let Surface::Desktop { .. } = ctx.surface {
                    if let Some((win_id, side)) = ctx.attract_target {
                        if self.rnd_bool(cfg.jump.climb_attract_prob) {
                            let dir = match side {
                                Side::Right => Dir::Left,
                                Side::Left  => Dir::Right,
                            };
                            let _ = win_id;
                            return Transition::To(State::Walking { dir, frame: 0, frame_elapsed: 0.0 });
                        }
                    }
                }
                let r = self.rnd();
                if r < cfg.floor.sit_idle_lie_prob {
                    Transition::To(self.make_lie_idle(ctx))
                } else if r < cfg.floor.sit_idle_stand_prob {
                    Transition::To(self.make_stand_idle(ctx))
                } else {
                    Transition::To(self.walk_to_corner(ctx))
                }
            }

            // ── LieIdle ──────────────────────────────────────────────
            State::LieIdle { duration, head_front, head_timer, .. } => {
                // Head-turn tick.
                if let Some(s) = self.tick_head_lie(e, *duration, *head_front, *head_timer, cfg) {
                    return Transition::To(s);
                }
                if e < *duration { return Transition::Stay; }
                // At a window-top edge: sleep or back to sit.
                if ctx.at_edge {
                    if let Surface::WindowTop { .. } = ctx.surface {
                        return if self.rnd_bool(cfg.floor.edge_lie_to_sleep_prob) {
                            Transition::To(self.make_sleeping(ctx))
                        } else {
                            Transition::To(self.make_sit_idle(ctx))
                        };
                    }
                }
                // Spontaneous window attraction (Desktop only).
                if let Surface::Desktop { .. } = ctx.surface {
                    if let Some((win_id, side)) = ctx.attract_target {
                        if self.rnd_bool(cfg.jump.climb_attract_prob) {
                            let dir = match side {
                                Side::Right => Dir::Left,
                                Side::Left  => Dir::Right,
                            };
                            let _ = win_id;
                            return Transition::To(State::Walking { dir, frame: 0, frame_elapsed: 0.0 });
                        }
                    }
                }
                let r = self.rnd();
                if r < cfg.floor.lie_idle_sleep_prob {
                    Transition::To(self.make_sleeping(ctx))
                } else if r < cfg.floor.lie_idle_sit_prob {
                    Transition::To(self.make_sit_idle(ctx))
                } else {
                    Transition::To(self.walk_to_corner(ctx))
                }
            }

            // ── Sleeping ─────────────────────────────────────────────
            State::Sleeping { duration, head_front, head_timer, .. } => {
                // Head-turn tick.
                if let Some(s) = self.tick_head_sleep(e, *duration, *head_front, *head_timer, cfg) {
                    return Transition::To(s);
                }
                if e < *duration { return Transition::Stay; }
                Transition::To(self.make_lie_idle(ctx))
            }

            // ── JumpRunup ────────────────────────────────────────────
            // Shows a "look up" pose (s-stand-up) for runup_duration, then
            // snaps directly to the target wall (handled in macos.rs transition block).
            State::JumpRunup { .. } => {
                if e >= cfg.jump.runup_duration {
                    Transition::To(State::WallEntry { elapsed: 0.0 })
                } else {
                    Transition::Stay
                }
            }

            // ── Wall entry ───────────────────────────────────────────
            State::WallEntry { .. } => {
                if e >= cfg.wall.entry_hold {
                    Transition::To(State::ClimbingUp { frame: 0, frame_elapsed: 0.0, wall_frames: 0 })
                } else {
                    Transition::Stay
                }
            }

            // ── Climbing Up ──────────────────────────────────────────
            State::ClimbingUp { wall_frames, .. } => {
                if ctx.at_edge {
                    if let Surface::WindowWall { side, .. } = ctx.surface {
                        return Transition::To(State::CornerTransitionSide {
                            elapsed: 0.0,
                            going_up: true,
                            side: *side,
                        });
                    }
                }
                if *wall_frames > 0 && wall_frames % 3 == 0 && self.rnd_bool(cfg.wall.pause_prob) {
                    let dur = self.rnd_range(cfg.wall.pause_duration);
                    Transition::To(State::WallPause {
                        elapsed: 0.0,
                        duration: dur,
                        was_climbing_up: true,
                    })
                } else {
                    Transition::Stay
                }
            }

            // ── Climbing Down ────────────────────────────────────────
            State::ClimbingDown { wall_frames, .. } => {
                if ctx.at_edge {
                    // Reached the bottom of the wall — drop off.
                    // (Only fires when descending from the top; jumps from the
                    //  desktop climb *up* and never reach this branch.)
                    return Transition::To(State::Falling { vx: 0.0, vy: 0.0 });
                }
                if *wall_frames > 0 && wall_frames % 3 == 0 && self.rnd_bool(cfg.wall.pause_prob) {
                    let dur = self.rnd_range(cfg.wall.pause_duration);
                    Transition::To(State::WallPause {
                        elapsed: 0.0,
                        duration: dur,
                        was_climbing_up: false,
                    })
                } else {
                    Transition::Stay
                }
            }

            // ── WallPause ────────────────────────────────────────────
            State::WallPause { duration, was_climbing_up, .. } => {
                if e < *duration { return Transition::Stay; }
                if *was_climbing_up {
                    Transition::To(State::ClimbingUp { frame: 0, frame_elapsed: 0.0, wall_frames: 0 })
                } else {
                    Transition::To(State::ClimbingDown { frame: 0, frame_elapsed: 0.0, wall_frames: 0 })
                }
            }

            // ── Corner transitions ────────────────────────────────────
            State::CornerTransitionSide { going_up, side, .. } => {
                if e >= cfg.corner.side_corner_secs {
                    Transition::To(State::CornerTransitionFront {
                        elapsed: 0.0,
                        going_up: *going_up,
                        side: *side,
                    })
                } else {
                    Transition::Stay
                }
            }

            State::CornerTransitionFront { going_up, side, .. } => {
                if e < cfg.corner.front_corner_secs { return Transition::Stay; }
                if *going_up {
                    // Arrived at upper corner from the wall
                    if self.rnd_bool(cfg.corner.rest_prob) {
                        let dur = self.rnd_range(cfg.corner.rest_duration);
                        let lying = self.rnd_bool(cfg.corner.rest_lying_prob);
                        Transition::To(State::CornerRest { elapsed: 0.0, duration: dur, lying })
                    } else {
                        // Step onto the window top and walk inward
                        let dir = match side {
                            Side::Left => Dir::Right,
                            Side::Right => Dir::Left,
                        };
                        Transition::To(State::Walking { dir, frame: 0, frame_elapsed: 0.0 })
                    }
                } else {
                    // Came from window top → descend the wall
                    Transition::To(State::ClimbingDown { frame: 0, frame_elapsed: 0.0, wall_frames: 0 })
                }
            }

            // ── CornerRest ───────────────────────────────────────────
            State::CornerRest { duration, .. } => {
                if e < *duration { return Transition::Stay; }
                // Decide: descend wall or walk inward on window top
                if self.rnd_bool(cfg.corner.rest_descend_prob) {
                    if let Surface::WindowUpperCorner { side, .. } = ctx.surface {
                        Transition::To(State::CornerTransitionFront {
                            elapsed: 0.0,
                            going_up: false,
                            side: *side,
                        })
                    } else {
                        Transition::To(self.walk_to_corner(ctx))
                    }
                } else {
                    let dir = match ctx.surface {
                        Surface::WindowUpperCorner { side: Side::Left, .. } => Dir::Right,
                        _ => Dir::Left,
                    };
                    Transition::To(State::Walking { dir, frame: 0, frame_elapsed: 0.0 })
                }
            }


            // ── Grabbed ──────────────────────────────────────────────
            State::Grabbed => Transition::Stay,
        }
    }

    fn on_surface_lost(&self, _ctx: &BehaviorContext) -> State {
        State::Falling { vx: 0.0, vy: 0.0 }
    }

    fn on_landed(&self, _ctx: &BehaviorContext) -> State {
        State::LandingStandUp { elapsed: 0.0 }
    }
}
