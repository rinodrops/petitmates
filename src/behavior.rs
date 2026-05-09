/// Core types for the behavior state machine.
///
/// `BehaviorScript` is the trait that drives state transitions.
/// Currently, it uses `RustBehavior` (built-in).
/// In the future, I will add `LuaBehavior` for user `.pmate` characters.

use crate::config::Config;

// ---- Orientation helpers ----

/// Horizontal direction the character is facing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Left,
    Right,
}

impl Dir {
    pub fn opposite(self) -> Self {
        match self {
            Dir::Left => Dir::Right,
            Dir::Right => Dir::Left,
        }
    }
}

/// Which side of a window (left wall or right wall).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

// ---- Surface ----

/// Where the character currently resides.
/// Positions are in CG coordinates (origin = screen top-left, Y down).
#[derive(Debug, Clone, PartialEq)]
pub enum Surface {
    /// Desktop floor. `x` is horizontal position.
    Desktop { x: f64 },
    /// Top edge of a window. `win_id` is the CGWindowID; `x_local` is offset
    /// from the window's left edge.
    WindowTop { win_id: u32, x_local: f64 },
    /// Side wall of a window. `y_local` is offset from the window's top edge
    /// (increases downward in window-local space).
    WindowWall { win_id: u32, side: Side, y_local: f64 },
    /// Upper corner of a window (junction of top edge and side wall).
    WindowUpperCorner { win_id: u32, side: Side },
    /// In the air (falling or jumping). Not bound to any surface.
    Airborne,
}

// ---- State ----

/// The full animation/behavior state of the character.
///
/// Fields named `elapsed` always count seconds since the state was entered.
/// Fields named `duration` hold the randomly-chosen target duration for that
/// state (decided at entry time so the value is stable across ticks).
#[derive(Debug, Clone, PartialEq)]
pub enum State {
    // -- Airborne --
    /// Falling or jumping. `vx`/`vy` in px/s (positive y = downward in CG coords).
    /// `shocked > 0` means the character just fell off a ledge; shows `f-shocked`
    /// sprite until it counts down to zero.
    Falling { vx: f64, vy: f64, shocked: f64 },

    // -- Floor / WindowTop --
    /// Playing `s-stand-up` after landing.
    LandingStandUp { elapsed: f64 },
    /// Post-landing observation phase (head-bob / peek-down).
    Observing { elapsed: f64, duration: f64 },
    /// Walking along the floor or window top.
    Walking { dir: Dir, frame: u8, frame_elapsed: f64 },
    /// Turning around (side → front → mirrored side).
    TurningAround { elapsed: f64, to_dir: Dir },
    /// Standing idle; occasionally opens mouth once (`s-stand-close`) then returns to `s-stand`.
    /// `bob_next` is the seconds until the next phase change (long when closed, brief when open).
    StandIdle { elapsed: f64, duration: f64, bob_elapsed: f64, bob_phase: bool, bob_next: f64 },
    /// Sitting idle. `head_front` toggles between side view and front view;
    /// `head_timer` counts down to the next head turn.
    SitIdle { elapsed: f64, duration: f64, head_front: bool, head_timer: f64 },
    /// Lying idle.
    LieIdle { elapsed: f64, duration: f64, head_front: bool, head_timer: f64 },
    /// Sleeping.
    Sleeping { elapsed: f64, duration: f64, head_front: bool, head_timer: f64 },
    /// Peeking down over the edge.
    PeekDown { elapsed: f64, dir: Dir },
    /// Short run-up before jumping to a wall.
    JumpRunup { elapsed: f64, target_win_id: u32, target_side: Side },

    // -- Wall --
    /// Climbing up the wall. `frame` cycles 0→1→2→1→…; advanced by the engine.
    ClimbingUp { frame: u8, frame_elapsed: f64, wall_frames: u32 },
    /// Climbing down the wall (reverse frame order).
    ClimbingDown { frame: u8, frame_elapsed: f64, wall_frames: u32 },
    /// Pausing on the wall.
    WallPause { elapsed: f64, duration: f64, was_climbing_up: bool },
    /// Holding the entry pose (`s-hang-wall-0`) upon first attaching.
    WallEntry { elapsed: f64 },

    // -- Corner transitions --
    /// Playing `s-hang-corner` (side-view corner sprite).
    CornerTransitionSide { elapsed: f64, going_up: bool, side: Side },
    /// Playing `f-hang-corner` (front-view corner sprite).
    CornerTransitionFront { elapsed: f64, going_up: bool, side: Side },
    /// Resting at the upper corner (`f-sit` or `f-lie`).
    CornerRest { elapsed: f64, duration: f64, lying: bool },

    // -- User interaction --
    /// Character is being dragged by the user (⌘+drag on macOS).
    #[allow(dead_code)]
    Grabbed,
}

// ---- Transition ----

/// Result returned by `BehaviorScript::next_state`.
pub enum Transition {
    /// Stay in the current state (no change).
    Stay,
    /// Move to a new state (resets elapsed to 0 in the engine).
    To(State),
}

// ---- BehaviorContext ----

/// Snapshot of the world passed to behavior logic each tick.
/// Kept intentionally minimal so the future Lua API surface stays stable.
pub struct BehaviorContext<'a> {
    pub state: &'a State,
    pub surface: &'a Surface,
    /// Seconds elapsed in the current state.
    pub elapsed_secs: f64,
    /// Current runtime config (hot-reloaded TOML values).
    pub config: &'a Config,
    /// A pre-rolled random value in [0.0, 1.0) for this tick (for Lua compat).
    #[allow(dead_code)]
    pub rng01: f64,
    /// Normalized position on the current surface: 0.0 = left/top end,
    /// 1.0 = right/bottom end. Corners are 0.0 or 1.0.
    pub surface_progress: f64,
    /// Current facing direction of the character.
    pub facing: Dir,
    /// True when the character has reached the boundary of the surface in
    /// the direction it is heading (edge of window-top, top/bottom of wall, etc.).
    pub at_edge: bool,
    /// Nearest window and side eligible for a wall-jump (Desktop surface only).
    /// Restricted to the current walking direction and `wall_jump_max_dist`.
    pub jump_target: Option<(u32, Side)>,
    /// Nearest window within `climb_attract_dist` in either direction
    /// (Desktop surface only). Used for spontaneous window-climbing attraction.
    pub attract_target: Option<(u32, Side)>,
}

// ---- BehaviorScript trait ----

/// Drives the character's state transitions.
///
/// # Contract
/// - `next_state` is called once per tick. Return `Transition::Stay` when
///   the current state should continue unchanged.
/// - `on_surface_lost` is called when the current Surface disappears
///   (window closed / moved away). Must return a new `State` (typically
///   `State::Falling`).
/// - `on_landed` is called the tick the character touches a new Surface.
///   Must return the initial state on that surface (typically
///   `State::LandingStandUp`).
pub trait BehaviorScript: Send + Sync {
    fn next_state(&self, ctx: &BehaviorContext) -> Transition;
    fn on_surface_lost(&self, ctx: &BehaviorContext) -> State;
    fn on_landed(&self, ctx: &BehaviorContext) -> State;
}
