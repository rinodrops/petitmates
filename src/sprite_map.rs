//! Maps `State` to a sprite name and a horizontal-mirror flag.
//!
//! All side-view sprites (`s-*`) are authored facing **left**.
//! When `mirror` is `true` the renderer must flip the image horizontally
//! before drawing — no extra sprite files are needed.

use crate::behavior::{Dir, Side, State};

/// Result of `sprite_for_state`.
#[derive(Debug, Clone, PartialEq)]
pub struct SpriteRef {
    /// Key into the sprite map (filename without `.png`).
    pub name: &'static str,
    /// If `true`, draw the sprite horizontally mirrored.
    pub mirror: bool,
}

impl SpriteRef {
    fn new(name: &'static str, mirror: bool) -> Self {
        Self { name, mirror }
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

/// Walk animation: ping-pong 0 → 1 → 2 → 1 → 0 → …
/// `frame` wraps in the range 0..=3 (engine uses `% 4`).
fn walk_frame_name(frame: u8) -> &'static str {
    match frame % 4 {
        0 => "s-walk-0",
        1 | 3 => "s-walk-1",
        _ => "s-walk-2", // 2
    }
}

/// Climb animation: ping-pong 0 → 1 → 2 → 1 → 0 → …
/// `frame` wraps in the range 0..=3 (engine uses `% 4`).
fn climb_frame_name(frame: u8) -> &'static str {
    match frame % 4 {
        0 => "s-hang-wall-0",
        1 | 3 => "s-hang-wall-1",
        _ => "s-hang-wall-2", // 2
    }
}

/// Return the sprite and mirror flag for the current `State`.
///
/// `facing` is the character's current horizontal direction.
/// For wall/corner states, direction is derived from `Side`.
pub fn sprite_for_state(state: &State, facing: Dir) -> SpriteRef {
    match state {
        // ── Airborne ─────────────────────────────────────────────────
        State::Falling { .. } => SpriteRef::side("s-jump", facing),

        // ── Floor / Window top ────────────────────────────────────────
        State::LandingStandUp { .. } => SpriteRef::side("s-stand-up", facing),

        State::Observing { .. } => SpriteRef::side("s-stand", facing),

        State::Walking { dir, frame, .. } => {
            SpriteRef::side(walk_frame_name(*frame), *dir)
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

        State::PeekDown { dir, .. } => SpriteRef::side("s-peek-down", *dir),

        State::JumpRunup { .. } => SpriteRef::side("s-stand-up", facing),

        // ── Wall ─────────────────────────────────────────────────────
        State::WallEntry { .. } => {
            let side = wall_side_from_surface(facing);
            SpriteRef::wall("s-hang-wall-0", side)
        }

        State::ClimbingUp { frame, .. } | State::ClimbingDown { frame, .. } => {
            let side = wall_side_from_surface(facing);
            SpriteRef::wall(climb_frame_name(*frame), side)
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

    #[test]
    fn walk_frames_cycle() {
        for (frame, expected) in [(0, "s-walk-0"), (1, "s-walk-1"), (2, "s-walk-2")] {
            let s = sprite_for_state(
                &State::Walking { dir: Dir::Left, frame, frame_elapsed: 0.0 },
                Dir::Left,
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
        );
        assert!(s.mirror);
    }

    #[test]
    fn corner_rest_sprites() {
        let sit = sprite_for_state(
            &State::CornerRest { elapsed: 0.0, duration: 5.0, lying: false },
            Dir::Left,
        );
        assert_eq!(sit.name, "f-sit");
        assert!(!sit.mirror);

        let lie = sprite_for_state(
            &State::CornerRest { elapsed: 0.0, duration: 5.0, lying: true },
            Dir::Left,
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
        );
        assert_eq!(s.name, "s-hang-wall-0");
        assert!(!s.mirror, "right wall: authored sprite, no mirror");

        // Right-facing character grips the left wall → mirror
        let s2 = sprite_for_state(
            &State::ClimbingUp { frame: 0, frame_elapsed: 0.0, wall_frames: 0 },
            Dir::Right,
        );
        assert!(s2.mirror, "left wall: needs mirror");
    }
}
