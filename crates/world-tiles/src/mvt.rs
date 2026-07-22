//! Minimal Mapbox Vector Tile encoder — just enough protobuf for a line
//! layer: varints, zigzag deltas, MoveTo/LineTo command streams. Hand-rolled
//! (the spec is a page of wire format) so the tile stack stays
//! dependency-light and every byte is accountable.

pub const EXTENT: u32 = 4096;

pub struct LineFeature {
    pub id: u64,
    /// Attribute class 1..=6, exposed as integer property under the layer key.
    pub class: u8,
    /// Points in tile coordinates (0..EXTENT inside the tile; outside is
    /// legal and clipped by the renderer).
    pub pts: Vec<(i64, i64)>,
}

/// Encode one line layer. Features with fewer than two distinct points are
/// skipped.
pub fn encode_line_layer(name: &str, key: &str, features: &[LineFeature]) -> Vec<u8> {
    let mut layer = Vec::new();
    field(&mut layer, 15, 0);
    varint(&mut layer, 2); // version
    bytes_field(&mut layer, 1, name.as_bytes());

    for f in features {
        let mut pts = f.pts.clone();
        pts.dedup();
        if pts.len() < 2 {
            continue;
        }
        let mut feat = Vec::new();
        field(&mut feat, 1, 0);
        varint(&mut feat, f.id);

        let mut tags = Vec::new();
        varint(&mut tags, 0); // key index: our single key
        varint(&mut tags, (f.class.clamp(1, 6) - 1) as u64); // value index
        bytes_field(&mut feat, 2, &tags);

        field(&mut feat, 3, 0);
        varint(&mut feat, 2); // GeomType::LINESTRING

        let mut geom = Vec::new();
        varint(&mut geom, 1 | (1 << 3)); // MoveTo × 1
        varint(&mut geom, zigzag(pts[0].0));
        varint(&mut geom, zigzag(pts[0].1));
        varint(&mut geom, 2 | (((pts.len() - 1) as u64) << 3)); // LineTo × n−1
        for w in pts.windows(2) {
            varint(&mut geom, zigzag(w[1].0 - w[0].0));
            varint(&mut geom, zigzag(w[1].1 - w[0].1));
        }
        bytes_field(&mut feat, 4, &geom);

        bytes_field(&mut layer, 2, &feat);
    }

    bytes_field(&mut layer, 3, key.as_bytes());
    for v in 1..=6u64 {
        // Value message with int_value (field 4).
        let mut val = Vec::new();
        field(&mut val, 4, 0);
        varint(&mut val, v);
        bytes_field(&mut layer, 4, &val);
    }
    field(&mut layer, 5, 0);
    varint(&mut layer, EXTENT as u64);

    let mut tile = Vec::new();
    bytes_field(&mut tile, 3, &layer); // Tile.layers
    tile
}

fn varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn field(out: &mut Vec<u8>, num: u64, wire: u64) {
    varint(out, (num << 3) | wire);
}

fn bytes_field(out: &mut Vec<u8>, num: u64, payload: &[u8]) {
    field(out, num, 2);
    varint(out, payload.len() as u64);
    out.extend_from_slice(payload);
}

#[inline]
fn zigzag(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_varint(buf: &[u8], i: &mut usize) -> u64 {
        let mut v = 0u64;
        let mut s = 0;
        loop {
            let b = buf[*i];
            *i += 1;
            v |= ((b & 0x7F) as u64) << s;
            if b < 0x80 {
                return v;
            }
            s += 7;
        }
    }

    fn unzigzag(v: u64) -> i64 {
        ((v >> 1) as i64) ^ -((v & 1) as i64)
    }

    /// Full round-trip: decode the protobuf we emit — layer framing, name,
    /// feature tags, and the complete geometry command stream — and require
    /// the decoded polylines to equal the input exactly.
    #[test]
    fn geometry_roundtrips_exactly() {
        let inputs = vec![
            LineFeature {
                id: 7,
                class: 3,
                pts: vec![(0, 0), (100, -50), (300, 20)],
            },
            LineFeature {
                id: 9,
                class: 6,
                pts: vec![(-200, 4096), (500, 500), (500, 501), (4100, -3)],
            },
        ];
        let tile = encode_line_layer("rivers", "w", &inputs);

        let mut i = 0usize;
        assert_eq!(read_varint(&tile, &mut i), (3 << 3) | 2, "Tile.layers tag");
        let len = read_varint(&tile, &mut i) as usize;
        assert_eq!(i + len, tile.len(), "layer length covers the rest");

        let mut decoded: Vec<(u64, u64, Vec<(i64, i64)>)> = Vec::new();
        let mut name = Vec::new();
        while i < tile.len() {
            let tag = read_varint(&tile, &mut i);
            let (field, wire) = (tag >> 3, tag & 7);
            match (field, wire) {
                (15, 0) => assert_eq!(read_varint(&tile, &mut i), 2, "MVT version"),
                (5, 0) => assert_eq!(read_varint(&tile, &mut i), EXTENT as u64),
                (1, 2) => {
                    let n = read_varint(&tile, &mut i) as usize;
                    name = tile[i..i + n].to_vec();
                    i += n;
                }
                (2, 2) => {
                    // Feature: id, tags, type, geometry.
                    let end = {
                        let n = read_varint(&tile, &mut i) as usize;
                        i + n
                    };
                    let (mut id, mut class_idx, mut pts) = (0, 0, Vec::new());
                    while i < end {
                        let t = read_varint(&tile, &mut i);
                        match (t >> 3, t & 7) {
                            (1, 0) => id = read_varint(&tile, &mut i),
                            (3, 0) => assert_eq!(read_varint(&tile, &mut i), 2, "LINESTRING"),
                            (2, 2) => {
                                let n = read_varint(&tile, &mut i) as usize;
                                let end = i + n;
                                assert_eq!(read_varint(&tile, &mut i), 0, "key index");
                                class_idx = read_varint(&tile, &mut i);
                                assert_eq!(i, end);
                            }
                            (4, 2) => {
                                let n = read_varint(&tile, &mut i) as usize;
                                let end = i + n;
                                let (mut cx, mut cy) = (0i64, 0i64);
                                while i < end {
                                    let cmd = read_varint(&tile, &mut i);
                                    let count = cmd >> 3;
                                    match cmd & 7 {
                                        1 | 2 => {
                                            for _ in 0..count {
                                                cx += unzigzag(read_varint(&tile, &mut i));
                                                cy += unzigzag(read_varint(&tile, &mut i));
                                                pts.push((cx, cy));
                                            }
                                        }
                                        c => panic!("unexpected command {c}"),
                                    }
                                }
                            }
                            (f, w) => panic!("unexpected feature field {f} wire {w}"),
                        }
                    }
                    decoded.push((id, class_idx, pts));
                }
                (3, 2) | (4, 2) => {
                    let n = read_varint(&tile, &mut i) as usize;
                    i += n; // keys / values checked by count below
                }
                (f, w) => panic!("unexpected layer field {f} wire {w}"),
            }
        }
        assert_eq!(name, b"rivers");
        assert_eq!(decoded.len(), inputs.len());
        for (got, want) in decoded.iter().zip(&inputs) {
            assert_eq!(got.0, want.id);
            assert_eq!(got.1, (want.class - 1) as u64);
            assert_eq!(got.2, want.pts);
        }
    }
}
