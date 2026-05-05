//! Platform-independent game engine helpers.
//!
//! Functions here depend only on `behavior` and `config` types and contain
//! no platform-specific code, so they can be compiled for every target.

use crate::behavior::State;
use crate::config::Config;

/// Advance per-state animation timers and frame counters by `dt` seconds.
/// Returns the current `elapsed` value for `BehaviorContext::elapsed_secs`.
pub fn advance_anim(state: &mut State, dt: f64, cfg: &Config) -> f64 {
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
