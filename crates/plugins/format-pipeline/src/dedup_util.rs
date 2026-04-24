//! Small helpers shared between `dedup` and `layered_pipeline`.

use sha2::{Digest, Sha256};

/// 8-char hex hash of a file path — used as the cache-invalidation key
/// for FileRead / FileMutate tools. The underlying path never leaves
/// the pipeline untruncated.
pub fn file_path_hash(path: &str) -> String {
    let digest = Sha256::digest(path.as_bytes());
    let mut out = String::with_capacity(8);
    for b in &digest[..4] {
        out.push_str(&format!("{:02x}", b));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_for_same_input() {
        assert_eq!(file_path_hash("/foo/bar"), file_path_hash("/foo/bar"));
    }

    #[test]
    fn differs_for_different_inputs() {
        assert_ne!(file_path_hash("/foo"), file_path_hash("/foo/bar"));
    }

    #[test]
    fn has_expected_length() {
        assert_eq!(file_path_hash("anything").len(), 8);
    }
}
