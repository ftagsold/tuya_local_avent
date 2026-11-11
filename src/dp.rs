//! Data-point map for the Philips Avent monitor, from aventproxy's
//! `examples/DPS_REFERENCE.md`. Each entry is one Home Assistant entity, except
//! `Brightness`, which belongs to the light.

/// Home Assistant entity component for a DP.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Switch,
    Light,     // on/off bulb; its brightness is the `Brightness` DP
    Brightness, // dimmer level of the light (part of the light entity)
    Number,    // slider
    Select,    // enum
    Sensor,    // read-only value
    BinarySensor, // read-only event (motion/sound)
}

pub struct Dp {
    pub id: u32,
    /// Stable object_id suffix for HA discovery topics.
    pub slug: &'static str,
    pub name: &'static str,
    pub kind: Kind,
    /// For Number: (min, max). For Sensor with scaling: divisor in `scale`.
    pub min: f64,
    pub max: f64,
    /// Divisor applied to the raw value for display (e.g. temperature ×100).
    pub scale: f64,
    pub unit: &'static str,
    pub device_class: &'static str,
    /// Enum options for Select (command value list).
    pub options: &'static [&'static str],
    pub writable: bool,
}

const fn dp(id: u32, slug: &'static str, name: &'static str, kind: Kind) -> Dp {
    Dp { id, slug, name, kind, min: 0.0, max: 0.0, scale: 1.0, unit: "", device_class: "", options: &[], writable: true }
}

/// The controls and sensors exposed to Home Assistant.
pub static DPS: &[Dp] = &[
    Dp { unit: "", ..dp(138, "night_light", "Night Light", Kind::Light) },
    Dp { min: 1.0, max: 100.0, unit: "%", ..dp(158, "brightness", "Brightness", Kind::Brightness) },
    Dp { options: &["play", "pause", "stop", "next", "prev"], ..dp(201, "lullaby", "Lullaby", Kind::Select) },
    Dp { options: &["loop", "loop1", "shuffle"], ..dp(203, "play_mode", "Play Mode", Kind::Select) },
    Dp { min: 1.0, max: 100.0, unit: "%", ..dp(209, "volume", "Volume", Kind::Number) },
    Dp { scale: 100.0, unit: "°C", device_class: "temperature", writable: false, ..dp(207, "temperature", "Temperature", Kind::Sensor) },
    dp(134, "motion_switch", "Motion Alert", Kind::Switch),
    dp(139, "sound_switch", "Sound Alert", Kind::Switch),
    dp(237, "privacy", "Privacy Mode", Kind::Switch),
    Dp { device_class: "motion", writable: false, ..dp(250, "motion", "Motion", Kind::BinarySensor) },
    Dp { device_class: "sound", writable: false, ..dp(141, "sound", "Sound", Kind::BinarySensor) },
];

pub fn by_id(id: u32) -> Option<&'static Dp> {
    DPS.iter().find(|d| d.id == id)
}

/// Motion/sound detection is one-at-a-time: writing one switch must clear the
/// other. Returns the partner DP id to turn off, if any.
pub fn exclusive_partner(id: u32) -> Option<u32> {
    match id {
        134 => Some(139),
        139 => Some(134),
        _ => None,
    }
}
