//! Minimal Mapbox Vector Tile encoder — just enough protobuf for point and
//! line layers: varints, zigzag deltas, MoveTo/LineTo command streams, and
//! a deduplicated key/value attribute table. Hand-rolled (the spec is a
//! page of wire format) so the tile stack stays dependency-light and every
//! byte is accountable.

pub const EXTENT: u32 = 4096;

#[derive(Clone, PartialEq, Debug)]
pub enum Value {
    Str(String),
    Int(i64),
}

#[derive(Clone, Debug)]
pub enum Geom {
    /// Rendered as MULTIPOINT (a single MoveTo with n points).
    Points(Vec<(i64, i64)>),
    Line(Vec<(i64, i64)>),
}

struct Feature {
    id: u64,
    geom: Geom,
    /// Interleaved key/value indices into the layer tables.
    tags: Vec<u32>,
}

pub struct Layer {
    name: String,
    keys: Vec<String>,
    values: Vec<Value>,
    features: Vec<Feature>,
}

impl Layer {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            keys: Vec::new(),
            values: Vec::new(),
            features: Vec::new(),
        }
    }

    pub fn add(&mut self, id: u64, geom: Geom, attrs: &[(&str, Value)]) {
        let pts = match &geom {
            Geom::Points(p) | Geom::Line(p) => p,
        };
        if pts.is_empty() || (matches!(geom, Geom::Line(_)) && pts.len() < 2) {
            return;
        }
        let mut tags = Vec::with_capacity(attrs.len() * 2);
        for (k, v) in attrs {
            let ki = match self.keys.iter().position(|x| x == k) {
                Some(i) => i,
                None => {
                    self.keys.push(k.to_string());
                    self.keys.len() - 1
                }
            };
            let vi = match self.values.iter().position(|x| x == v) {
                Some(i) => i,
                None => {
                    self.values.push(v.clone());
                    self.values.len() - 1
                }
            };
            tags.push(ki as u32);
            tags.push(vi as u32);
        }
        self.features.push(Feature { id, geom, tags });
    }

    /// Encode as a complete single-layer tile.
    pub fn encode(&self) -> Vec<u8> {
        let mut layer = Vec::new();
        field(&mut layer, 15, 0);
        varint(&mut layer, 2); // version
        bytes_field(&mut layer, 1, self.name.as_bytes());

        for f in &self.features {
            let mut feat = Vec::new();
            field(&mut feat, 1, 0);
            varint(&mut feat, f.id);

            let mut tags = Vec::new();
            for &t in &f.tags {
                varint(&mut tags, t as u64);
            }
            bytes_field(&mut feat, 2, &tags);

            let mut geom = Vec::new();
            match &f.geom {
                Geom::Points(pts) => {
                    field(&mut feat, 3, 0);
                    varint(&mut feat, 1); // GeomType::POINT
                    varint(&mut geom, 1 | ((pts.len() as u64) << 3)); // MoveTo × n
                    let (mut cx, mut cy) = (0i64, 0i64);
                    for &(x, y) in pts {
                        varint(&mut geom, zigzag(x - cx));
                        varint(&mut geom, zigzag(y - cy));
                        (cx, cy) = (x, y);
                    }
                }
                Geom::Line(pts) => {
                    let mut pts = pts.clone();
                    pts.dedup();
                    if pts.len() < 2 {
                        continue;
                    }
                    field(&mut feat, 3, 0);
                    varint(&mut feat, 2); // GeomType::LINESTRING
                    varint(&mut geom, 1 | (1 << 3)); // MoveTo × 1
                    varint(&mut geom, zigzag(pts[0].0));
                    varint(&mut geom, zigzag(pts[0].1));
                    varint(&mut geom, 2 | (((pts.len() - 1) as u64) << 3)); // LineTo × n−1
                    for w in pts.windows(2) {
                        varint(&mut geom, zigzag(w[1].0 - w[0].0));
                        varint(&mut geom, zigzag(w[1].1 - w[0].1));
                    }
                }
            }
            bytes_field(&mut feat, 4, &geom);
            bytes_field(&mut layer, 2, &feat);
        }

        for k in &self.keys {
            bytes_field(&mut layer, 3, k.as_bytes());
        }
        for v in &self.values {
            let mut val = Vec::new();
            match v {
                Value::Str(s) => bytes_field(&mut val, 1, s.as_bytes()),
                Value::Int(i) => {
                    field(&mut val, 4, 0);
                    varint(&mut val, *i as u64);
                }
            }
            bytes_field(&mut layer, 4, &val);
        }
        field(&mut layer, 5, 0);
        varint(&mut layer, EXTENT as u64);

        let mut tile = Vec::new();
        bytes_field(&mut tile, 3, &layer); // Tile.layers
        tile
    }
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
    /// key/value tables, feature tags and the complete geometry command
    /// streams — and require everything to equal the input exactly.
    #[test]
    fn geometry_and_attributes_roundtrip_exactly() {
        let mut layer = Layer::new("mix");
        layer.add(
            7,
            Geom::Line(vec![(0, 0), (100, -50), (300, 20)]),
            &[("w", Value::Int(3))],
        );
        layer.add(
            9,
            Geom::Points(vec![(2048, 1024)]),
            &[
                ("name", Value::Str("Kelford".into())),
                ("rank", Value::Int(1)),
            ],
        );
        let tile = layer.encode();

        let mut i = 0usize;
        assert_eq!(read_varint(&tile, &mut i), (3 << 3) | 2, "Tile.layers tag");
        let len = read_varint(&tile, &mut i) as usize;
        assert_eq!(i + len, tile.len(), "layer length covers the rest");

        let mut name = Vec::new();
        let mut keys: Vec<Vec<u8>> = Vec::new();
        let mut values: Vec<Value> = Vec::new();
        let mut feats: Vec<(u64, u64, Vec<u64>, Vec<(i64, i64)>)> = Vec::new();
        while i < tile.len() {
            let tag = read_varint(&tile, &mut i);
            match (tag >> 3, tag & 7) {
                (15, 0) => assert_eq!(read_varint(&tile, &mut i), 2, "MVT version"),
                (5, 0) => assert_eq!(read_varint(&tile, &mut i), EXTENT as u64),
                (1, 2) => {
                    let n = read_varint(&tile, &mut i) as usize;
                    name = tile[i..i + n].to_vec();
                    i += n;
                }
                (3, 2) => {
                    let n = read_varint(&tile, &mut i) as usize;
                    keys.push(tile[i..i + n].to_vec());
                    i += n;
                }
                (4, 2) => {
                    let end = read_varint(&tile, &mut i) as usize + i;
                    let t = read_varint(&tile, &mut i);
                    match (t >> 3, t & 7) {
                        (1, 2) => {
                            let n = read_varint(&tile, &mut i) as usize;
                            values.push(Value::Str(
                                String::from_utf8(tile[i..i + n].to_vec()).unwrap(),
                            ));
                            i += n;
                        }
                        (4, 0) => values.push(Value::Int(read_varint(&tile, &mut i) as i64)),
                        other => panic!("unexpected value field {other:?}"),
                    }
                    assert_eq!(i, end);
                }
                (2, 2) => {
                    let end = read_varint(&tile, &mut i) as usize + i;
                    let (mut id, mut gt, mut tags, mut pts) = (0, 0, Vec::new(), Vec::new());
                    while i < end {
                        let t = read_varint(&tile, &mut i);
                        match (t >> 3, t & 7) {
                            (1, 0) => id = read_varint(&tile, &mut i),
                            (3, 0) => gt = read_varint(&tile, &mut i),
                            (2, 2) => {
                                let e = read_varint(&tile, &mut i) as usize + i;
                                while i < e {
                                    tags.push(read_varint(&tile, &mut i));
                                }
                            }
                            (4, 2) => {
                                let e = read_varint(&tile, &mut i) as usize + i;
                                let (mut cx, mut cy) = (0i64, 0i64);
                                while i < e {
                                    let cmd = read_varint(&tile, &mut i);
                                    assert!(matches!(cmd & 7, 1 | 2), "unexpected command");
                                    for _ in 0..(cmd >> 3) {
                                        cx += unzigzag(read_varint(&tile, &mut i));
                                        cy += unzigzag(read_varint(&tile, &mut i));
                                        pts.push((cx, cy));
                                    }
                                }
                            }
                            other => panic!("unexpected feature field {other:?}"),
                        }
                    }
                    feats.push((id, gt, tags, pts));
                }
                other => panic!("unexpected layer field {other:?}"),
            }
        }

        assert_eq!(name, b"mix");
        assert_eq!(keys, vec![b"w".to_vec(), b"name".to_vec(), b"rank".to_vec()]);
        assert_eq!(
            values,
            vec![
                Value::Int(3),
                Value::Str("Kelford".into()),
                Value::Int(1)
            ]
        );
        assert_eq!(feats.len(), 2);
        assert_eq!(
            feats[0],
            (7, 2, vec![0, 0], vec![(0, 0), (100, -50), (300, 20)])
        );
        assert_eq!(feats[1], (9, 1, vec![1, 1, 2, 2], vec![(2048, 1024)]));
    }
}
