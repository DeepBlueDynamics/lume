use std::collections::HashMap;
use serde::{Serialize, Deserialize};

// ─── MiniRoaring Bitmap ──────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Container {
    Array(Vec<u16>),
    Bitmap(Box<[u64; 1024]>),
}

impl Serialize for Container {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Container::Array(v) => serializer.serialize_newtype_variant("Container", 0, "Array", v),
            Container::Bitmap(b) => {
                let vec: Vec<u64> = b.iter().cloned().collect();
                serializer.serialize_newtype_variant("Container", 1, "Bitmap", &vec)
            }
        }
    }
}

impl<'de> Deserialize<'de> for Container {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        enum Helper {
            Array(Vec<u16>),
            Bitmap(Vec<u64>),
        }
        let helper = Helper::deserialize(deserializer)?;
        match helper {
            Helper::Array(v) => Ok(Container::Array(v)),
            Helper::Bitmap(v) => {
                let mut arr = [0u64; 1024];
                if v.len() == 1024 {
                    arr.copy_from_slice(&v);
                } else {
                    for (i, &x) in v.iter().enumerate().take(1024) {
                        arr[i] = x;
                    }
                }
                Ok(Container::Bitmap(Box::new(arr)))
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MiniRoaring {
    pub containers: HashMap<u16, Container>,
}

impl Default for MiniRoaring {
    fn default() -> Self {
        Self::new()
    }
}

impl MiniRoaring {
    pub fn new() -> Self {
        Self {
            containers: HashMap::new(),
        }
    }

    /// Insert a 32-bit ID into the roaring bitmap
    pub fn insert(&mut self, id: u32) {
        let key = (id >> 16) as u16;
        let value = (id & 0xFFFF) as u16;

        let container = self.containers.entry(key).or_insert_with(|| Container::Array(Vec::new()));

        match container {
            Container::Array(ref mut arr) => {
                if let Err(idx) = arr.binary_search(&value) {
                    arr.insert(idx, value);
                    // If sparse vector grows beyond 1024 elements, upgrade to a packed bitset
                    if arr.len() > 1024 {
                        let mut bitmap = Box::new([0u64; 1024]);
                        for &v in arr.iter() {
                            let idx = (v >> 6) as usize;
                            let bit = (v & 63) as u64;
                            bitmap[idx] |= 1 << bit;
                        }
                        *container = Container::Bitmap(bitmap);
                    }
                }
            }
            Container::Bitmap(ref mut bitmap) => {
                let idx = (value >> 6) as usize;
                let bit = (value & 63) as u64;
                bitmap[idx] |= 1 << bit;
            }
        }
    }

    /// Check if the 32-bit ID is contained in the bitmap
    pub fn contains(&self, id: u32) -> bool {
        let key = (id >> 16) as u16;
        let value = (id & 0xFFFF) as u16;

        match self.containers.get(&key) {
            Some(Container::Array(arr)) => arr.binary_search(&value).is_ok(),
            Some(Container::Bitmap(bitmap)) => {
                let idx = (value >> 6) as usize;
                let bit = (value & 63) as u64;
                (bitmap[idx] & (1 << bit)) != 0
            }
            None => false,
        }
    }

    /// Intersects two roaring bitmaps to yield a new intersected bitmap
    pub fn intersect(&self, other: &Self) -> Self {
        let mut containers = HashMap::new();

        for (key, self_c) in &self.containers {
            if let Some(other_c) = other.containers.get(key) {
                let intersection = match (self_c, other_c) {
                    (Container::Array(a), Container::Array(b)) => {
                        let mut res = Vec::new();
                        let (mut i, mut j) = (0, 0);
                        while i < a.len() && j < b.len() {
                            if a[i] == b[j] {
                                res.push(a[i]);
                                i += 1;
                                j += 1;
                            } else if a[i] < b[j] {
                                i += 1;
                            } else {
                                j += 1;
                            }
                        }
                        if !res.is_empty() {
                            Some(Container::Array(res))
                        } else {
                            None
                        }
                    }
                    (Container::Bitmap(a), Container::Bitmap(b)) => {
                        let mut bitmap = Box::new([0u64; 1024]);
                        let mut empty = true;
                        for i in 0..1024 {
                            bitmap[i] = a[i] & b[i];
                            if bitmap[i] != 0 {
                                empty = false;
                            }
                        }
                        if !empty {
                            Some(Container::Bitmap(bitmap))
                        } else {
                            None
                        }
                    }
                    (Container::Array(arr), Container::Bitmap(bitmap)) |
                    (Container::Bitmap(bitmap), Container::Array(arr)) => {
                        let mut res = Vec::new();
                        for &v in arr {
                            let idx = (v >> 6) as usize;
                            let bit = (v & 63) as u64;
                            if (bitmap[idx] & (1 << bit)) != 0 {
                                res.push(v);
                            }
                        }
                        if !res.is_empty() {
                            Some(Container::Array(res))
                        } else {
                            None
                        }
                    }
                };

                if let Some(c) = intersection {
                    containers.insert(*key, c);
                }
            }
        }

        Self { containers }
    }

    /// Unions two roaring bitmaps to yield a new unioned bitmap
    pub fn union(&self, other: &Self) -> Self {
        let mut containers = self.containers.clone();

        for (key, other_c) in &other.containers {
            if let Some(self_c) = containers.get_mut(key) {
                *self_c = match (&*self_c, other_c) {
                    (Container::Array(a), Container::Array(b)) => {
                        let mut res = Vec::new();
                        let (mut i, mut j) = (0, 0);
                        while i < a.len() || j < b.len() {
                            if i < a.len() && (j >= b.len() || a[i] < b[j]) {
                                res.push(a[i]);
                                i += 1;
                            } else if j < b.len() && (i >= a.len() || b[j] < a[i]) {
                                res.push(b[j]);
                                j += 1;
                            } else {
                                res.push(a[i]);
                                i += 1;
                                j += 1;
                            }
                        }
                        if res.len() > 1024 {
                            let mut bitmap = Box::new([0u64; 1024]);
                            for &v in &res {
                                let idx = (v >> 6) as usize;
                                let bit = (v & 63) as u64;
                                bitmap[idx] |= 1 << bit;
                            }
                            Container::Bitmap(bitmap)
                        } else {
                            Container::Array(res)
                        }
                    }
                    (Container::Bitmap(a), Container::Bitmap(b)) => {
                        let mut bitmap = Box::new([0u64; 1024]);
                        for i in 0..1024 {
                            bitmap[i] = a[i] | b[i];
                        }
                        Container::Bitmap(bitmap)
                    }
                    (Container::Array(arr), Container::Bitmap(bitmap)) |
                    (Container::Bitmap(bitmap), Container::Array(arr)) => {
                        let mut new_bitmap = bitmap.clone();
                        for &v in arr {
                            let idx = (v >> 6) as usize;
                            let bit = (v & 63) as u64;
                            new_bitmap[idx] |= 1 << bit;
                        }
                        Container::Bitmap(new_bitmap)
                    }
                };
            } else {
                containers.insert(*key, other_c.clone());
            }
        }

        Self { containers }
    }

    /// Extract all document IDs in sorted order
    pub fn iter(&self) -> Vec<u32> {
        let mut keys: Vec<&u16> = self.containers.keys().collect();
        keys.sort();

        let mut res = Vec::new();
        for &key in keys {
            let high = (key as u32) << 16;
            match &self.containers[&key] {
                Container::Array(arr) => {
                    for &val in arr {
                        res.push(high | (val as u32));
                    }
                }
                Container::Bitmap(bitmap) => {
                    for i in 0..1024 {
                        let word = bitmap[i];
                        if word != 0 {
                            let base = (i << 6) as u32;
                            for bit in 0..64 {
                                if (word & (1 << bit)) != 0 {
                                    res.push(high | base | bit);
                                }
                            }
                        }
                    }
                }
            }
        }
        res
    }

    pub fn is_empty(&self) -> bool {
        self.containers.is_empty()
    }

    /// Returns the total number of IDs stored in this roaring bitmap
    pub fn len(&self) -> usize {
        let mut count = 0;
        for container in self.containers.values() {
            match container {
                Container::Array(arr) => count += arr.len(),
                Container::Bitmap(bitmap) => {
                    for &word in bitmap.iter() {
                        count += word.count_ones() as usize;
                    }
                }
            }
        }
        count
    }

    /// Counts the intersection cardinality without materializing a result bitmap.
    pub fn intersection_count(&self, other: &Self) -> usize {
        let mut count = 0;
        for (key, self_c) in &self.containers {
            if let Some(other_c) = other.containers.get(key) {
                count += match (self_c, other_c) {
                    (Container::Array(a), Container::Array(b)) => {
                        let (mut i, mut j, mut c) = (0, 0, 0);
                        while i < a.len() && j < b.len() {
                            match a[i].cmp(&b[j]) {
                                std::cmp::Ordering::Equal => {
                                    c += 1;
                                    i += 1;
                                    j += 1;
                                }
                                std::cmp::Ordering::Less => i += 1,
                                std::cmp::Ordering::Greater => j += 1,
                            }
                        }
                        c
                    }
                    (Container::Bitmap(a), Container::Bitmap(b)) => {
                        let mut c = 0;
                        for i in 0..1024 {
                            c += (a[i] & b[i]).count_ones() as usize;
                        }
                        c
                    }
                    (Container::Array(arr), Container::Bitmap(bitmap))
                    | (Container::Bitmap(bitmap), Container::Array(arr)) => arr
                        .iter()
                        .filter(|&&v| (bitmap[(v >> 6) as usize] & (1 << (v & 63))) != 0)
                        .count(),
                };
            }
        }
        count
    }

    /// Computes the Jaccard similarity index (intersection size / union size) between two roaring bitmaps.
    /// The union size is derived by inclusion-exclusion, so no result bitmap is ever allocated.
    pub fn jaccard_similarity(&self, other: &Self) -> f64 {
        let intersection_count = self.intersection_count(other);
        let union_count = self.len() + other.len() - intersection_count;
        if union_count == 0 {
            0.0
        } else {
            intersection_count as f64 / union_count as f64
        }
    }
}

// ─── Prime Partitioned Gödel Filter ─────────────────────────────────────

/// Helper to check if a number is prime.
pub fn is_prime(n: u64) -> bool {
    if n <= 1 { return false; }
    if n <= 3 { return true; }
    if n.is_multiple_of(2) || n.is_multiple_of(3) { return false; }
    let mut i = 5;
    while i * i <= n {
        if n.is_multiple_of(i) || n.is_multiple_of(i + 2) {
            return false;
        }
        i += 6;
    }
    true
}

/// Helper to get the n-th prime number (1-indexed, so 1st prime is 2, 2nd is 3, etc.)
pub fn get_nth_prime(n: usize) -> u128 {
    let mut count = 0;
    let mut candidate = 1;
    while count < n {
        candidate += 1;
        if is_prime(candidate) {
            count += 1;
        }
    }
    candidate as u128
}

/// Simple fast FNV-1a 32-bit hash function
pub fn fnv1a_hash(bytes: &[u8]) -> u32 {
    let mut hash = 2166136261u32;
    for &b in bytes {
        hash ^= b as u32;
        hash = hash.wrapping_mul(16777619);
    }
    hash
}

/// A Prime Filter representation of a document's lexical terms and FST tags.
/// Uses a high-speed bitwise Bloom filter for terms, and a dynamic-prime Gödel
/// product signature for tag outputs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrimeFilter {
    // 64-bit Bloom filter mask for fast lexical term pruning
    pub term_mask: u64,
    // Perfect Gödel signature for FST tags
    pub tag_signature: u128,
}

impl Default for PrimeFilter {
    fn default() -> Self {
        Self::new()
    }
}

impl PrimeFilter {
    pub fn new() -> Self {
        Self {
            term_mask: 0,
            tag_signature: 1,
        }
    }

    /// Add a vocabulary term to the 64-bit bitwise Bloom filter mask
    pub fn add_term(&mut self, term_bytes: &[u8]) {
        let h = fnv1a_hash(term_bytes);
        let idx1 = h & 0x3F;
        let idx2 = (h >> 16) & 0x3F;
        self.term_mask |= (1u64 << idx1) | (1u64 << idx2);
    }

    /// Check if a query term is possibly present in the document.
    /// Returns false if it is definitely NOT present.
    pub fn test_term(&self, term_bytes: &[u8]) -> bool {
        let h = fnv1a_hash(term_bytes);
        let idx1 = h & 0x3F;
        let idx2 = (h >> 16) & 0x3F;
        let mask = (1u64 << idx1) | (1u64 << idx2);
        (self.term_mask & mask) == mask
    }

    /// Add a tag prime to the tag Gödel signature
    pub fn add_tag_prime(&mut self, prime: u128) {
        if self.tag_signature != 0 {
            if let Some(val) = self.tag_signature.checked_mul(prime) {
                self.tag_signature = val;
            } else {
                self.tag_signature = 0; // Saturated/Overflowed: match all
            }
        }
    }

    /// Check if a tag prime is possibly present in the document.
    pub fn test_tag_prime(&self, prime: u128) -> bool {
        self.tag_signature == 0 || self.tag_signature.is_multiple_of(prime)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_miniroaring_basic() {
        let mut bitmap = MiniRoaring::new();
        assert!(bitmap.is_empty());
        assert_eq!(bitmap.len(), 0);

        bitmap.insert(5);
        bitmap.insert(10);
        bitmap.insert(1000);
        assert!(!bitmap.is_empty());
        assert_eq!(bitmap.len(), 3);
        assert!(bitmap.contains(5));
        assert!(bitmap.contains(10));
        assert!(bitmap.contains(1000));
        assert!(!bitmap.contains(11));

        let sorted = bitmap.iter();
        assert_eq!(sorted, vec![5, 10, 1000]);
    }

    #[test]
    fn test_miniroaring_upgrade() {
        let mut bitmap = MiniRoaring::new();
        // Insert 1025 unique values to force upgrade to dense Container::Bitmap
        for i in 0..1025 {
            bitmap.insert(i * 2);
        }
        assert_eq!(bitmap.len(), 1025);
        assert!(bitmap.contains(0));
        assert!(bitmap.contains(2048));
        assert!(!bitmap.contains(1));

        let key = 0u16;
        let container = bitmap.containers.get(&key).unwrap();
        match container {
            Container::Bitmap(_) => {}
            _ => panic!("Expected Container::Bitmap, found Array"),
        }
    }

    #[test]
    fn test_miniroaring_operations() {
        let mut a = MiniRoaring::new();
        a.insert(1);
        a.insert(2);
        a.insert(3);

        let mut b = MiniRoaring::new();
        b.insert(2);
        b.insert(3);
        b.insert(4);

        let intersection = a.intersect(&b);
        assert_eq!(intersection.iter(), vec![2, 3]);
        assert_eq!(intersection.len(), 2);

        let union_set = a.union(&b);
        assert_eq!(union_set.iter(), vec![1, 2, 3, 4]);
        assert_eq!(union_set.len(), 4);

        let jaccard = a.jaccard_similarity(&b);
        // intersection / union = 2 / 4 = 0.5
        assert!((jaccard - 0.5).abs() < 1e-9);
    }

    #[test]
    fn test_intersection_count_matches_materialized() {
        // Cover all container pairings: array/array, bitmap/bitmap, array/bitmap.
        let mut sparse_a = MiniRoaring::new();
        let mut sparse_b = MiniRoaring::new();
        for i in 0..500 {
            sparse_a.insert(i * 3);
            sparse_b.insert(i * 5);
        }
        let mut dense_a = MiniRoaring::new();
        let mut dense_b = MiniRoaring::new();
        for i in 0..3000 {
            dense_a.insert(i * 2);
            dense_b.insert(i * 3);
        }
        // Spread across multiple containers (high bits differ)
        for i in 0..100 {
            sparse_a.insert(70000 + i * 7);
            dense_b.insert(70000 + i * 2);
        }

        let pairs = [
            (&sparse_a, &sparse_b),
            (&dense_a, &dense_b),
            (&sparse_a, &dense_b),
            (&dense_a, &sparse_b),
        ];
        for (a, b) in pairs {
            let materialized = a.intersect(b).len();
            assert_eq!(a.intersection_count(b), materialized);
            let union_materialized = a.union(b).len();
            assert_eq!(a.len() + b.len() - materialized, union_materialized);
            let expected_jaccard = if union_materialized == 0 {
                0.0
            } else {
                materialized as f64 / union_materialized as f64
            };
            assert!((a.jaccard_similarity(b) - expected_jaccard).abs() < 1e-12);
        }

        // Disjoint and empty edge cases
        let empty = MiniRoaring::new();
        assert_eq!(empty.intersection_count(&sparse_a), 0);
        assert_eq!(empty.jaccard_similarity(&empty), 0.0);
    }

    #[test]
    fn test_prime_filter() {
        let mut filter = PrimeFilter::new();
        
        filter.add_term(b"apple");
        filter.add_term(b"banana");
        
        assert!(filter.test_term(b"apple"));
        assert!(filter.test_term(b"banana"));
        
        filter.add_tag_prime(2);
        filter.add_tag_prime(3);
        assert!(filter.test_tag_prime(2));
        assert!(filter.test_tag_prime(3));
        assert!(!filter.test_tag_prime(5));

        // Test overflow robustness: verify that adding many terms degrades to true (match all) but never false negatives
        let mut overflow_filter = PrimeFilter::new();
        for i in 0..1000 {
            let term = format!("term_{}", i);
            overflow_filter.add_term(term.as_bytes());
        }
        // Verify all added terms still evaluate to true
        for i in 0..1000 {
            let term = format!("term_{}", i);
            assert!(overflow_filter.test_term(term.as_bytes()), "Term term_{} had false negative after overflow!", i);
        }
    }
}
