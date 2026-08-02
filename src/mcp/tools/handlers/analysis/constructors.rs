//! `tracedecay_constructors` — struct-literal construction sites and the fields each one sets.

use super::*;

pub(crate) async fn handle_constructors(
    cg: &TraceDecay,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    let struct_name =
        args.get("struct")
            .and_then(|v| v.as_str())
            .ok_or_else(|| TraceDecayError::Config {
                message: "tracedecay_constructors requires a 'struct' argument".to_string(),
            })?;
    let limit = args
        .get("limit")
        .and_then(serde_json::Value::as_u64)
        .map_or(100, |v| v.clamp(1, 1000) as usize);

    let candidates = cg
        .db()
        .search_nodes_by_exact_name(&[struct_name.to_string()], 50)
        .await?;
    let struct_nodes: Vec<&crate::types::Node> = candidates
        .iter()
        .filter(|n| {
            matches!(
                n.kind,
                NodeKind::Struct | NodeKind::Class | NodeKind::CaseClass
            )
        })
        .collect();

    if struct_nodes.is_empty() {
        let payload = json!({
            "found": false,
            "struct": struct_name,
            "message": format!("No struct, class, or case-class named '{struct_name}' found."),
            "match_count": 0,
            "sites": [],
        });
        return Ok(generic_tool_result(
            Some(cg.project_root()),
            &args,
            &payload,
            vec![],
        ));
    }

    let mut expected_fields: HashSet<String> = HashSet::new();
    for sn in &struct_nodes {
        let children = cg.db().get_children_of(&sn.id).await?;
        for child in children {
            if matches!(
                child.kind,
                NodeKind::Field | NodeKind::ValField | NodeKind::VarField
            ) {
                expected_fields.insert(child.name);
            }
        }
    }

    let project_root = cg.project_root();
    let files = cg.get_all_files().await?;

    // Reading and parsing every source file in the project is a long CPU and
    // I/O slice with no await points. Running it inline pinned a request
    // runtime worker for the whole scan — tens of seconds on a large
    // repository — which is exactly what starves other interactive calls. The
    // scan is self-contained, so it belongs on a blocking thread.
    let scan_paths: Vec<String> = files
        .iter()
        .filter(|file| path_matches_optional_scope(&file.path, scope_prefix))
        .map(|file| file.path.clone())
        .collect();
    let scan_root = project_root.to_path_buf();
    let scan_struct = struct_name.to_string();
    let scan_fields = expected_fields.clone();

    let (sites, touched) = tokio::task::spawn_blocking(move || {
        let mut sites: Vec<Value> = Vec::new();
        let mut touched: Vec<String> = Vec::new();

        'outer: for path in &scan_paths {
            let abs = scan_root.join(path);
            let Ok(source) = crate::sync::read_source_file(&abs) else {
                continue;
            };

            for site in find_struct_literals(&source, &scan_struct) {
                let field_list = parse_literal_fields(&source, site.brace_open_byte);
                let missing: Vec<String> = if scan_fields.is_empty() {
                    Vec::new()
                } else {
                    scan_fields
                        .iter()
                        .filter(|f| !field_list.contains(f))
                        .cloned()
                        .collect()
                };
                if !touched.contains(path) {
                    touched.push(path.clone());
                }
                sites.push(json!({
                    "file": path,
                    "line": site.line,
                    "fields": field_list,
                    "missing_fields": missing,
                }));
                if sites.len() >= limit {
                    break 'outer;
                }
            }
        }

        (sites, touched)
    })
    .await
    .map_err(|e| TraceDecayError::Config {
        message: format!("tracedecay_constructors scan failed to join: {e}"),
    })?;

    let payload = json!({
        "struct": struct_name,
        "expected_fields": expected_fields.iter().cloned().collect::<Vec<_>>(),
        "match_count": sites.len(),
        "sites": sites,
    });
    Ok(generic_tool_result(
        Some(cg.project_root()),
        &args,
        &payload,
        touched,
    ))
}

#[derive(Debug, Clone, Copy)]
struct LiteralSite {
    line: u32,
    brace_open_byte: usize,
}

fn find_struct_literals(source: &str, struct_name: &str) -> Vec<LiteralSite> {
    let bytes = source.as_bytes();
    let mut pattern_stack: Vec<i32> = Vec::new();
    let mut depth: i32 = 0;
    let mut string_delim: Option<u8> = None;
    let mut prev_was_backslash = false;
    let mut out: Vec<LiteralSite> = Vec::new();
    let mut byte = 0usize;
    let n = bytes.len();
    while byte < n {
        let b = bytes[byte];

        if let Some(delim) = string_delim {
            if !prev_was_backslash && b == delim {
                string_delim = None;
                prev_was_backslash = false;
                byte += 1;
                continue;
            }
            prev_was_backslash = !prev_was_backslash && b == b'\\';
            byte += 1;
            continue;
        }
        if b == b'"' {
            string_delim = Some(b'"');
            prev_was_backslash = false;
            byte += 1;
            continue;
        }
        if b == b'\'' {
            let after = bytes.get(byte + 1).copied();
            if matches!(after, Some(b'a'..=b'z' | b'A'..=b'Z' | b'_')) {
                let mut probe = byte + 1;
                while bytes.get(probe).copied().is_some_and(is_ident_byte) {
                    probe += 1;
                }
                if bytes.get(probe).copied() != Some(b'\'') {
                    byte += 1;
                    continue;
                }
            }
            string_delim = Some(b'\'');
            prev_was_backslash = false;
            byte += 1;
            continue;
        }

        if matches_word(bytes, byte, b"match") {
            pattern_stack.push(depth);
            byte += "match".len();
            continue;
        }
        if matches_word(bytes, byte, b"if") && lookahead_let(bytes, byte + 2) {
            pattern_stack.push(depth);
            byte += "if".len();
            continue;
        }
        if matches_word(bytes, byte, b"while") && lookahead_let(bytes, byte + 5) {
            pattern_stack.push(depth);
            byte += "while".len();
            continue;
        }

        if b == b'{' {
            depth += 1;
            byte += 1;
            continue;
        }
        if b == b'}' {
            depth -= 1;
            if let Some(&entered_at) = pattern_stack.last()
                && depth == entered_at
            {
                pattern_stack.pop();
            }
            byte += 1;
            continue;
        }

        if matches_word(bytes, byte, struct_name.as_bytes()) {
            let start = byte;
            let end = start + struct_name.len();

            let probe = skip_ascii_whitespace(bytes, end);
            if bytes.get(probe).copied() != Some(b'{') {
                byte = end;
                continue;
            }
            if has_disqualifying_prefix(source, start) {
                byte = end;
                continue;
            }
            if !pattern_stack.is_empty() {
                byte = end;
                continue;
            }
            out.push(LiteralSite {
                line: line_number_at(source, start),
                brace_open_byte: probe,
            });
            byte = probe + 1;
            continue;
        }

        byte += 1;
    }
    out
}

fn lookahead_let(bytes: &[u8], at: usize) -> bool {
    matches_word(bytes, skip_ascii_whitespace(bytes, at), b"let")
}

fn matches_word(bytes: &[u8], at: usize, needle: &[u8]) -> bool {
    if at + needle.len() > bytes.len() {
        return false;
    }
    if &bytes[at..at + needle.len()] != needle {
        return false;
    }
    let left_ok = at == 0 || !is_ident_byte(bytes[at - 1]);
    let right_ok = !bytes
        .get(at + needle.len())
        .copied()
        .is_some_and(is_ident_byte);
    left_ok && right_ok
}

fn has_disqualifying_prefix(source: &str, idx: usize) -> bool {
    let bytes = source.as_bytes();
    let mut probe = idx;
    while probe > 0 && bytes[probe - 1].is_ascii_whitespace() {
        probe -= 1;
    }
    if probe == 0 {
        return false;
    }
    if probe >= 2 && &bytes[probe - 2..probe] == b"->" {
        return true;
    }
    let id_end = probe;
    let mut id_start = probe;
    while id_start > 0 && is_ident_byte(bytes[id_start - 1]) {
        id_start -= 1;
    }
    if id_start == id_end {
        return false;
    }
    let token = &source[id_start..id_end];
    matches!(
        token,
        "struct" | "enum" | "union" | "impl" | "trait" | "type"
    )
}

fn parse_literal_fields(source: &str, open_byte: usize) -> Vec<String> {
    let bytes = source.as_bytes();
    if bytes.get(open_byte).copied() != Some(b'{') {
        return Vec::new();
    }
    let mut depth = 0i32;
    let mut close_byte = None;
    for (i, b) in bytes.iter().enumerate().skip(open_byte) {
        match b {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    close_byte = Some(i);
                    break;
                }
            }
            _ => {}
        }
    }
    let Some(close) = close_byte else {
        return Vec::new();
    };
    let body = &source[open_byte + 1..close];

    let mut fields: Vec<String> = Vec::new();
    let mut depth_brace = 0i32;
    let mut depth_paren = 0i32;
    let mut current = String::new();
    for c in body.chars() {
        match c {
            '{' | '[' => depth_brace += 1,
            '}' | ']' => depth_brace -= 1,
            '(' => depth_paren += 1,
            ')' => depth_paren -= 1,
            ',' if depth_brace == 0 && depth_paren == 0 => {
                if let Some(name) = field_name_from_chunk(&current) {
                    fields.push(name);
                }
                current.clear();
                continue;
            }
            _ => {}
        }
        current.push(c);
    }
    if let Some(name) = field_name_from_chunk(&current) {
        fields.push(name);
    }
    fields
}

fn field_name_from_chunk(chunk: &str) -> Option<String> {
    let trimmed = chunk.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.starts_with("..") || trimmed.starts_with("//") {
        return None;
    }
    let name_end = trimmed
        .find(|c: char| c == ':' || c == ',' || c.is_whitespace())
        .unwrap_or(trimmed.len());
    let name = &trimmed[..name_end];
    if name.is_empty() {
        return None;
    }
    if !name
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
    {
        return None;
    }
    Some(name.to_string())
}
