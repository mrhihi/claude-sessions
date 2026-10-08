use std::path::Path;

/// Claude Code truncates project-folder names longer than this and appends a hash.
const MAX_LEN: usize = 200;

/// `Math.abs(h).toString(36)` of Claude's `h = h * 31 + utf16_unit` (wrapping i32) over `s`.
fn short_hash(s: &str) -> String {
    let h = s.encode_utf16().fold(0i32, |h, u| h.wrapping_mul(31).wrapping_add(u as i32));
    let mut n = (h as i64).unsigned_abs();
    if n == 0 {
        return "0".into();
    }
    let mut out = Vec::new();
    while n > 0 {
        out.push(char::from_digit((n % 36) as u32, 36).unwrap());
        n /= 36;
    }
    out.iter().rev().collect()
}

/// Claude Code's project-folder name for a directory: every UTF-16 unit that is
/// not `[A-Za-z0-9]` becomes `-`; names over 200 characters are cut to 200 and get
/// `-<hash of the original path>`. The mapping is lossy, so it can't be reversed.
pub fn encode_path(p: &Path) -> String {
    let full = p.to_string_lossy();
    let slug: String = full
        .encode_utf16()
        .map(|u| match u {
            0x30..=0x39 | 0x41..=0x5a | 0x61..=0x7a => u as u8 as char,
            _ => '-',
        })
        .collect();
    if slug.len() <= MAX_LEN {
        return slug;
    }
    format!("{}-{}", &slug[..MAX_LEN], short_hash(&full))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_non_alphanumerics() {
        let p = Path::new("/Users/a b/Local Workspace/my_proj.v2");
        assert_eq!(encode_path(p), "-Users-a-b-Local-Workspace-my-proj-v2");
    }

    #[test]
    fn non_bmp_counts_two_units() {
        assert_eq!(encode_path(Path::new("/😀")), "---");
    }

    #[test]
    fn long_paths_are_cut_and_hashed_like_claude() {
        let p = format!("/{}", "a".repeat(250));
        let e = encode_path(Path::new(&p));
        assert!(e.starts_with(&format!("-{}-", "a".repeat(199))));
        assert_eq!(e.len(), 200 + 1 + short_hash(&p).len());
        assert_ne!(e, encode_path(Path::new(&format!("{p}b"))));
    }

    #[test]
    fn hash_matches_java_string_hashcode() {
        // "hello".hashCode() == 99162322 == "1n1e4y" in base 36
        assert_eq!(short_hash("hello"), "1n1e4y");
        assert_eq!(short_hash(""), "0");
    }
}
