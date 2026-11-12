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
    Track,     // lullaby track picker: names out, DP 202 ids in, state from DP 248
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
    /// For BinarySensor: the string that means "on" (others mean off). Empty:
    /// any non-empty string or `true` is on, and the event clears itself.
    pub on_value: &'static str,
}

const fn dp(id: u32, slug: &'static str, name: &'static str, kind: Kind) -> Dp {
    Dp { id, slug, name, kind, min: 0.0, max: 0.0, scale: 1.0, unit: "", device_class: "", options: &[], writable: true, on_value: "" }
}

/// The controls and sensors exposed to Home Assistant.
pub static DPS: &[Dp] = &[
    Dp { unit: "", ..dp(138, "night_light", "Night Light", Kind::Light) },
    Dp { min: 1.0, max: 100.0, unit: "%", ..dp(158, "brightness", "Brightness", Kind::Brightness) },
    Dp { options: &["play", "pause", "stop", "next", "prev"], ..dp(201, "lullaby", "Lullaby", Kind::Select) },
    Dp { options: LULLABY_TRACKS, ..dp(202, "lullaby_track", "Lullaby Track", Kind::Track) },
    Dp { device_class: "running", on_value: "playing", writable: false, ..dp(246, "lullaby_playing", "Lullaby Playing", Kind::BinarySensor) },
    Dp { options: &["loop", "loop1", "shuffle"], ..dp(203, "play_mode", "Play Mode", Kind::Select) },
    Dp { min: 1.0, max: 100.0, unit: "%", ..dp(209, "volume", "Volume", Kind::Number) },
    Dp { scale: 100.0, unit: "°C", device_class: "temperature", writable: false, ..dp(207, "temperature", "Temperature", Kind::Sensor) },
    dp(134, "motion_switch", "Motion Alert", Kind::Switch),
    dp(139, "sound_switch", "Sound Alert", Kind::Switch),
    dp(237, "privacy", "Privacy Mode", Kind::Switch),
    Dp { device_class: "motion", writable: false, ..dp(250, "motion", "Motion", Kind::BinarySensor) },
    Dp { device_class: "sound", writable: false, ..dp(141, "sound", "Sound", Kind::BinarySensor) },
];

/// DP the monitor reports the current track in, as
/// `{"bizcode":"phi-no-bm","id":3542155,"errcode":0}`. Written tracks (DP 202)
/// are echoed here, not on 202.
pub const PLAY_CURRENT: u32 = 248;
pub const PLAY_CONTROL: u32 = 201;
pub const PLAY_TRACK: u32 = 202;

/// The built-in tracks, in id order (also the order `next` steps through
/// them). From aventproxy's `LULLABY_CATALOGUE`.
pub static LULLABY_TRACKS: &[&str] = &[
    "Baa Baa Black Sheep",
    "Brahms' Lullaby",
    "Rock-a-Bye Baby",
    "Golden Slumbers",
    "Hush Little Baby",
    "Mother's Shh",
    "Calming River",
    "Heartbeat",
    "Vacuum Cleaner",
    "White Noise",
    "Garden Bird Song",
    "Valley Wind",
    "Ocean Shore",
    "Night-time Nature",
    "Rain Shower",
];

/// First track id of each block the catalogue is published under. Units of
/// the same product report different blocks (SCD973: 3542154, firmware
/// 1.1.12: 12072916, SCD923 1.4.0: 3219038), and a monitor ignores an id
/// outside its own block.
pub static TRACK_BLOCKS: &[u32] = &[3542154, 12072916, 3219038];

/// The block start and track name of a DP 248 track id.
pub fn track_by_id(id: u32) -> Option<(u32, &'static str)> {
    let n = LULLABY_TRACKS.len() as u32;
    TRACK_BLOCKS.iter().find(|&&b| (b..b + n).contains(&id)).map(|&b| (b, LULLABY_TRACKS[(id - b) as usize]))
}

/// The DP 202 value that plays track `index` of block `block`.
pub fn track_value(block: u32, index: usize) -> String {
    serde_json::json!({ "bizcode": "phi-no-bm", "id": block + index as u32 }).to_string()
}

/// The track id in a DP 248 value (a JSON string, or already an object).
pub fn current_track_id(raw: &serde_json::Value) -> Option<u32> {
    let v = match raw {
        serde_json::Value::String(s) => serde_json::from_str(s).ok()?,
        other => other.clone(),
    };
    v.get("id")?.as_u64().and_then(|n| u32::try_from(n).ok())
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn track_ids_map_to_names_in_every_block() {
        assert_eq!(track_by_id(3542155), Some((3542154, "Brahms' Lullaby")));
        assert_eq!(track_by_id(12072916 + 14), Some((12072916, "Rain Shower")));
        assert_eq!(track_by_id(3219038 + 15), None);
        assert_eq!(current_track_id(&serde_json::json!(r#"{"bizcode":"phi-no-bm","id":3542155,"errcode":0}"#)), Some(3542155));
        assert_eq!(track_value(3219038, 1), r#"{"bizcode":"phi-no-bm","id":3219039}"#);
    }
}
