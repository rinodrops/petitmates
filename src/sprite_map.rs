//! Maps `State` to a sprite name and a horizontal-mirror flag.
//!
//! All side-view sprites (`s-*`) are authored facing **left**.
//! When `mirror` is `true` the renderer must flip the image horizontally
//! before drawing — no extra sprite files are needed.

use std::collections::HashMap;

use crate::behavior::{Dir, Side, State};
use crate::manifest::AnimationDef;

/// Result of `sprite_for_state`.
#[derive(Debug, Clone, PartialEq)]
pub struct SpriteRef {
    /// Key into the sprite map (filename without `.png`).
    pub name: String,
    /// If `true`, draw the sprite horizontally mirrored.
    pub mirror: bool,
}

impl SpriteRef {
    fn new(name: impl Into<String>, mirror: bool) -> Self {
        Self { name: name.into(), mirror }
    }

    /// Side-view, left-facing authored sprite.
    /// `dir == Right` → mirror.
    fn side(name: &'static str, dir: Dir) -> Self {
        Self::new(name, dir == Dir::Right)
    }

    /// Front-view sprite: never mirrored (symmetric or authored center).
    fn front(name: &'static str) -> Self {
        Self::new(name, false)
    }

    /// Wall sprite: authored for the **right** wall (character faces left
    /// while gripping the right wall).
    /// `side == Left` → mirror (so it grips the left wall).
    fn wall(name: &'static str, side: Side) -> Self {
        Self::new(name, side == Side::Left)
    }
}

/// Walk animation frame names indexed by sprite index (0-based).
static WALK_FRAME_NAMES: &[&str] = &[
    "s-walk-0", "s-walk-1", "s-walk-2", "s-walk-3",
    "s-walk-4", "s-walk-5", "s-walk-6", "s-walk-7",
];

/// Climb animation frame names indexed by sprite index (0-based).
static CLIMB_FRAME_NAMES: &[&str] = &[
    "s-hang-wall-0", "s-hang-wall-1", "s-hang-wall-2", "s-hang-wall-3",
    "s-hang-wall-4", "s-hang-wall-5", "s-hang-wall-6", "s-hang-wall-7",
];

fn walk_frame_name(tick: u8, anim: &AnimationDef) -> &'static str {
    let idx = anim.sprite_index(tick) as usize;
    WALK_FRAME_NAMES.get(idx).copied().unwrap_or("s-walk-0")
}

fn climb_frame_name(tick: u8, anim: &AnimationDef) -> &'static str {
    let idx = anim.sprite_index(tick) as usize;
    CLIMB_FRAME_NAMES.get(idx).copied().unwrap_or("s-hang-wall-0")
}

/// Return the sprite and mirror flag for the current `State`.
///
/// `facing` is the character's current horizontal direction.
/// For wall/corner states, direction is derived from `Side`.
pub fn sprite_for_state(
    state: &State,
    facing: Dir,
    animations: &HashMap<String, AnimationDef>,
) -> SpriteRef {
    match state {
        // ── Airborne ─────────────────────────────────────────────────
        State::Falling { shocked, .. } => {
            if *shocked > 0.0 { SpriteRef::front("f-shocked") }
            else { SpriteRef::side("s-jump", facing) }
        }
        State::Airborne { .. } => SpriteRef::side("s-jump", facing),

        // ── Floor / Window top ────────────────────────────────────────
        State::LandingStandUp { .. } => SpriteRef::side("s-stand-up", facing),

        State::Observing { .. } => SpriteRef::side("s-stand", facing),

        State::Walking { dir, frame, .. } => {
            let anim = animations.get("walk").cloned().unwrap_or_default();
            SpriteRef::side(walk_frame_name(*frame, &anim), *dir)
        }

        // Turn-around: three-phase animation driven by elapsed vs turn_duration.
        // Caller divides elapsed into thirds and passes the appropriate sprite.
        // Here we return the middle (front-facing) phase as a safe default;
        // the renderer overrides using `sprite_for_turn`.
        State::TurningAround { .. } => SpriteRef::front("f-stand"),

        State::StandIdle { bob_phase: false, .. } => SpriteRef::side("s-stand", facing),
        State::StandIdle { bob_phase: true, .. } => SpriteRef::side("s-stand-close", facing),

        State::SitIdle { head_front: false, .. } => SpriteRef::side("s-sit", facing),
        State::SitIdle { head_front: true,  .. } => SpriteRef::front("f-sit"),

        State::LieIdle { head_front: false, .. } => SpriteRef::side("s-lie", facing),
        State::LieIdle { head_front: true,  .. } => SpriteRef::front("f-lie"),

        State::Sleeping { head_front: false, .. } => SpriteRef::side("s-lie-sleep", facing),
        State::Sleeping { head_front: true,  .. } => SpriteRef::front("f-lie-sleep"),

        State::SurfaceInteract { animation, dir, .. } => {
            SpriteRef::new(format!("s-{animation}"), *dir == Dir::Right)
        }

        State::JumpRunup { .. } => SpriteRef::side("s-stand-up", facing),

        // ── Wall ─────────────────────────────────────────────────────
        State::WallEntry { .. } => {
            let side = wall_side_from_surface(facing);
            SpriteRef::wall("s-hang-wall-0", side)
        }

        State::ClimbingUp { frame, .. } | State::ClimbingDown { frame, .. } => {
            let side = wall_side_from_surface(facing);
            let anim = animations.get("climb").cloned().unwrap_or_default();
            SpriteRef::wall(climb_frame_name(*frame, &anim), side)
        }

        State::WallPause { .. } => {
            let side = wall_side_from_surface(facing);
            SpriteRef::wall("s-hang-wall-0", side)
        }

        // ── Corner transitions ────────────────────────────────────────
        State::CornerTransitionSide { .. } => SpriteRef::side("s-hang-corner", facing),

        // f-hang-corner is authored for the Right corner (character faces left).
        // For the Left corner the sprite must be mirrored (character faces right).
        State::CornerTransitionFront { side, .. } => SpriteRef::new("f-hang-corner", *side == Side::Left),

        // ── Corner rest ───────────────────────────────────────────────
        State::CornerRest { lying: false, .. } => SpriteRef::front("f-sit"),
        State::CornerRest { lying: true, .. } => SpriteRef::front("f-lie"),


        // ── One-shot ──────────────────────────────────────────────────
        State::OneShot { animation, frame, done, .. } => {
            let anim = animations.get(animation.as_str()).cloned().unwrap_or_default();
            let idx = if *done {
                anim.frames.saturating_sub(1)
            } else {
                anim.sprite_index(*frame)
            };
            SpriteRef::new(format!("s-{animation}-{idx}"), facing == Dir::Right)
        }

        // ── Grabbed ──────────────────────────────────────────────────
        State::Grabbed => SpriteRef::side("s-hang-corner", facing),
    }
}

/// Three-phase sprite for a turn-around animation.
/// `progress` is in [0.0, 1.0] across the full `turn_duration`.
pub fn sprite_for_turn(progress: f64, from_dir: Dir) -> SpriteRef {
    if progress < 0.29 {
        // Phase 1: side view, mouth closed, original direction
        SpriteRef::side("s-stand-close", from_dir)
    } else if progress < 0.71 {
        // Phase 2: front view (symmetric)
        SpriteRef::front("f-stand")
    } else {
        // Phase 3: side view, mouth closed, new direction (mirrored)
        SpriteRef::side("s-stand-close", from_dir.opposite())
    }
}

/// Derive a `Side` from the character's facing direction when on a wall.
/// Left-facing character grips the right wall; right-facing grips the left wall.
/// (Side-view sprites are authored for the right wall, facing left.)
fn wall_side_from_surface(facing: Dir) -> Side {
    match facing {
        Dir::Left => Side::Right,
        Dir::Right => Side::Left,
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::behavior::State;

    fn no_anims() -> HashMap<String, AnimationDef> { HashMap::new() }

    #[test]
    fn walk_frames_cycle() {
        for (frame, expected) in [(0, "s-walk-0"), (1, "s-walk-1"), (2, "s-walk-2")] {
            let s = sprite_for_state(
                &State::Walking { dir: Dir::Left, frame, frame_elapsed: 0.0 },
                Dir::Left,
                &no_anims(),
            );
            assert_eq!(s.name, expected);
            assert!(!s.mirror, "left-facing should not mirror");
        }
    }

    #[test]
    fn walk_right_mirrors() {
        let s = sprite_for_state(
            &State::Walking { dir: Dir::Right, frame: 0, frame_elapsed: 0.0 },
            Dir::Right,
            &no_anims(),
        );
        assert!(s.mirror);
    }

    #[test]
    fn corner_rest_sprites() {
        let sit = sprite_for_state(
            &State::CornerRest { elapsed: 0.0, duration: 5.0, lying: false },
            Dir::Left,
            &no_anims(),
        );
        assert_eq!(sit.name, "f-sit");
        assert!(!sit.mirror);

        let lie = sprite_for_state(
            &State::CornerRest { elapsed: 0.0, duration: 5.0, lying: true },
            Dir::Left,
            &no_anims(),
        );
        assert_eq!(lie.name, "f-lie");
    }

    #[test]
    fn turn_phases() {
        let p1 = sprite_for_turn(0.0, Dir::Left);
        assert_eq!(p1.name, "s-stand-close");
        assert!(!p1.mirror);

        let p2 = sprite_for_turn(0.5, Dir::Left);
        assert_eq!(p2.name, "f-stand");

        let p3 = sprite_for_turn(1.0, Dir::Left);
        assert_eq!(p3.name, "s-stand-close");
        assert!(p3.mirror, "phase 3 should mirror (now facing right)");
    }

    #[test]
    fn wall_grip_sides() {
        // Left-facing character should grip the right wall (no mirror)
        let s = sprite_for_state(
            &State::ClimbingUp { frame: 0, frame_elapsed: 0.0, wall_frames: 0 },
            Dir::Left,
            &no_anims(),
        );
        assert_eq!(s.name, "s-hang-wall-0");
        assert!(!s.mirror, "right wall: authored sprite, no mirror");

        // Right-facing character grips the left wall → mirror
        let s2 = sprite_for_state(
            &State::ClimbingUp { frame: 0, frame_elapsed: 0.0, wall_frames: 0 },
            Dir::Right,
            &no_anims(),
        );
        assert!(s2.mirror, "left wall: needs mirror");
    }

    #[test]
    fn anim_def_ping_pong() {
        let anim = AnimationDef { frames: 3, mode: crate::manifest::AnimMode::PingPong, frame_secs: 0.12 };
        // cycle: 0→1→2→1, length 4
        assert_eq!(anim.cycle_len(), 4);
        assert_eq!(anim.sprite_index(0), 0);
        assert_eq!(anim.sprite_index(1), 1);
        assert_eq!(anim.sprite_index(2), 2);
        assert_eq!(anim.sprite_index(3), 1);
    }

    #[test]
    fn anim_def_loop() {
        let anim = AnimationDef { frames: 4, mode: crate::manifest::AnimMode::Loop, frame_secs: 0.12 };
        assert_eq!(anim.cycle_len(), 4);
        assert_eq!(anim.sprite_index(0), 0);
        assert_eq!(anim.sprite_index(3), 3);
        assert_eq!(anim.sprite_index(4), 0);
    }
}
