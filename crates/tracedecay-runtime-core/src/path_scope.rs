pub fn path_matches_scope(path: &str, scope_prefix: Option<&str>) -> bool {
    scope_prefix.is_none_or(|prefix| {
        let with_slash = if prefix.ends_with('/') {
            prefix.to_string()
        } else {
            format!("{prefix}/")
        };
        path.starts_with(&with_slash) || path == prefix
    })
}

#[cfg(test)]
mod tests {
    use super::path_matches_scope;

    #[test]
    fn scope_prefix_matches_exact_file_or_descendant() {
        assert!(path_matches_scope("src/lib.rs", Some("src")));
        assert!(path_matches_scope("src", Some("src")));
        assert!(!path_matches_scope("src2/lib.rs", Some("src")));
        assert!(path_matches_scope("src/lib.rs", None));
    }
}
