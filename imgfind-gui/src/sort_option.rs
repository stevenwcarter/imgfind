//! GUI sort-selector option: the label layer above the core `SortKey`.
//! `Relevance` has no `SortKey` (it restores relevance order).
use imgfind::sort::SortKey;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortOption {
    Relevance,
    Name,
    Size,
    Type,
}

impl SortOption {
    pub fn all() -> [SortOption; 4] {
        [
            SortOption::Relevance,
            SortOption::Name,
            SortOption::Size,
            SortOption::Type,
        ]
    }
    pub fn to_sort_key(self) -> Option<SortKey> {
        match self {
            SortOption::Relevance => None,
            SortOption::Name => Some(SortKey::Name),
            SortOption::Size => Some(SortKey::Size),
            SortOption::Type => Some(SortKey::Type),
        }
    }
}
impl fmt::Display for SortOption {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            SortOption::Relevance => "Relevance",
            SortOption::Name => "Name",
            SortOption::Size => "Size",
            SortOption::Type => "Type",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn to_sort_key_maps_all_variants() {
        assert_eq!(SortOption::Relevance.to_sort_key(), None);
        assert_eq!(SortOption::Size.to_sort_key(), Some(SortKey::Size));
    }
    #[test]
    fn display_matches_labels() {
        assert_eq!(
            SortOption::all().map(|o| o.to_string()),
            ["Relevance", "Name", "Size", "Type"].map(String::from)
        );
    }
}
