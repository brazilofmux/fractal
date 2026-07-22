//! Positional hashing. Every random value in the world is a pure function of
//! (seed, coordinates) — no RNG state exists anywhere, which is what makes
//! lazy, out-of-order, parallel generation of any tile at any zoom possible.

#[inline]
pub fn splitmix64(x: u64) -> u64 {
    let mut z = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

#[inline]
pub fn hash3(seed: u64, x: i64, y: i64, z: i64) -> u64 {
    let mut h = splitmix64(seed);
    h = splitmix64(h ^ x as u64);
    h = splitmix64(h ^ y as u64);
    splitmix64(h ^ z as u64)
}
