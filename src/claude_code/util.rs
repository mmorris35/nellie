/// Truncate a string to at most `max_bytes` bytes without splitting a
/// multi-byte UTF-8 character. Returns the longest prefix that fits.
pub(crate) fn truncate_to_char_boundary(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_within_limit() {
        assert_eq!(truncate_to_char_boundary("hello", 200), "hello");
    }

    #[test]
    fn ascii_at_limit() {
        let s = "a".repeat(200);
        assert_eq!(truncate_to_char_boundary(&s, 200).len(), 200);
    }

    #[test]
    fn ascii_over_limit() {
        let s = "a".repeat(300);
        assert_eq!(truncate_to_char_boundary(&s, 200).len(), 200);
    }

    #[test]
    fn multibyte_on_boundary() {
        // 199 ASCII bytes + a 3-byte char (→) straddling byte 200
        let s = "x".repeat(199) + "→tail";
        let t = truncate_to_char_boundary(&s, 200);
        assert_eq!(t.len(), 199);
        assert!(t.is_char_boundary(t.len()));
    }

    #[test]
    fn empty_string() {
        assert_eq!(truncate_to_char_boundary("", 200), "");
    }

    #[test]
    fn zero_max() {
        assert_eq!(truncate_to_char_boundary("hello", 0), "");
    }
}
