//! Admitted-root identity and strict `file:` URI admission for the gateway.

use std::path::{Component, PathBuf};

use tracedecay_domain::ManifestDigest;
use tracedecay_runtime_core::path_safety::{canonicalize_existing_prefix, same_canonical_path};
use url::Url;

/// A single root that was authoritatively admitted before the LSP session was
/// created. The gateway never chooses a root from CWD or client folder order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdmittedRoot {
    uri: String,
    scope_digest: Option<ManifestDigest>,
}

impl AdmittedRoot {
    pub fn new(uri: impl Into<String>) -> Self {
        Self {
            uri: uri.into(),
            scope_digest: None,
        }
    }

    /// Bind a presentation URI to an exact application-resolved scope.
    pub fn authorized(uri: impl Into<String>, scope_digest: ManifestDigest) -> Self {
        Self {
            uri: uri.into(),
            scope_digest: Some(scope_digest),
        }
    }

    pub fn uri(&self) -> &str {
        &self.uri
    }

    pub fn scope_digest(&self) -> Option<&ManifestDigest> {
        self.scope_digest.as_ref()
    }

    pub(crate) fn is_valid(&self) -> bool {
        strict_file_uri_segments(&self.uri).is_some()
    }

    pub(crate) fn matches_root_uri(&self, candidate: &str) -> bool {
        match (
            strict_file_uri_segments(&self.uri),
            strict_file_uri_segments(candidate),
        ) {
            (
                Some((admitted_url, admitted_segments)),
                Some((candidate_url, candidate_segments)),
            ) => {
                admitted_url.host_str() == candidate_url.host_str()
                    && (admitted_segments == candidate_segments
                        || same_local_directory(&admitted_url, &candidate_url))
            }
            _ => false,
        }
    }

    /// Presentation-level containment guard. Root admission itself remains a
    /// daemon authorization decision; this rejects non-file and ambiguous URI
    /// forms, then compares decoded filesystem path components rather than raw
    /// URI prefixes.
    pub fn contains_document(&self, document_uri: &str) -> bool {
        DocumentConfinement::for_root(self).is_some_and(|root| root.contains(document_uri))
    }

    pub(crate) fn document_root_depth(&self, document_uri: &str) -> Option<usize> {
        self.contains_document(document_uri)
            .then_some(())
            .and_then(|()| strict_file_uri_segments(&self.uri))
            .map(|(_, segments)| segments.len())
    }
}

/// Validates a `file:` URI at the URL level, rejecting any ambiguous,
/// non-canonical, or traversal-prone form. Deliberately stops short of
/// `to_file_path()`: a UNC-host URI (`file://server/share/…`) is a valid URL
/// on every platform even though only Windows can convert it to a local
/// path. Shared so a URI and an HTTP path can never disagree on which forms
/// are admitted.
pub fn strict_file_url(uri: &str) -> Option<Url> {
    let (_, after_scheme) = uri.split_once(':')?;
    if after_scheme.contains('\\') {
        return None;
    }
    let raw_path = if let Some(authority_and_path) = after_scheme.strip_prefix("//") {
        authority_and_path
            .find('/')
            .map_or("", |path_start| &authority_and_path[path_start..])
    } else {
        after_scheme
    };
    if !valid_raw_uri_path(raw_path) {
        return None;
    }

    let url = Url::parse(uri).ok()?;
    if url.scheme() != "file"
        || url.cannot_be_a_base()
        || url.path().is_empty()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    Some(url)
}

/// Parses a `file:` URI into its `(Url, decoded path segments)` form: the
/// URL-level rules of [`strict_file_url`] plus percent-decoding of every
/// non-empty segment.
///
/// URI identity and containment are protocol questions, so this deliberately
/// stops short of [`Url::to_file_path`] the same way [`strict_file_url`] does.
/// That conversion is platform-dependent in both directions — a drive-less
/// path such as `file:///root` converts on Unix and fails on Windows, while a
/// UNC host converts only on Windows — so routing root equality or document
/// containment through it makes the same LSP request admitted on one host and
/// rejected on another. Segments are compared instead, which is identical on
/// every platform. A real filesystem path is produced only where one is
/// genuinely needed (the symlink-escape guard below, daemon owner lookup),
/// and those sites already fail closed when the conversion is impossible.
pub fn strict_file_uri_segments(uri: &str) -> Option<(Url, Vec<String>)> {
    let url = strict_file_url(uri)?;
    let mut segments = Vec::new();
    for segment in url.path_segments()? {
        if segment.is_empty() {
            continue;
        }
        let decoded = String::from_utf8(decode_uri_segment(segment)?).ok()?;
        if decoded == "." || decoded == ".." {
            return None;
        }
        segments.push(decoded);
    }
    Some((url, segments))
}

/// Parses a `file:` URI into its `(Url, PathBuf)` form: the URL-level rules
/// of [`strict_file_url`] plus a platform-local path conversion and a
/// traversal-component check on the converted path.
///
/// Only for callers that need to touch the filesystem; use
/// [`strict_file_uri_segments`] to compare or contain URIs.
pub fn strict_file_uri_path(uri: &str) -> Option<(Url, PathBuf)> {
    let url = strict_file_url(uri)?;
    let path = url.to_file_path().ok()?;
    if path
        .components()
        .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        return None;
    }
    Some((url, path))
}

/// Whether two already-validated `file:` URIs name one directory on this
/// host, whatever spelling each carries.
///
/// Daemon admission canonicalizes a root before binding it, so a client that
/// addresses the same directory through an OS alias (macOS `/var` ->
/// `/private/var`) or a worktree symlink presents different segments for the
/// admitted root. Two URIs this host cannot convert to local paths never
/// compare here; their identity stays the platform-independent segment
/// comparison.
fn same_local_directory(left: &Url, right: &Url) -> bool {
    match (left.to_file_path(), right.to_file_path()) {
        (Ok(left), Ok(right)) => same_canonical_path(&left, &right),
        _ => false,
    }
}

/// Validates a raw (pre-percent-decoded) URI path: rejects an empty path,
/// NUL bytes, interior empty segments (`a//b`), and any segment that decodes
/// to `.`, `..`, or an encoded separator; a leading or trailing empty segment
/// (the root slash, a directory's trailing slash) is allowed. Shared so a URI
/// and an HTTP path can never disagree on which forms are admitted.
pub fn valid_raw_uri_path(raw_path: &str) -> bool {
    if raw_path.is_empty() || raw_path.as_bytes().contains(&0) {
        return false;
    }
    let segments = raw_path.split('/').collect::<Vec<_>>();
    for (index, segment) in segments.iter().enumerate() {
        if segment.is_empty() {
            let is_leading = index == 0;
            let is_trailing = index + 1 == segments.len();
            if !is_leading && !is_trailing {
                return false;
            }
            continue;
        }
        let Some(decoded) = decode_uri_segment(segment) else {
            return false;
        };
        if decoded == b"."
            || decoded == b".."
            || decoded.iter().any(|byte| matches!(*byte, b'/' | b'\\' | 0))
        {
            return false;
        }
    }
    true
}

/// Percent-decodes one URI path segment, or `None` for a malformed escape.
/// Shared so a URI and an HTTP path can never disagree on which escapes are
/// well formed.
pub fn decode_uri_segment(segment: &str) -> Option<Vec<u8>> {
    let source = segment.as_bytes();
    let mut decoded = Vec::with_capacity(source.len());
    let mut index = 0;
    while index < source.len() {
        if source[index] != b'%' {
            decoded.push(source[index]);
            index += 1;
            continue;
        }
        let high = source
            .get(index + 1)
            .copied()
            .and_then(percent_hex_nibble)?;
        let low = source
            .get(index + 2)
            .copied()
            .and_then(percent_hex_nibble)?;
        decoded.push((high << 4) | low);
        index += 3;
    }
    Some(decoded)
}

/// Decodes one hex digit of a `%XX` percent-escape, in either case.
///
/// `None` for any byte that is not a hex digit, which is what makes a
/// malformed escape fail the whole decode instead of silently producing a
/// different byte. Shared so a URI and an HTTP path can never disagree on
/// which escapes are well formed.
#[must_use]
pub fn percent_hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Cached root identity for one confinement pass. `contains_document` would
/// otherwise re-parse the admitted URI and re-canonicalize the root for every
/// location in a semantic response.
pub(super) struct DocumentConfinement {
    root_host: Option<String>,
    root_segments: Vec<String>,
    canonical_root: Option<PathBuf>,
}

impl DocumentConfinement {
    pub(super) fn for_root(root: &AdmittedRoot) -> Option<Self> {
        let (root_url, root_segments) = strict_file_uri_segments(&root.uri)?;
        Some(Self {
            root_host: root_url.host_str().map(str::to_owned),
            canonical_root: strict_file_uri_path(&root.uri)
                .and_then(|(_, path)| path.canonicalize().ok()),
            root_segments,
        })
    }

    pub(super) fn contains(&self, document_uri: &str) -> bool {
        let Some((document_url, document_segments)) = strict_file_uri_segments(document_uri) else {
            return false;
        };
        if self.root_host.as_deref() != document_url.host_str() {
            return false;
        }
        // A root this host cannot resolve to a directory (a foreign drive or
        // UNC shape) is contained lexically: the decoded segments are the only
        // identity available and they are the same on every platform.
        let Some(canonical_root) = self.canonical_root.as_ref() else {
            return document_segments.len() > self.root_segments.len()
                && document_segments.starts_with(&self.root_segments);
        };
        // Admission resolves filesystem aliases before binding this root.
        // Apply the same identity rule to client document URIs, including an
        // unsaved buffer whose existing parent is reached through an alias, so
        // a symlink that escapes the root is refused and an alias of the root
        // is admitted for the same reason.
        let Some((_, document_path)) = strict_file_uri_path(document_uri) else {
            return false;
        };
        let Some(canonical_document) = canonicalize_existing_prefix(&document_path) else {
            return false;
        };
        canonical_document
            .strip_prefix(canonical_root)
            .is_ok_and(|relative| {
                !relative.as_os_str().is_empty()
                    && relative
                        .components()
                        .all(|component| matches!(component, Component::Normal(_)))
            })
    }
}
