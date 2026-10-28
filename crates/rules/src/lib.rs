//! Pure, deterministic gameplay rules for SimCore.
//!
//! Every function here is plain `f64` arithmetic with no state, no randomness and no allocation.
//! Units are SI throughout: metres (m), seconds (s), radians (rad), watts (W). The rules are
//! deliberately game-level: the numbers that tune them are data supplied by the caller.
//!
//! All functions return finite results for finite inputs, including the degenerate cases called
//! out in their docs (zero speed, coincident points, a target directly behind).

mod detection;
mod origin;
mod steering;
mod sweep;
mod vec3;

pub use detection::{detection_range, in_detection_range, signal_strength, signal_strength_sq};
pub use origin::{to_local, to_local_f32};
pub use steering::{Kinematic, PursuitParams, pursuit_velocity};
pub use sweep::{Approach, closest_approach};
pub use vec3::Vec3;
