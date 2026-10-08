use std::path::Path;

/// Claude Code's project-folder name for a directory: every UTF-16 unit that is
/// not `[A-Za-z0-9]` becomes `-`. The mapping is lossy, so it can't be reversed.
pub fn encode_path(p: &Path) -> String {
    p.to_string_lossy()
        .encode_utf16()
        .map(|u| match u {
            0x30..=0x39 | 0x41..=0x5a | 0x61..=0x7a => u as u8 as char,
            _ => '-',
        })
        .collect()
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
}
