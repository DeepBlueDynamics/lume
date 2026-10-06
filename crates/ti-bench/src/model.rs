//! Path model: the Signal K paths the generator emits, matching
//! `tests/golden/paths.md` (the corpus/generator coordination contract).

/// Fixed-point scale per path, matching the corpus's `paths.md` scale table and
/// the frozen W0 unit→scale registry.
pub fn scale_for(path: &str) -> Option<u8> {
    Some(match path {
        "environment.wind.speedTrue" => 3,
        "environment.wind.speedApparent" => 3,
        "navigation.speedOverGround" => 3,
        "navigation.position.latitude" => 7,
        "navigation.position.longitude" => 7,
        "navigation.attitude.roll" => 4,
        "propulsion.port.motorPower" => 1,
        "propulsion.starboard.motorPower" => 1,
        "propulsion.port.revolutions" => 2, // explicit override (Hz default is 1)
        "propulsion.starboard.revolutions" => 2,
        "propulsion.port.temperature" => 2,
        "propulsion.starboard.temperature" => 2,
        "electrical.batteries.house.voltage" => 3,
        "electrical.batteries.house.current" => 2,
        "electrical.batteries.house.stateOfCharge" => 4,
        "electrical.solar.house.panelPower" => 1,
        "environment.depth.belowTransducer" => 2,
        "environment.outside.temperature" => 2,
        "environment.outside.pressure" => 0,
        "tanks.freshWater.port.currentLevel" => 4,
        "tanks.freshWater.starboard.currentLevel" => 4,
        _ => return None,
    })
}

/// The numeric (bsi) paths emitted as scalar `value` DOUBLE samples.
pub const NUMERIC_PATHS: &[&str] = &[
    "environment.wind.speedTrue",
    "environment.wind.speedApparent",
    "navigation.speedOverGround",
    "propulsion.port.motorPower",
    "propulsion.starboard.motorPower",
    "propulsion.port.revolutions",
    "propulsion.starboard.revolutions",
    "propulsion.port.temperature",
    "propulsion.starboard.temperature",
    "electrical.batteries.house.voltage",
    "electrical.batteries.house.current",
    "electrical.batteries.house.stateOfCharge",
    "electrical.solar.house.panelPower",
    "environment.depth.belowTransducer",
    "environment.outside.temperature",
    "environment.outside.pressure",
    "tanks.freshWater.port.currentLevel",
    "tanks.freshWater.starboard.currentLevel",
];

/// The `count` path (bilge pump cycles): each event is one raw row.
pub const COUNT_PATHS: &[&str] = &["electrical.bilge.pumpCycles"];

/// The object paths (flattened to value_<key> columns; no `value`).
pub const POSITION_PATH: &str = "navigation.position";
pub const ATTITUDE_PATH: &str = "navigation.attitude";

/// The set paths emitted as scalar `value` UTF8 samples.
pub const SET_PATHS: &[&str] = &[
    "propulsion.main.state",
    "propulsion.port.state",
    "propulsion.starboard.state",
    "navigation.state",
];

/// Vessel contexts. Vessel 0 is the primary twin-motor vessel; 1..4 are background.
pub const VESSEL_URNS: &[&str] = &[
    "vessels.urn:mrn:imo:mmsi:367000000",
    "vessels.urn:mrn:imo:mmsi:367000001",
    "vessels.urn:mrn:imo:mmsi:367000002",
    "vessels.urn:mrn:imo:mmsi:367000003",
    "vessels.urn:mrn:imo:mmsi:367000004",
];

pub const VESSEL_NAMES: &[&str] = &["PV-1", "Fleet-2", "Fleet-3", "Fleet-4", "Fleet-5"];

/// A single source label for every path, so the preferred-source choice is
/// deterministic (per the lead's set-field ruling).
pub const SOURCE_LABEL: &str = "can0.115";

/// Document kinds.
pub const DOC_KINDS: &[&str] = &["notes", "logbook", "alerts"];

/// Planted keywords that the `match()` oracle's LIKE-substring expects.
pub const PLANTED_NOTES: &[&str] = &["leak", "water", "bilge", "mooring", "weather"];
pub const PLANTED_LOGBOOK: &[&str] = &["anchorage"];
pub const PLANTED_ALERTS: &[&str] = &["alarm"];
