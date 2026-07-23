//! PMTiles v3 archive writer — a baked world in a single file. Hand-rolled
//! like the MVT encoder: the format is a 127-byte header, varint-encoded
//! directories over Hilbert-ordered tile ids, and a data section. We write
//! with no internal or tile compression (PNGs are already compressed and
//! the spec allows it), cluster tiles in id order, deduplicate identical
//! contents, and run-length-encode runs of the same tile — which collapses
//! an ocean of empty vector tiles to a single entry.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::io::Write;

/// Hilbert-curve tile id per the PMTiles spec: all zooms concatenated,
/// each zoom's tiles in Hilbert order.
pub fn tile_id(z: u32, x: u32, y: u32) -> u64 {
    // Sum of 4^k for k < z.
    let base: u64 = ((1u64 << (2 * z)) - 1) / 3;
    let n = 1u64 << z;
    let (mut tx, mut ty) = (x as u64, y as u64);
    let mut d = 0u64;
    let mut s = n / 2;
    while s > 0 {
        let rx = u64::from(tx & s > 0);
        let ry = u64::from(ty & s > 0);
        d += s * s * ((3 * rx) ^ ry);
        // Rotate the quadrant. Wrapping subtraction is deliberate: only the
        // bits below `s` matter to later iterations, and they stay correct
        // modulo the mask even when the reflection "underflows".
        if ry == 0 {
            if rx == 1 {
                tx = s.wrapping_sub(1).wrapping_sub(tx);
                ty = s.wrapping_sub(1).wrapping_sub(ty);
            }
            std::mem::swap(&mut tx, &mut ty);
        }
        s /= 2;
    }
    base + d
}

struct Entry {
    id: u64,
    offset: u64,
    length: u32,
    run: u32,
}

pub struct TileType;
impl TileType {
    pub const MVT: u8 = 1;
    pub const PNG: u8 = 2;
}

/// Accumulates tiles (which must arrive in ascending id order) and writes
/// the archive.
pub struct Archive {
    entries: Vec<Entry>,
    data: Vec<u8>,
    seen: HashMap<u64, (u64, u32)>,
    addressed: u64,
    contents: u64,
}

impl Default for Archive {
    fn default() -> Self {
        Self::new()
    }
}

impl Archive {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            data: Vec::new(),
            seen: HashMap::new(),
            addressed: 0,
            contents: 0,
        }
    }

    pub fn add(&mut self, id: u64, bytes: &[u8]) {
        assert!(
            self.entries.last().map_or(true, |e| id >= e.id + e.run as u64),
            "tiles must be added in ascending id order"
        );
        self.addressed += 1;
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        bytes.hash(&mut hasher);
        let key = hasher.finish();

        if let Some(last) = self.entries.last_mut() {
            let (off, len) = self.seen.get(&key).copied().unwrap_or((u64::MAX, 0));
            // Extend a run of identical, id-contiguous tiles.
            if last.offset == off && last.length == len && id == last.id + last.run as u64 {
                last.run += 1;
                return;
            }
        }
        let (offset, length) = *self.seen.entry(key).or_insert_with(|| {
            let off = self.data.len() as u64;
            self.data.extend_from_slice(bytes);
            self.contents += 1;
            (off, bytes.len() as u32)
        });
        self.entries.push(Entry {
            id,
            offset,
            length,
            run: 1,
        });
    }

    pub fn finish(
        self,
        path: &std::path::Path,
        tile_type: u8,
        min_zoom: u8,
        max_zoom: u8,
        metadata_json: &str,
    ) -> std::io::Result<(u64, u64, u64)> {
        // Root directory, splitting into leaves if the root would be large.
        let root_bytes;
        let mut leaf_bytes = Vec::new();
        let serialized = serialize_dir(&self.entries);
        if serialized.len() <= 16_384 || self.entries.len() <= 256 {
            root_bytes = serialized;
        } else {
            let mut root_entries = Vec::new();
            for chunk in self.entries.chunks(2048) {
                let leaf = serialize_dir(chunk);
                root_entries.push(Entry {
                    id: chunk[0].id,
                    offset: leaf_bytes.len() as u64,
                    length: leaf.len() as u32,
                    run: 0, // run of zero marks a leaf pointer
                });
                leaf_bytes.extend_from_slice(&leaf);
            }
            root_bytes = serialize_dir(&root_entries);
        }

        let meta = metadata_json.as_bytes();
        let root_off = 127u64;
        let meta_off = root_off + root_bytes.len() as u64;
        let leaf_off = meta_off + meta.len() as u64;
        let data_off = leaf_off + leaf_bytes.len() as u64;

        let mut header = Vec::with_capacity(127);
        header.extend_from_slice(b"PMTiles");
        header.push(3);
        for v in [
            root_off,
            root_bytes.len() as u64,
            meta_off,
            meta.len() as u64,
            leaf_off,
            leaf_bytes.len() as u64,
            data_off,
            self.data.len() as u64,
            self.addressed,
            self.entries.len() as u64,
            self.contents,
        ] {
            header.extend_from_slice(&v.to_le_bytes());
        }
        header.push(1); // clustered
        header.push(1); // internal compression: none
        header.push(1); // tile compression: none
        header.push(tile_type);
        header.push(min_zoom);
        header.push(max_zoom);
        for v in [-180_0000000i32, -85_0000000, 180_0000000, 85_0000000] {
            header.extend_from_slice(&v.to_le_bytes());
        }
        header.push(min_zoom); // center zoom
        for v in [0i32, 15_0000000] {
            header.extend_from_slice(&v.to_le_bytes());
        }
        assert_eq!(header.len(), 127);

        let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
        f.write_all(&header)?;
        f.write_all(&root_bytes)?;
        f.write_all(meta)?;
        f.write_all(&leaf_bytes)?;
        f.write_all(&self.data)?;
        f.flush()?;
        Ok((self.addressed, self.contents, data_off + self.data.len() as u64))
    }
}

fn varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

/// Spec directory layout: entry count, then delta-encoded ids, then run
/// lengths, then lengths, then offsets (0 = contiguous with the previous
/// entry, else offset + 1).
fn serialize_dir(entries: &[Entry]) -> Vec<u8> {
    let mut out = Vec::new();
    varint(&mut out, entries.len() as u64);
    let mut last = 0u64;
    for e in entries {
        varint(&mut out, e.id - last);
        last = e.id;
    }
    for e in entries {
        varint(&mut out, e.run as u64);
    }
    for e in entries {
        varint(&mut out, e.length as u64);
    }
    for (i, e) in entries.iter().enumerate() {
        if i > 0
            && entries[i - 1].run > 0
            && e.offset == entries[i - 1].offset + entries[i - 1].length as u64
        {
            varint(&mut out, 0);
        } else {
            varint(&mut out, e.offset + 1);
        }
    }
    out
}

#[cfg(test)]
pub mod reader {
    //! A minimal decoder, existing so the tests can round-trip our own
    //! archives rather than trusting the writer to grade its homework.

    pub struct Parsed {
        pub root: Vec<(u64, u32, u64, u32)>, // (id, run, offset, length)
        pub leaf_off: u64,
        pub data_off: u64,
        pub tile_type: u8,
        pub clustered: bool,
        pub bytes: Vec<u8>,
    }

    pub fn parse(bytes: Vec<u8>) -> Parsed {
        assert_eq!(&bytes[0..7], b"PMTiles");
        assert_eq!(bytes[7], 3);
        let u64at = |o: usize| u64::from_le_bytes(bytes[o..o + 8].try_into().unwrap());
        let (root_off, root_len) = (u64at(8), u64at(16));
        let leaf_off = u64at(40);
        let data_off = u64at(56);
        let root = decode_dir(&bytes[root_off as usize..(root_off + root_len) as usize]);
        Parsed {
            root,
            leaf_off,
            data_off,
            tile_type: bytes[99],
            clustered: bytes[96] == 1,
            bytes,
        }
    }

    pub fn decode_dir(buf: &[u8]) -> Vec<(u64, u32, u64, u32)> {
        let mut i = 0usize;
        let mut read = || {
            let mut v = 0u64;
            let mut s = 0;
            loop {
                let b = buf[i];
                i += 1;
                v |= ((b & 0x7F) as u64) << s;
                if b < 0x80 {
                    return v;
                }
                s += 7;
            }
        };
        let n = read() as usize;
        let mut ids = Vec::with_capacity(n);
        let mut last = 0u64;
        for _ in 0..n {
            last += read();
            ids.push(last);
        }
        let runs: Vec<u64> = (0..n).map(|_| read()).collect();
        let lens: Vec<u64> = (0..n).map(|_| read()).collect();
        let mut out = Vec::with_capacity(n);
        let mut prev_off = 0u64;
        let mut prev_len = 0u64;
        for k in 0..n {
            let raw = read();
            let off = if raw == 0 { prev_off + prev_len } else { raw - 1 };
            prev_off = off;
            prev_len = lens[k];
            out.push((ids[k], runs[k] as u32, off, lens[k] as u32));
        }
        out
    }

    /// Resolve a tile id to its bytes, following one level of leaves.
    pub fn get(p: &Parsed, id: u64) -> Option<Vec<u8>> {
        let find = |dir: &[(u64, u32, u64, u32)], id: u64| -> Option<(u64, u32, u64, u32)> {
            let mut best = None;
            for &e in dir {
                if e.0 <= id {
                    best = Some(e);
                } else {
                    break;
                }
            }
            best
        };
        let e = find(&p.root, id)?;
        let e = if e.1 == 0 {
            // Leaf pointer.
            let leaf = decode_dir(
                &p.bytes[(p.leaf_off + e.2) as usize..(p.leaf_off + e.2 + e.3 as u64) as usize],
            );
            find(&leaf, id)?
        } else {
            e
        };
        // Run coverage check.
        if e.1 > 0 && id >= e.0 + e.1 as u64 {
            return None;
        }
        let start = (p.data_off + e.2) as usize;
        Some(p.bytes[start..start + e.3 as usize].to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tile_ids_match_the_spec_vectors() {
        // Test vectors from the PMTiles v3 specification.
        assert_eq!(tile_id(0, 0, 0), 0);
        assert_eq!(tile_id(1, 0, 0), 1);
        assert_eq!(tile_id(1, 0, 1), 2);
        assert_eq!(tile_id(1, 1, 1), 3);
        assert_eq!(tile_id(1, 1, 0), 4);
        assert_eq!(tile_id(2, 0, 0), 5);
        // Ids are unique and dense per zoom.
        let mut ids: Vec<u64> = (0..8u32)
            .flat_map(|y| (0..8u32).map(move |x| tile_id(3, x, y)))
            .collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), 64);
        assert_eq!(*ids.first().unwrap(), 21); // base of z3 = (4^3-1)/3
        assert_eq!(*ids.last().unwrap(), 21 + 63);
    }

    /// Independent Hilbert inverse for the round-trip check.
    fn d2xy(z: u32, mut t: u64) -> (u32, u32) {
        let n = 1u64 << z;
        let (mut x, mut y) = (0u64, 0u64);
        let mut s = 1u64;
        while s < n {
            let rx = 1 & (t / 2);
            let ry = 1 & (t ^ rx);
            if ry == 0 {
                if rx == 1 {
                    x = s - 1 - x;
                    y = s - 1 - y;
                }
                std::mem::swap(&mut x, &mut y);
            }
            x += s * rx;
            y += s * ry;
            t /= 4;
            s *= 2;
        }
        (x as u32, y as u32)
    }

    #[test]
    fn hilbert_walks_like_a_hilbert_curve() {
        // Round-trip through an independent inverse, and verify the curve
        // property external readers rely on: consecutive ids are adjacent.
        let z = 5;
        let base = ((1u64 << (2 * z)) - 1) / 3;
        let mut prev = d2xy(z, 0);
        assert_eq!(tile_id(z, prev.0, prev.1), base);
        for d in 1..(1u64 << (2 * z)) {
            let (x, y) = d2xy(z, d);
            assert_eq!(tile_id(z, x, y), base + d, "xy2d must invert d2xy");
            let step = (x as i64 - prev.0 as i64).abs() + (y as i64 - prev.1 as i64).abs();
            assert_eq!(step, 1, "consecutive ids must be neighbors");
            prev = (x, y);
        }
    }

    #[test]
    fn archives_roundtrip_with_dedup_and_runs() {
        let mut a = Archive::new();
        let blank = vec![7u8; 40];
        // z0 unique tile, then a z1 run of blanks, then a unique z1 tile.
        a.add(tile_id(0, 0, 0), b"root-tile");
        a.add(tile_id(1, 0, 0), &blank);
        a.add(tile_id(1, 0, 1), &blank);
        a.add(tile_id(1, 1, 1), &blank);
        a.add(tile_id(1, 1, 0), b"odd-one-out");

        let path = std::env::temp_dir().join(format!(
            "fractal-pmtiles-test-{}.pmtiles",
            std::process::id()
        ));
        let (addressed, contents, _) = a
            .finish(&path, TileType::PNG, 0, 1, r#"{"name":"test"}"#)
            .unwrap();
        assert_eq!(addressed, 5);
        assert_eq!(contents, 3, "blank tiles must share one content");

        let p = reader::parse(std::fs::read(&path).unwrap());
        assert!(p.clustered);
        assert_eq!(p.tile_type, TileType::PNG);
        assert_eq!(reader::get(&p, tile_id(0, 0, 0)).unwrap(), b"root-tile");
        for (x, y) in [(0, 0), (0, 1), (1, 1)] {
            assert_eq!(reader::get(&p, tile_id(1, x, y)).unwrap(), blank);
        }
        assert_eq!(reader::get(&p, tile_id(1, 1, 0)).unwrap(), b"odd-one-out");
        assert!(reader::get(&p, tile_id(2, 3, 3)).is_none(), "absent tile");
        let _ = std::fs::remove_file(&path);
    }
}
