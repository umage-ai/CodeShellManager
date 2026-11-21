//! Where the rows of a code or diff view are, without a row table: a fixed row height, plus a few
//! taller blocks (open comment threads) inserted after some rows. Scrolling to line 50,000 with
//! threads above it is a binary search, and nothing here allocates per row. Pure: no egui.


/// Rows of height `row_h`, with blocks inserted after some of them.
#[derive(Debug, Clone, PartialEq)]
pub struct RowLayout {
    n: usize,
    row_h: f32,
    /// `(after_row, height)`, sorted by `after_row` (stable: blocks after the same row keep order).
    inserts: Vec<(usize, f32)>,
    /// `cum[k]`: the height of `inserts[..k]`.
    cum: Vec<f32>,
}

fn sane(h: f32) -> f32 {
    if h.is_finite() { h.clamp(0.0, 100_000.0) } else { 0.0 }
}

impl RowLayout {
    pub fn new(rows: usize, row_h: f32, mut inserts: Vec<(usize, f32)>) -> RowLayout {
        let row_h = if row_h.is_finite() && row_h > 0.0 { row_h } else { 1.0 };
        inserts.sort_by_key(|(a, _)| *a);
        for (a, h) in &mut inserts {
            *a = (*a).min(rows.saturating_sub(1));
            *h = sane(*h);
        }
        let mut cum = Vec::with_capacity(inserts.len() + 1);
        let mut acc = 0.0f32;
        for (_, h) in &inserts {
            cum.push(acc);
            acc += *h;
        }
        cum.push(acc);
        RowLayout { n: rows, row_h, inserts, cum }
    }

    /// The blocks, as `(after_row, height)`.
    pub fn inserts(&self) -> &[(usize, f32)] {
        &self.inserts
    }

    pub fn total(&self) -> f32 {
        self.n as f32 * self.row_h + self.cum.last().copied().unwrap_or(0.0)
    }

    /// The top of row `i` (the bottom of the last row for `i == rows`).
    pub fn row_top(&self, i: usize) -> f32 {
        let i = i.min(self.n);
        let before = self.inserts.partition_point(|(a, _)| *a < i);
        i as f32 * self.row_h + self.cum.get(before).copied().unwrap_or(0.0)
    }

    /// The top of block `k` (just under its row, and under the blocks before it).
    pub fn insert_top(&self, k: usize) -> f32 {
        let Some((after, _)) = self.inserts.get(k) else { return self.total() };
        (*after as f32 + 1.0) * self.row_h + self.cum.get(k).copied().unwrap_or(0.0)
    }

    /// The blocks inserted after row `i`.
    #[cfg(test)]
    pub fn inserts_after(&self, i: usize) -> std::ops::Range<usize> {
        self.inserts.partition_point(|(a, _)| *a < i)..self.inserts.partition_point(|(a, _)| *a <= i)
    }

    /// The row that covers `y` (a `y` inside a block gives the row above it). `None` when there
    /// are no rows.
    pub fn row_at(&self, y: f32) -> Option<usize> {
        if self.n == 0 {
            return None;
        }
        if !y.is_finite() || y <= 0.0 {
            return Some(0);
        }
        let (mut lo, mut hi) = (0usize, self.n - 1);
        while lo < hi {
            let mid = lo + (hi - lo).div_ceil(2);
            if self.row_top(mid) <= y {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }
        Some(lo)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_without_blocks_are_a_plain_grid() {
        let l = RowLayout::new(100, 20.0, vec![]);
        assert_eq!((l.total(), l.row_top(0), l.row_top(5), l.row_top(100)), (2000.0, 0.0, 100.0, 2000.0));
        assert_eq!((l.row_at(0.0), l.row_at(19.9), l.row_at(20.0), l.row_at(1999.0), l.row_at(1e9)), (Some(0), Some(0), Some(1), Some(99), Some(99)));
        assert_eq!(l.row_at(-5.0), Some(0));
        assert_eq!(l.row_at(f32::NAN), Some(0));
        assert_eq!(RowLayout::new(0, 20.0, vec![]).row_at(5.0), None);
    }

    #[test]
    fn blocks_push_the_rows_under_them_down() {
        // A 100 px block after row 1 and a 50 px block after row 1 again, one after row 4.
        let l = RowLayout::new(10, 10.0, vec![(4, 30.0), (1, 100.0), (1, 50.0)]);
        assert_eq!(l.inserts(), &[(1, 100.0), (1, 50.0), (4, 30.0)], "sorted, stable");
        assert_eq!(l.total(), 100.0 + 180.0);
        assert_eq!((l.row_top(0), l.row_top(1), l.row_top(2)), (0.0, 10.0, 20.0 + 150.0));
        assert_eq!((l.insert_top(0), l.insert_top(1), l.insert_top(2)), (20.0, 120.0, 200.0));
        assert_eq!(l.inserts_after(1), 0..2);
        assert_eq!(l.inserts_after(4), 2..3);
        assert_eq!(l.inserts_after(0), 0..0);
        // A y inside a block is the row above it; the first row under it starts after the blocks.
        assert_eq!(l.row_at(100.0), Some(1));
        assert_eq!(l.row_at(169.9), Some(1));
        assert_eq!(l.row_at(170.0), Some(2));
        // Consistency over every row.
        for i in 0..10 {
            assert_eq!(l.row_at(l.row_top(i) + 0.5), Some(i));
        }
    }

    #[test]
    fn a_hundred_thousand_rows_with_threads_find_line_50000_fast() {
        let inserts: Vec<(usize, f32)> = (0..500).map(|i| (i * 200, 140.0)).collect();
        let l = RowLayout::new(100_000, 18.0, inserts);
        let started = std::time::Instant::now();
        let mut sum = 0usize;
        for k in 0..10_000 {
            let y = l.row_top(50_000) + (k % 18) as f32;
            sum += l.row_at(y).unwrap_or(0);
        }
        assert!(started.elapsed() < std::time::Duration::from_millis(500), "{:?}", started.elapsed());
        assert_eq!(l.row_at(l.row_top(50_000)), Some(50_000));
        assert!(sum > 0);
        // 250 blocks sit above row 50,000 (after rows 0, 200, ... 49,800).
        assert_eq!(l.row_top(50_000), 50_000.0 * 18.0 + 250.0 * 140.0);
    }

    #[test]
    fn hostile_numbers_are_made_sane() {
        let l = RowLayout::new(3, f32::NAN, vec![(99, f32::INFINITY), (1, -5.0), (0, 1e30)]);
        assert!(l.total().is_finite());
        assert_eq!(l.n, 3);
        assert!(l.inserts().iter().all(|(a, h)| *a < 3 && h.is_finite() && *h >= 0.0));
        let _ = l.row_at(1e30);
        let _ = l.insert_top(77);
    }
}
