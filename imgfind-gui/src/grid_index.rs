//! Index newtypes for the thumbnail grid's pure navigation/virtualization math.
//!
//! `move_selection` (`nav.rs`) and `window_range` (`window.rs`) each take several
//! same-typed `usize` arguments where transposing two — e.g. `cols` and `len` —
//! silently produces wrong grid math. Wrapping them makes such a swap a compile
//! error. These are ephemeral GUI state (never persisted), so no serde.

/// A cursor / item position within the ordered result list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorIndex(pub usize);

/// The number of columns in the thumbnail grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GridCols(pub usize);

/// The total number of items in the grid (result count).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ItemCount(pub usize);

impl CursorIndex {
    /// The wrapped index.
    pub fn get(self) -> usize {
        self.0
    }
}

impl GridCols {
    /// The wrapped column count.
    pub fn get(self) -> usize {
        self.0
    }
}

impl ItemCount {
    /// The wrapped item count.
    pub fn get(self) -> usize {
        self.0
    }
}
