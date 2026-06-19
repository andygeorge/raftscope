//! Small numeric/geometry helpers ported from util.js.

/// Really big number. f64 INFINITY is avoided because it does not survive
/// JSON serialization (serde emits `null`), which would corrupt checkpoints.
pub const INF: f64 = 1e300;

/// Uniform random in [0, 1). On wasm this is backed by the JS engine to match
/// raft.js exactly; on the host (for `cargo test`) it uses a small xorshift so
/// the pure protocol layers can be exercised without a browser.
#[cfg(target_arch = "wasm32")]
pub fn random() -> f64 {
    js_sys::Math::random()
}

#[cfg(not(target_arch = "wasm32"))]
pub fn random() -> f64 {
    use std::cell::Cell;
    thread_local! {
        static SEED: Cell<u64> = const { Cell::new(0x9E3779B97F4A7C15) };
    }
    SEED.with(|s| {
        let mut x = s.get();
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        s.set(x);
        (x >> 11) as f64 / (1u64 << 53) as f64
    })
}

#[derive(Clone, Copy)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

/// Position `frac` of the way around a circle, starting from the top.
pub fn circle_coord(frac: f64, cx: f64, cy: f64, r: f64) -> Point {
    let radians = 2.0 * std::f64::consts::PI * (0.75 + frac);
    Point {
        x: cx + r * radians.cos(),
        y: cy + r * radians.sin(),
    }
}

pub fn clamp(value: f64, low: f64, high: f64) -> f64 {
    if value < low {
        low
    } else if value > high {
        high
    } else {
        value
    }
}

/// Index of the greatest element of `a` for which `gt(elem)` is false,
/// assuming `a` is sorted such that `gt` is monotonic. Returns -1 if none.
/// Mirrors util.greatestLower's binary search semantics.
pub fn greatest_lower<T, F: Fn(&T) -> bool>(a: &[T], gt: F) -> isize {
    fn bs<T, F: Fn(&T) -> bool>(a: &[T], gt: &F, low: isize, high: isize) -> isize {
        if high < low {
            return low - 1;
        }
        let mid = (low + high) / 2;
        if gt(&a[mid as usize]) {
            bs(a, gt, low, mid - 1)
        } else {
            bs(a, gt, mid + 1, high)
        }
    }
    bs(a, &gt, 0, a.len() as isize - 1)
}
