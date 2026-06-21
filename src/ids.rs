//! Strongly-typed row identifiers. Newtypes over `i64` so an image id, tag id,
//! and collection id can never be transposed (the `image_tags` /
//! `collection_images` inserts take two adjacent ids). `#[serde(transparent)]`
//! keeps the persisted `ui_state` JSON a bare integer.
use serde::{Deserialize, Serialize};

macro_rules! row_id {
    ($name:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub i64);
        impl $name {
            pub const fn get(self) -> i64 {
                self.0
            }
        }
        impl From<i64> for $name {
            fn from(v: i64) -> Self {
                Self(v)
            }
        }
    };
}
row_id!(ImageId);
row_id!(TagId);
row_id!(CollectionId);

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn image_id_serializes_transparently() {
        assert_eq!(serde_json::to_string(&ImageId(7)).unwrap(), "7");
        let v: Vec<ImageId> = serde_json::from_str("[3,1,2]").unwrap();
        assert_eq!(v, vec![ImageId(3), ImageId(1), ImageId(2)]);
    }
}
