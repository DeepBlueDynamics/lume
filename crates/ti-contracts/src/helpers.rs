use crate::{BucketIx, ColumnId, Error, Result, ShardKey, VesselOrd};

/// 2020-01-01T00:00:00Z, in Unix seconds.
pub const EPOCH: i64 = 1_577_836_800;

/// Compute a bucket from Unix seconds and a positive whole-second width.
/// Rejects timestamps before EPOCH and buckets outside the u32 column space.
pub fn bucket_of(timestamp: i64, width: u64) -> Result<BucketIx> {
    if width == 0 {
        return Err(Error::InvalidInput("bucket width must be positive".into()));
    }
    let elapsed = (timestamp as i128) - (EPOCH as i128);
    if elapsed < 0 {
        return Err(Error::InvalidInput("timestamp precedes EPOCH".into()));
    }
    u32::try_from(elapsed / (width as i128)).map_err(|_| Error::Overflow("bucket index"))
}

/// Encode a vessel and bucket without losing either 32-bit component.
pub const fn column_id(vessel: VesselOrd, bucket: BucketIx) -> ColumnId {
    ((vessel as u64) << 32) | (bucket as u64)
}

/// Address the vessel's group of 65,536 consecutive buckets.
pub const fn shard_key(vessel: VesselOrd, bucket: BucketIx) -> ShardKey {
    ShardKey {
        vessel,
        shard: bucket >> 16,
    }
}

/// Return a local column in 0..=65,535.
pub const fn local_col(bucket: BucketIx) -> u32 {
    bucket & 0xffff
}

fn factor(scale: u8) -> Result<f64> {
    if scale > 18 {
        return Err(Error::InvalidInput(
            "fixed-point scale must be in 0..=18".into(),
        ));
    }
    Ok(10_f64.powi(i32::from(scale)))
}

/// Round a finite physical value to a signed fixed-point integer.
/// Halfway ties round away from zero. Rejects unsupported scales and overflow.
pub fn to_fixed(value: f64, scale: u8) -> Result<i64> {
    let factor = factor(scale)?;
    if !value.is_finite() {
        return Err(Error::InvalidInput(
            "fixed-point value must be finite".into(),
        ));
    }
    let rounded = (value * factor).round();
    // i64::MAX as f64 rounds to 2^63, which is already out of range.
    if !rounded.is_finite()
        || !(-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0).contains(&rounded)
    {
        return Err(Error::Overflow("fixed-point integer"));
    }
    Ok(rounded as i64)
}

/// Convert a fixed-point integer to physical units.
/// f64 may approximate integers whose magnitude exceeds 2^53.
pub fn from_fixed(value: i64, scale: u8) -> Result<f64> {
    Ok((value as f64) / factor(scale)?)
}

/// Maximum quantization error in physical units at this scale (half a unit).
/// This is not a floating-point representation error budget.
pub fn tolerance(scale: u8) -> Result<f64> {
    Ok(0.5 / factor(scale)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_and_bucket_edges() {
        assert_eq!(bucket_of(EPOCH, 10).unwrap(), 0);
        assert_eq!(bucket_of(EPOCH + 9, 10).unwrap(), 0);
        assert_eq!(bucket_of(EPOCH + 10, 10).unwrap(), 1);
        assert!(bucket_of(EPOCH - 1, 10).is_err());
        assert!(bucket_of(i64::MIN, 10).is_err());
        assert!(bucket_of(EPOCH, 0).is_err());
        let last = EPOCH + i64::from(u32::MAX) * 10;
        assert_eq!(bucket_of(last + 9, 10).unwrap(), u32::MAX);
        assert!(bucket_of(last + 10, 10).is_err());
        assert!(bucket_of(i64::MAX, 1).is_err());
        assert_eq!(bucket_of(i64::MAX, u64::MAX).unwrap(), 0);
    }

    #[test]
    fn column_and_shard_edges() {
        assert_eq!(column_id(0, 0), 0);
        assert_eq!(column_id(u32::MAX, u32::MAX), u64::MAX);
        for b in [0, 65535, 65536, 65537, u32::MAX] {
            let key = shard_key(42, b);
            assert_eq!(key.vessel, 42);
            assert_eq!((key.shard << 16) | local_col(b), b);
            assert!(local_col(b) <= 65535);
        }
        assert_eq!(shard_key(42, 65535).shard, 0);
        assert_eq!(shard_key(42, 65536).shard, 1);
        assert_eq!(local_col(u32::MAX), 65535);
    }

    #[test]
    fn signed_rounding_and_tolerance() {
        assert_eq!(to_fixed(1.25, 1).unwrap(), 13);
        assert_eq!(to_fixed(-1.25, 1).unwrap(), -13);
        assert_eq!(to_fixed(-0.0, 3).unwrap(), 0);
        assert_eq!(to_fixed(0.5, 0).unwrap(), 1);
        assert_eq!(to_fixed(-0.5, 0).unwrap(), -1);
        for scale in 0..=18 {
            let v = from_fixed(-12345, scale).unwrap();
            assert_eq!(to_fixed(v, scale).unwrap(), -12345);
            assert!(tolerance(scale).unwrap() > 0.0);
        }
        let tol = tolerance(3).unwrap();
        for value in [-12.3454, -0.0004, 0.0004, 12.3454] {
            let actual = from_fixed(to_fixed(value, 3).unwrap(), 3).unwrap();
            assert!((actual - value).abs() <= tol);
        }
    }

    #[test]
    fn reject_nonfinite_overflow_and_invalid_scale() {
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(matches!(to_fixed(value, 3), Err(Error::InvalidInput(_))));
        }
        for value in [
            f64::MAX,
            9_223_372_036_854_775_808.0,
            -18_446_744_073_709_551_616.0,
        ] {
            assert!(matches!(to_fixed(value, 0), Err(Error::Overflow(_))));
        }
        assert_eq!(to_fixed(-9_223_372_036_854_775_808.0, 0).unwrap(), i64::MIN);
        assert!(to_fixed(1.0, 19).is_err());
        assert!(from_fixed(1, 19).is_err());
        assert!(tolerance(19).is_err());
    }
}
