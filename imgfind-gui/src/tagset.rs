//! Pure helpers for editing ordered, de-duplicated tag lists (brushes and the
//! "Most Recent" buffer).

/// Add whitespace-separated words to `list`, trimming and skipping duplicates,
/// preserving existing order and append order of new words.
pub fn add_words(list: &mut Vec<String>, text: &str) {
    for w in text.split_whitespace() {
        let w = w.trim();
        if !w.is_empty() && !list.iter().any(|t| t == w) {
            list.push(w.to_string());
        }
    }
}

/// Replace the list contents with the whitespace-separated words of `text`
/// (trim, dedupe, preserve order).
pub fn set_words(list: &mut Vec<String>, text: &str) {
    list.clear();
    add_words(list, text);
}

/// Remove `tag` if present.
pub fn remove(list: &mut Vec<String>, tag: &str) {
    list.retain(|t| t != tag);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_words_dedupes_and_trims() {
        let mut v = vec!["beach".to_string()];
        add_words(&mut v, "  sunset beach   john ");
        assert_eq!(v, vec!["beach", "sunset", "john"]);
    }

    #[test]
    fn set_words_replaces() {
        let mut v = vec!["old".to_string()];
        set_words(&mut v, "a b a");
        assert_eq!(v, vec!["a", "b"]);
    }

    #[test]
    fn remove_drops_one() {
        let mut v = vec!["a".to_string(), "b".to_string()];
        remove(&mut v, "a");
        assert_eq!(v, vec!["b"]);
    }
}
