//! Cube-sphere grid: the coarse global lattice for pipeline stages that need
//! neighbors — hydrology now, civilization later. Six gnomonic faces of N×N
//! cells: no pole singularity, near-uniform cell size, and cross-face
//! adjacency discovered *geometrically* (extend the face plane past its edge
//! and locate the resulting point) instead of via a hand-written transition
//! table, which is where cube-grid bugs traditionally live.

/// Per-face orthonormal frames: (outward normal, tangent a, tangent b).
/// Orientation per face is arbitrary but fixed — each face is its own chart.
const AXES: [[[f64; 3]; 3]; 6] = [
    [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
    [[-1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
    [[0.0, 1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]],
    [[0.0, -1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]],
    [[0.0, 0.0, 1.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
    [[0.0, 0.0, -1.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
];

pub struct CubeGrid {
    n: usize,
}

impl CubeGrid {
    pub fn new(n: usize) -> Self {
        assert!(n >= 2);
        Self { n }
    }

    pub fn cells(&self) -> usize {
        6 * self.n * self.n
    }

    /// Upper bound on the angular size of any cell (face centers, where
    /// gnomonic cells are largest).
    pub fn max_cell_size(&self) -> f64 {
        2.0 / self.n as f64
    }

    fn face_of(p: [f64; 3]) -> usize {
        let (ax, ay, az) = (p[0].abs(), p[1].abs(), p[2].abs());
        if ax >= ay && ax >= az {
            if p[0] >= 0.0 {
                0
            } else {
                1
            }
        } else if ay >= az {
            if p[1] >= 0.0 {
                2
            } else {
                3
            }
        } else if p[2] >= 0.0 {
            4
        } else {
            5
        }
    }

    /// Face index and gnomonic coordinates (a, b) ∈ [−1, 1] of a unit vector.
    pub fn face_coords(&self, p: [f64; 3]) -> (usize, f64, f64) {
        let f = Self::face_of(p);
        let [nrm, ta, tb] = AXES[f];
        let den = dot(p, nrm);
        (f, dot(p, ta) / den, dot(p, tb) / den)
    }

    pub fn point_to_cell(&self, p: [f64; 3]) -> u32 {
        let (f, a, b) = self.face_coords(p);
        let n = self.n as isize;
        let i = (((a + 1.0) * 0.5 * self.n as f64) as isize).clamp(0, n - 1) as usize;
        let j = (((b + 1.0) * 0.5 * self.n as f64) as isize).clamp(0, n - 1) as usize;
        (f * self.n * self.n + j * self.n + i) as u32
    }

    fn decompose(&self, c: u32) -> (usize, usize, usize) {
        let c = c as usize;
        let f = c / (self.n * self.n);
        let r = c % (self.n * self.n);
        (f, r % self.n, r / self.n)
    }

    /// Cell-center direction; defined for out-of-range (i, j) too, where the
    /// gnomonic plane extends naturally past the face edge — that is exactly
    /// what lets us discover cross-face neighbors without a transition table.
    fn raw_center(&self, f: usize, i: isize, j: isize) -> [f64; 3] {
        let n = self.n as f64;
        let a = (i as f64 + 0.5) * 2.0 / n - 1.0;
        let b = (j as f64 + 0.5) * 2.0 / n - 1.0;
        let [nrm, ta, tb] = AXES[f];
        normalize([
            nrm[0] + a * ta[0] + b * tb[0],
            nrm[1] + a * ta[1] + b * tb[1],
            nrm[2] + a * ta[2] + b * tb[2],
        ])
    }

    pub fn cell_center(&self, c: u32) -> [f64; 3] {
        let (f, i, j) = self.decompose(c);
        self.raw_center(f, i as isize, j as isize)
    }

    /// Geometric 8-neighborhood, crossing face edges where needed. Deduped;
    /// may be one-sided right at face seams (symmetrize before graph use).
    pub fn neighbors(&self, c: u32) -> Vec<u32> {
        let (f, i, j) = self.decompose(c);
        let n = self.n as isize;
        let mut out = Vec::with_capacity(8);
        for dj in -1isize..=1 {
            for di in -1isize..=1 {
                if di == 0 && dj == 0 {
                    continue;
                }
                let (ii, jj) = (i as isize + di, j as isize + dj);
                let nb = if (0..n).contains(&ii) && (0..n).contains(&jj) {
                    (f * self.n * self.n + jj as usize * self.n + ii as usize) as u32
                } else {
                    self.point_to_cell(self.raw_center(f, ii, jj))
                };
                if nb != c && !out.contains(&nb) {
                    out.push(nb);
                }
            }
        }
        out
    }

    /// Local angular cell size at p. Gnomonic cells shrink toward face
    /// corners (to roughly half the face-center size); anything scaled to
    /// the grid — valley widths, node jitter — must shrink with them so
    /// coverage guarantees hold everywhere.
    pub fn local_cell_size(&self, p: [f64; 3]) -> f64 {
        let (_, a, b) = self.face_coords(p);
        let r2 = 1.0 + a * a + b * b;
        let r = r2.sqrt();
        let sa = (1.0 - a * a / r2).sqrt() / r;
        let sb = (1.0 - b * b / r2).sqrt() / r;
        self.max_cell_size() * sa.min(sb)
    }
}

#[inline]
fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

#[inline]
fn normalize(v: [f64; 3]) -> [f64; 3] {
    let inv = 1.0 / dot(v, v).sqrt();
    [v[0] * inv, v[1] * inv, v[2] * inv]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn center_roundtrips_to_same_cell() {
        let g = CubeGrid::new(64);
        for c in (0..g.cells() as u32).step_by(7) {
            assert_eq!(g.point_to_cell(g.cell_center(c)), c, "cell {c}");
        }
    }

    #[test]
    fn neighbors_are_geometrically_adjacent() {
        // Every neighbor — including across face edges and at cube corners —
        // must sit within a few cell sizes of the cell itself. This is the
        // test that would catch any adjacency bug a transition table hides.
        let g = CubeGrid::new(64);
        for c in (0..g.cells() as u32).step_by(11) {
            let p = g.cell_center(c);
            let nbs = g.neighbors(c);
            assert!(
                (5..=8).contains(&nbs.len()),
                "cell {c} has {} neighbors",
                nbs.len()
            );
            for nb in nbs {
                let q = g.cell_center(nb);
                let d = dot(p, q).clamp(-1.0, 1.0).acos();
                assert!(
                    d < 3.0 * g.max_cell_size(),
                    "cell {c} neighbor {nb} is {d} rad away"
                );
            }
        }
    }
}
