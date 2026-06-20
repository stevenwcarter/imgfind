//! Fixed palette of tag "brush" colors. Colors are an input convenience in the
//! GUI (quick-apply sets of tags); they are never persisted on images or tags.
//! Index order is stable and shared with the persisted `UiState.brushes` array.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrushColor {
    Red,
    Green,
    Yellow,
    Purple,
    Blue,
}

impl BrushColor {
    pub const ALL: [BrushColor; 5] = [
        BrushColor::Red,
        BrushColor::Green,
        BrushColor::Yellow,
        BrushColor::Purple,
        BrushColor::Blue,
    ];

    pub fn index(self) -> usize {
        match self {
            BrushColor::Red => 0,
            BrushColor::Green => 1,
            BrushColor::Yellow => 2,
            BrushColor::Purple => 3,
            BrushColor::Blue => 4,
        }
    }

    pub fn from_index(i: usize) -> Option<BrushColor> {
        BrushColor::ALL.get(i).copied()
    }

    pub fn letter(self) -> &'static str {
        match self {
            BrushColor::Red => "r",
            BrushColor::Green => "g",
            BrushColor::Yellow => "y",
            BrushColor::Purple => "p",
            BrushColor::Blue => "b",
        }
    }

    pub fn from_letter(s: &str) -> Option<BrushColor> {
        match s {
            "r" => Some(BrushColor::Red),
            "g" => Some(BrushColor::Green),
            "y" => Some(BrushColor::Yellow),
            "p" => Some(BrushColor::Purple),
            "b" => Some(BrushColor::Blue),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letter_index_roundtrip() {
        for c in BrushColor::ALL {
            assert_eq!(BrushColor::from_letter(c.letter()), Some(c));
            assert_eq!(BrushColor::from_index(c.index()), Some(c));
        }
    }

    #[test]
    fn index_is_stable_order() {
        assert_eq!(BrushColor::Red.index(), 0);
        assert_eq!(BrushColor::Blue.index(), 4);
    }

    #[test]
    fn unknown_letter_is_none() {
        assert_eq!(BrushColor::from_letter("x"), None);
        assert_eq!(BrushColor::from_letter("m"), None);
    }
}
