//! The catalogue, as netlists. The six microphone preamplifiers live in
//! `faderframe-circuit`; they are named here so the catalogue reads as
//! upstream's does, but the Guitar Station never builds them. `neve` is
//! upstream's own copy: it carries the line driver's loudspeaker adapter the
//! power-stage table names.

pub use faderframe_circuit::circuits::{american312, british_47, console_e, german_76, tube610};

#[rustfmt::skip]
pub mod ac30;
#[rustfmt::skip]
pub mod american_800rb;
#[rustfmt::skip]
pub mod american_ss800;
#[rustfmt::skip]
pub mod american_svt;
#[rustfmt::skip]
pub mod american_v4b;
#[rustfmt::skip]
pub mod american_vt40;
#[rustfmt::skip]
pub mod bass_driver;
#[rustfmt::skip]
pub mod bigmuff;
#[rustfmt::skip]
pub mod blue_chorus;
#[rustfmt::skip]
pub mod brit2205;
#[rustfmt::skip]
pub mod brit800;
#[rustfmt::skip]
pub mod brit_drive;
#[rustfmt::skip]
pub mod brum100;
#[rustfmt::skip]
pub mod cabinet;
#[rustfmt::skip]
pub mod clean_boost;
#[rustfmt::skip]
pub mod clipper;
#[rustfmt::skip]
pub mod deluxe;
#[rustfmt::skip]
pub mod distortion_plus;
#[rustfmt::skip]
pub mod dr103;
#[rustfmt::skip]
pub mod evh5150;
#[rustfmt::skip]
pub mod gold_drive;
#[rustfmt::skip]
pub mod heavy_metal;
#[rustfmt::skip]
pub mod iron;
#[rustfmt::skip]
pub mod jazz120;
#[rustfmt::skip]
pub mod jc120_power;
#[rustfmt::skip]
pub mod jfet;
#[rustfmt::skip]
pub mod jtm45;
#[rustfmt::skip]
pub mod markiic;
#[rustfmt::skip]
pub mod metal_zone;
#[rustfmt::skip]
pub mod modern_33;
#[rustfmt::skip]
pub mod modern_purple;
#[rustfmt::skip]
pub mod orange_dist;
#[rustfmt::skip]
pub mod orange_phase;
#[rustfmt::skip]
pub mod neve;
#[rustfmt::skip]
pub mod oregon_t;
#[rustfmt::skip]
pub mod plexi_bass;
#[rustfmt::skip]
pub mod plexi;
#[rustfmt::skip]
pub mod power;
#[rustfmt::skip]
pub mod preamp;
#[rustfmt::skip]
pub mod rectifier;
#[rustfmt::skip]
pub mod rodent;
#[rustfmt::skip]
pub mod round_fuzz;
#[rustfmt::skip]
pub mod studio;
#[rustfmt::skip]
pub mod tone;
#[rustfmt::skip]
pub mod treble_boost;
#[rustfmt::skip]
pub mod ts808;
#[rustfmt::skip]
pub mod twin;
#[rustfmt::skip]
pub mod valve;
#[rustfmt::skip]
pub mod wah;
