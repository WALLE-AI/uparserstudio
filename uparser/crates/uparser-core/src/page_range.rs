//! `--pages` range parsing (CLI_ENHANCEMENT_PROPOSAL.md's "P0：`--pages`
//! 范围选择"): lets a caller parse only a subset of a large document's
//! pages — e.g. to validate a protocol/endpoint against page 50 of a
//! 107-page document without waiting for pages 1-49 first.
//!
//! Syntax: comma-separated list of single page numbers or `start-end`
//! ranges, 1-indexed inclusive, e.g. `"1-5"`, `"3"`, `"1,5,10-12"`.

/// Parse a `--pages` argument into a sorted, deduplicated list of
/// 1-indexed page numbers. Returns a clear `Err` (not a panic) for
/// malformed input: non-numeric tokens, an inverted range (`"5-2"`), or
/// a zero page number (pages are 1-indexed).
pub fn parse_page_range(spec: &str) -> Result<Vec<u32>, String> {
    let mut pages = Vec::new();

    for token in spec.split(',') {
        let token = token.trim();
        if token.is_empty() {
            return Err(format!("empty page token in {spec:?}"));
        }

        if let Some((start, end)) = token.split_once('-') {
            let start: u32 = start
                .trim()
                .parse()
                .map_err(|_| format!("invalid page range {token:?} in {spec:?}"))?;
            let end: u32 = end
                .trim()
                .parse()
                .map_err(|_| format!("invalid page range {token:?} in {spec:?}"))?;
            if start == 0 || end == 0 {
                return Err(format!(
                    "page numbers are 1-indexed, got {token:?} in {spec:?}"
                ));
            }
            if start > end {
                return Err(format!(
                    "inverted page range {token:?} in {spec:?} (start > end)"
                ));
            }
            pages.extend(start..=end);
        } else {
            let page: u32 = token
                .parse()
                .map_err(|_| format!("invalid page number {token:?} in {spec:?}"))?;
            if page == 0 {
                return Err(format!(
                    "page numbers are 1-indexed, got {token:?} in {spec:?}"
                ));
            }
            pages.push(page);
        }
    }

    pages.sort_unstable();
    pages.dedup();
    Ok(pages)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_page() {
        assert_eq!(parse_page_range("3").unwrap(), vec![3]);
    }

    #[test]
    fn simple_range() {
        assert_eq!(parse_page_range("1-5").unwrap(), vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn comma_separated_mix() {
        assert_eq!(
            parse_page_range("1,5,10-12").unwrap(),
            vec![1, 5, 10, 11, 12]
        );
    }

    #[test]
    fn dedupes_and_sorts_overlapping_input() {
        assert_eq!(
            parse_page_range("5,1-3,2,3-4").unwrap(),
            vec![1, 2, 3, 4, 5]
        );
    }

    #[test]
    fn single_page_range_is_valid() {
        assert_eq!(parse_page_range("7-7").unwrap(), vec![7]);
    }

    #[test]
    fn whitespace_around_tokens_is_tolerated() {
        assert_eq!(parse_page_range(" 1 , 3-4 ").unwrap(), vec![1, 3, 4]);
    }

    #[test]
    fn non_numeric_token_is_a_clean_error_not_a_panic() {
        assert!(parse_page_range("abc").is_err());
        assert!(parse_page_range("1,abc,3").is_err());
    }

    #[test]
    fn inverted_range_is_a_clean_error() {
        let err = parse_page_range("5-2").unwrap_err();
        assert!(err.contains("inverted"));
    }

    #[test]
    fn zero_page_is_rejected_since_pages_are_1_indexed() {
        assert!(parse_page_range("0").is_err());
        assert!(parse_page_range("0-3").is_err());
    }

    #[test]
    fn empty_token_is_a_clean_error() {
        assert!(parse_page_range("1,,3").is_err());
        assert!(parse_page_range("").is_err());
    }
}
