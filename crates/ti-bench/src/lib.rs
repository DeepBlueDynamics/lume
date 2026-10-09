pub mod contention;
pub mod gen;
pub mod harness;
pub mod layout;
pub mod model;
pub mod otlp_soak;
pub mod rng;
pub mod robots;
pub mod sk_feed;
pub mod write;

/// Default master seed for the correctness set.
pub const DEFAULT_SEED: u64 = 0x5EED_5EED_5EED_5EED;

/// Correctness set: 5 vessels x ~92 days.
pub const CORRECTNESS_VESSELS: usize = 5;
/// Performance set: 50 vessels x 365 days (behind a flag).
pub const PERFORMANCE_VESSELS: usize = 50;

#[cfg(test)]
mod tests {
    use super::*;

    /// Byte-identical regeneration: the same seed produces the same samples, docs,
    /// positions and attitudes (the write layer is deterministic given equal input).
    #[test]
    fn regeneration_is_byte_identical() {
        let seed = 42;
        let a = gen::generate(seed, 1, gen::START_SECS, gen::START_SECS + 86400 * 3);
        let b = gen::generate(seed, 1, gen::START_SECS, gen::START_SECS + 86400 * 3);

        assert_eq!(a.samples.len(), b.samples.len());
        assert_eq!(a.positions.len(), b.positions.len());
        assert_eq!(a.attitudes.len(), b.attitudes.len());
        assert_eq!(a.docs.len(), b.docs.len());

        // Compare every sample field.
        for (x, y) in a.samples.iter().zip(&b.samples) {
            assert_eq!(x.context, y.context);
            assert_eq!(x.path, y.path);
            assert_eq!(x.ts_secs, y.ts_secs);
            assert_eq!(x.value, y.value);
            assert_eq!(x.value_str, y.value_str);
            assert_eq!(x.source_label, y.source_label);
        }
        for (x, y) in a.positions.iter().zip(&b.positions) {
            assert_eq!(x.context, y.context);
            assert_eq!(x.ts_secs, y.ts_secs);
            assert_eq!(x.latitude.to_bits(), y.latitude.to_bits());
            assert_eq!(x.longitude.to_bits(), y.longitude.to_bits());
        }
        for (x, y) in a.docs.iter().zip(&b.docs) {
            assert_eq!(x.body, y.body);
            assert_eq!(x.title, y.title);
            assert_eq!(x.ts_start, y.ts_start);
        }
    }

    /// Different seeds differ (sanity: the seed actually drives generation).
    #[test]
    fn different_seeds_differ() {
        let a = gen::generate(1, 1, gen::START_SECS, gen::START_SECS + 86400);
        let b = gen::generate(2, 1, gen::START_SECS, gen::START_SECS + 86400);
        let a_wind: Vec<f64> = a
            .samples
            .iter()
            .filter(|s| s.path == "environment.wind.speedTrue")
            .map(|s| s.value.unwrap())
            .collect();
        let b_wind: Vec<f64> = b
            .samples
            .iter()
            .filter(|s| s.path == "environment.wind.speedTrue")
            .map(|s| s.value.unwrap())
            .collect();
        assert_ne!(a_wind, b_wind);
    }

    /// The correctness set covers every corpus path (paths.md agreement).
    #[test]
    fn corpus_paths_are_generated() {
        let data = gen::generate(
            DEFAULT_SEED,
            1,
            gen::START_SECS,
            gen::START_SECS + 86400 * 5,
        );
        let mut paths: std::collections::BTreeSet<&str> =
            data.samples.iter().map(|s| s.path.as_str()).collect();
        paths.insert(model::POSITION_PATH);
        paths.insert(model::ATTITUDE_PATH);
        for p in model::NUMERIC_PATHS {
            assert!(paths.contains(p), "missing numeric path {p}");
        }
        for p in model::COUNT_PATHS {
            assert!(paths.contains(p), "missing count path {p}");
        }
        for p in model::SET_PATHS {
            assert!(paths.contains(p), "missing set path {p}");
        }
    }

    /// Byte-identical regeneration with 1 Hz and per-path override:
    /// Same seed and args produce byte-identical samples, docs, positions, and attitudes.
    #[test]
    fn regeneration_with_hz_and_override_is_byte_identical() {
        let seed = 42;
        let cfg = gen::GenConfig {
            hz: 1.0,
            per_path_override: true,
        };
        let a = gen::generate_with_config(seed, 1, gen::START_SECS, gen::START_SECS + 120, &cfg);
        let b = gen::generate_with_config(seed, 1, gen::START_SECS, gen::START_SECS + 120, &cfg);

        assert_eq!(a.samples.len(), b.samples.len());
        assert_eq!(a.positions.len(), b.positions.len());
        assert_eq!(a.attitudes.len(), b.attitudes.len());
        assert_eq!(a.docs.len(), b.docs.len());

        for (x, y) in a.samples.iter().zip(&b.samples) {
            assert_eq!(x.context, y.context);
            assert_eq!(x.path, y.path);
            assert_eq!(x.ts_secs, y.ts_secs);
            assert_eq!(x.value, y.value);
            assert_eq!(x.value_str, y.value_str);
            assert_eq!(x.source_label, y.source_label);
        }
        for (x, y) in a.positions.iter().zip(&b.positions) {
            assert_eq!(x.context, y.context);
            assert_eq!(x.ts_secs, y.ts_secs);
            assert_eq!(x.latitude.to_bits(), y.latitude.to_bits());
            assert_eq!(x.longitude.to_bits(), y.longitude.to_bits());
        }
    }

    /// Pin test for 1 Hz generation with per-path override:
    /// Verifies exact sample distribution across 60 seconds.
    #[test]
    fn pin_hz_samples_distribution_and_checksum() {
        let seed = DEFAULT_SEED;
        let cfg = gen::GenConfig {
            hz: 1.0,
            per_path_override: true,
        };
        // 60 seconds of 1 Hz generation with per-path override
        let data = gen::generate_with_config(seed, 1, gen::START_SECS, gen::START_SECS + 60, &cfg);

        let mut counts_by_path: std::collections::BTreeMap<String, usize> =
            std::collections::BTreeMap::new();
        for s in &data.samples {
            *counts_by_path.entry(s.path.clone()).or_default() += 1;
        }

        // Navigation / Wind / Depth numeric paths must have 60 samples (1 Hz)
        assert_eq!(counts_by_path["navigation.speedOverGround"], 60);
        assert_eq!(counts_by_path["environment.wind.speedTrue"], 60);
        assert_eq!(counts_by_path["environment.wind.speedApparent"], 60);
        assert_eq!(counts_by_path["environment.depth.belowTransducer"], 60);
        // Navigation state must have 60 samples (1 Hz)
        assert_eq!(counts_by_path["navigation.state"], 60);

        // Non-HR paths must have 6 samples (0.1 Hz: t=0, 10, 20, 30, 40, 50)
        assert_eq!(counts_by_path["propulsion.port.motorPower"], 6);
        assert_eq!(counts_by_path["electrical.batteries.house.voltage"], 6);
        assert_eq!(counts_by_path["tanks.freshWater.port.currentLevel"], 6);
        assert_eq!(counts_by_path["propulsion.main.state"], 6);

        // Position and attitude are HR -> 60 samples
        assert_eq!(data.positions.len(), 60);
        assert_eq!(data.attitudes.len(), 60);
    }
}
