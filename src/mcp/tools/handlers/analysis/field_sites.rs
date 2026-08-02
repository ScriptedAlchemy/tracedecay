//! `tracedecay_field_sites` — read and write references to a named field.

use super::*;

pub(crate) async fn handle_field_sites(
    cg: &TraceDecay,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    let raw =
        args.get("field")
            .and_then(|v| v.as_str())
            .ok_or_else(|| TraceDecayError::Config {
                message: "tracedecay_field_sites requires a 'field' argument".to_string(),
            })?;
    let writes_only = args
        .get("writes_only")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let limit = args
        .get("limit")
        .and_then(serde_json::Value::as_u64)
        .map_or(200, |v| v.clamp(1, 2000) as usize);

    let (qualifier, field_name) = match raw.rsplit_once("::") {
        Some((q, f)) => (Some(q.to_string()), f.to_string()),
        None => (None, raw.to_string()),
    };

    let project_root = cg.project_root();
    let files = cg.get_all_files().await?;
    let mut writes: Vec<Value> = Vec::new();
    let mut reads: Vec<Value> = Vec::new();
    let mut touched: Vec<String> = Vec::new();

    'outer: for file in &files {
        if !path_matches_optional_scope(&file.path, scope_prefix) {
            continue;
        }
        let abs = project_root.join(&file.path);
        let Ok(source) = crate::sync::read_source_file(&abs) else {
            continue;
        };

        // Cheap textual pre-filter before any per-file store read. Most files
        // in a repository never mention the field, and fetching their nodes
        // anyway cost one daemon round trip per file in the project — O(store)
        // work to answer a question whose result is a handful of sites.
        let sites = find_field_references(&source, &field_name);
        if sites.is_empty() {
            continue;
        }
        let nodes = cg.get_nodes_by_file(&file.path).await?;

        for site in sites {
            let line_text = line_at(&source, site.byte).unwrap_or("");
            let enclosing = nodes
                .iter()
                .filter(|n| n.start_line <= site.line && site.line <= n.end_line)
                .min_by_key(|n| n.end_line.saturating_sub(n.start_line))
                .map(|n| n.qualified_name.clone());
            let entry = json!({
                "file": file.path,
                "line": site.line,
                "enclosing": enclosing,
                "snippet": line_text.trim(),
            });
            if !touched.contains(&file.path) {
                touched.push(file.path.clone());
            }
            match site.kind {
                FieldRefKind::Write => {
                    writes.push(entry);
                    if writes.len() >= limit && (writes_only || reads.len() >= limit) {
                        break 'outer;
                    }
                }
                FieldRefKind::Read => {
                    if writes_only {
                        continue;
                    }
                    reads.push(entry);
                    if reads.len() >= limit && writes.len() >= limit {
                        break 'outer;
                    }
                }
            }
        }
    }

    let qualifier_applied = false;
    let payload = if writes_only {
        json!({
            "field": raw,
            "qualifier": qualifier,
            "qualifier_applied": qualifier_applied,
            "write_count": writes.len(),
            "write_sites": writes,
        })
    } else {
        json!({
            "field": raw,
            "qualifier": qualifier,
            "qualifier_applied": qualifier_applied,
            "write_count": writes.len(),
            "read_count": reads.len(),
            "write_sites": writes,
            "read_sites": reads,
        })
    };
    Ok(generic_tool_result(
        Some(cg.project_root()),
        &args,
        &payload,
        touched,
    ))
}

#[derive(Debug, Clone, Copy)]
enum FieldRefKind {
    Read,
    Write,
}

#[derive(Debug, Clone, Copy)]
struct FieldSite {
    byte: usize,
    line: u32,
    kind: FieldRefKind,
}

fn find_field_references(source: &str, field: &str) -> Vec<FieldSite> {
    let bytes = source.as_bytes();
    let needle = format!(".{field}");
    let mut out: Vec<FieldSite> = Vec::new();
    let mut byte = 0usize;
    while let Some(rel) = source[byte..].find(&needle) {
        let dot = byte + rel;
        let name_start = dot + 1;
        let name_end = name_start + field.len();
        let right_ok = !bytes.get(name_end).copied().is_some_and(is_ident_byte);
        if !right_ok {
            byte = name_end;
            continue;
        }
        if line_is_comment(source, dot) {
            byte = name_end;
            continue;
        }

        out.push(FieldSite {
            byte: name_end,
            line: line_number_at(source, dot),
            kind: classify_field_reference(source, name_end),
        });
        byte = name_end;
    }
    out
}

fn classify_field_reference(source: &str, after_name: usize) -> FieldRefKind {
    let bytes = source.as_bytes();
    let mut probe = after_name;
    while let Some(b) = bytes.get(probe) {
        if *b == b' ' || *b == b'\t' {
            probe += 1;
        } else {
            break;
        }
    }

    if let Some(b'\n') = bytes.get(probe).copied() {
        probe += 1;
        while let Some(b) = bytes.get(probe) {
            if *b == b' ' || *b == b'\t' {
                probe += 1;
            } else {
                break;
            }
        }
    }

    let next = bytes.get(probe).copied();
    let next2 = bytes.get(probe + 1).copied();
    match (next, next2) {
        (Some(b'='), Some(b'=' | b'>')) => FieldRefKind::Read,
        (Some(b'='), _) => FieldRefKind::Write,
        (Some(b'+' | b'-' | b'*' | b'/' | b'%' | b'&' | b'|' | b'^'), Some(b'=')) => {
            FieldRefKind::Write
        }
        (Some(b'<'), Some(b'<')) | (Some(b'>'), Some(b'>')) => {
            if bytes.get(probe + 2).copied() == Some(b'=') {
                FieldRefKind::Write
            } else {
                FieldRefKind::Read
            }
        }
        _ => {
            if has_mut_borrow_prefix(source, after_name.saturating_sub(1)) {
                FieldRefKind::Write
            } else {
                FieldRefKind::Read
            }
        }
    }
}

fn has_mut_borrow_prefix(source: &str, idx: usize) -> bool {
    let bytes = source.as_bytes();
    let mut probe = idx;
    while probe > 0 && (is_ident_byte(bytes[probe]) || matches!(bytes[probe], b'.' | b':' | b'?')) {
        probe -= 1;
    }
    while probe > 0 && bytes[probe].is_ascii_whitespace() {
        probe -= 1;
    }
    if probe < 4 {
        return false;
    }
    let window = &source[probe.saturating_sub(4)..=probe];
    window.ends_with("&mut")
}

fn line_at(source: &str, byte: usize) -> Option<&str> {
    let line_start = source[..byte].rfind('\n').map_or(0, |i| i + 1);
    let line_end = source[byte..].find('\n').map_or(source.len(), |i| byte + i);
    source.get(line_start..line_end)
}

fn line_is_comment(source: &str, byte: usize) -> bool {
    let line_start = source[..byte].rfind('\n').map_or(0, |i| i + 1);
    let line = &source[line_start..];
    let trimmed = line.trim_start();
    trimmed.starts_with("//")
}
