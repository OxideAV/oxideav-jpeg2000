//! Tag-tree decoder (ISO/IEC 15444-1 §B.10.2).
//!
//! Tag trees compress the stream of per-code-block inclusion flags and
//! "number of missing zero bitplanes" values carried in the packet
//! header. Each leaf corresponds to one code-block. Every interior node
//! holds the minimum of its children, encoded hierarchically so that
//! threshold queries decode only as many bits as needed to prove the
//! leaf is above / at a threshold.

use super::bio::Bio;

/// Tag-tree state. The leaf layer is `w × h` nodes; interior levels halve
/// both dimensions at each step. `current` stores the best-known lower
/// bound for each node; it is updated as more bits are decoded.
pub struct TagTree {
    /// Width in leaves.
    pub w: usize,
    /// Height in leaves.
    pub h: usize,
    /// Running lower bound per node (initialised to 0 on reset).
    low: Vec<u32>,
    /// Running decoded value per node (initialised to u32::MAX on reset).
    value: Vec<u32>,
    /// Per-level offset into the flat `low`/`value` arrays.
    level_off: Vec<usize>,
    /// Per-level (w, h).
    levels: Vec<(usize, usize)>,
}

impl TagTree {
    pub fn new(w: usize, h: usize) -> Self {
        let mut levels = Vec::new();
        let mut level_off = Vec::new();
        let (mut lw, mut lh) = (w, h);
        let mut total = 0;
        levels.push((lw, lh));
        level_off.push(total);
        total += lw * lh;
        while lw > 1 || lh > 1 {
            lw = lw.div_ceil(2);
            lh = lh.div_ceil(2);
            levels.push((lw, lh));
            level_off.push(total);
            total += lw * lh;
        }
        let mut t = TagTree {
            w,
            h,
            low: vec![0; total.max(1)],
            value: vec![u32::MAX; total.max(1)],
            level_off,
            levels,
        };
        t.reset();
        t
    }

    /// Reset all tree state (same as calling `new` again but without a
    /// reallocation). Called once per packet before decoding inclusion
    /// and zero-bitplane tag trees.
    pub fn reset(&mut self) {
        for v in &mut self.low {
            *v = 0;
        }
        for v in &mut self.value {
            *v = u32::MAX;
        }
    }

    /// Decode the value at `(x, y)` in the leaf layer up to `threshold`.
    /// Returns `true` if the leaf value is strictly less than the
    /// threshold (i.e. the event has happened), `false` otherwise.
    pub fn decode(&mut self, x: usize, y: usize, threshold: u32, bio: &mut Bio<'_>) -> bool {
        // Walk from root down to leaf, maintaining a running lower bound.
        let nlvls = self.levels.len();
        let mut stack: [(usize, usize, usize); 32] = [(0, 0, 0); 32];
        let mut top = 0;
        let mut cx = x;
        let mut cy = y;
        for lvl in 0..nlvls {
            let (lw, _lh) = self.levels[lvl];
            stack[top] = (lvl, cx + cy * lw, lvl);
            top += 1;
            cx /= 2;
            cy /= 2;
        }
        let mut low: u32 = 0;
        // Process from the top level (root) down to the leaf.
        while top > 0 {
            top -= 1;
            let (lvl, idx_in_level, _) = stack[top];
            let idx = self.level_off[lvl] + idx_in_level;
            if low > self.low[idx] {
                self.low[idx] = low;
            } else {
                low = self.low[idx];
            }
            while low < threshold && low < self.value[idx] {
                if bio.read_bit() != 0 {
                    self.value[idx] = low;
                } else {
                    low += 1;
                }
            }
            self.low[idx] = low;
        }
        let leaf_idx = self.level_off[0] + x + y * self.w;
        self.value[leaf_idx] < threshold
    }

    /// Return the fully-resolved leaf value (only valid once all
    /// threshold queries have succeeded).
    pub fn value_at(&self, x: usize, y: usize) -> u32 {
        self.value[self.level_off[0] + x + y * self.w]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tagtree_empty_build() {
        let t = TagTree::new(2, 2);
        assert_eq!(t.levels[0], (2, 2));
        assert!(t.levels.len() >= 2);
    }

    /// Simple tree with 1 leaf: decode with threshold=1 and bit sequence
    /// `[1]` should yield `value=0 < 1 = true`.
    #[test]
    fn tagtree_single_leaf_threshold_one_with_bit_one_is_true() {
        let mut t = TagTree::new(1, 1);
        let bits = [0b1000_0000u8];
        let mut bio = Bio::new(&bits);
        assert!(t.decode(0, 0, 1, &mut bio));
        assert_eq!(t.value_at(0, 0), 0);
    }

    /// With bit sequence `[0, 0, 1]` the leaf value should resolve to
    /// 2 (two zero bits advancing `low`, then a one terminating).
    #[test]
    fn tagtree_single_leaf_threshold_three_needs_three_bits() {
        let mut t = TagTree::new(1, 1);
        // Bits: 0, 0, 1 -> leaf value = 2.
        let bits = [0b001_00000u8];
        let mut bio = Bio::new(&bits);
        // Incrementally walk thresholds 1..=3.
        assert!(!t.decode(0, 0, 1, &mut bio));
        assert!(!t.decode(0, 0, 2, &mut bio));
        assert!(t.decode(0, 0, 3, &mut bio));
        assert_eq!(t.value_at(0, 0), 2);
    }

    /// Threshold-0 queries must consume zero bits.
    #[test]
    fn tagtree_threshold_zero_consumes_no_bits() {
        let mut t = TagTree::new(1, 1);
        let bits = [0b1000_0000u8];
        let mut bio = Bio::new(&bits);
        assert!(!t.decode(0, 0, 0, &mut bio));
        // The next threshold-1 call should still see the first bit.
        assert!(t.decode(0, 0, 1, &mut bio));
    }
}
