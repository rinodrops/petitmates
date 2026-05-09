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
        State::Falling { shocked, .. } => {
            *shocked = (*shocked - dt).max(0.0);
            0.0
        }
        State::Grabbed => 0.0,

        State::LandingStandUp { elapsed }
        | State::Observing { elapsed, .. }
        | State::TurningAround { elapsed, .. }
        | State::PeekDown { elapsed, .. }
        | State::JumpRunup { elapsed, .. }
        | State::WallEntry { elapsed }
        | State::WallPause { elapsed, .. }
        | State::CornerTransitionSide { elapsed, .. }
        | State::CornerTransitionFront { elapsed, .. }
        | State::CornerRest { elapsed, .. } => {
            *elapsed += dt;
            *elapsed
        }

        State::SitIdle { elapsed, head_timer, .. }
        | State::LieIdle { elapsed, head_timer, .. }
        | State::Sleeping { elapsed, head_timer, .. } => {
            *elapsed += dt;
            *head_timer = (*head_timer - dt).max(-1.0); // clamp to avoid large negatives
            *elapsed
        }

        State::StandIdle { elapsed, bob_elapsed, bob_phase, bob_next, .. } => {
            *elapsed += dt;
            *bob_elapsed += dt;
            if *bob_elapsed >= *bob_next {
                *bob_elapsed = 0.0;
                *bob_phase = !*bob_phase;
                // Mouth just opened → use open duration; mouth just closed → use long interval.
                *bob_next = if *bob_phase {
                    (cfg.floor.headbob_open_duration[0] + cfg.floor.headbob_open_duration[1]) / 2.0
                } else {
                    (cfg.floor.headbob_period[0] + cfg.floor.headbob_period[1]) / 2.0
                };
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
