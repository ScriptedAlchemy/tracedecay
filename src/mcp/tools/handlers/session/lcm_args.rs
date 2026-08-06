use super::*;
use tracedecay_domain::{TemporalModeV1, UtcMicros};

pub(super) const DEFAULT_LCM_CONTENT_LIMIT: usize = 4096;
pub(super) const DEFAULT_LCM_EXPAND_QUERY_CONTEXT_LIMIT: usize = 32_000;
pub(super) const MAX_LCM_EXPAND_QUERY_CONTEXT_LIMIT: usize = 65_536;
pub(super) const MAX_LCM_CONTENT_LIMIT: usize = 8192;
pub(super) const MAX_LCM_LOAD_CONTENT_LIMIT: usize = 20_000;
pub(super) const MAX_LCM_RESULT_LIMIT: usize = 100;

pub(super) fn required_string_arg<'a>(args: &'a Value, name: &str) -> Result<&'a str> {
    string_arg(args, name).ok_or_else(|| TraceDecayError::Config {
        message: format!("missing required parameter: {name}"),
    })
}

pub(super) fn optional_non_empty_string_arg<'a>(
    args: &'a Value,
    name: &str,
) -> Result<Option<&'a str>> {
    let Some(value) = args.get(name) else {
        return Ok(None);
    };
    value
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(Some)
        .ok_or_else(|| argument_error(format!("{name} must be a non-empty string")))
}

pub(super) fn bounded_usize_arg(
    args: &Value,
    name: &str,
    min: usize,
    max: usize,
) -> Result<Option<usize>> {
    let Some(value) = args.get(name) else {
        return Ok(None);
    };
    let Some(integer) = value.as_i64() else {
        return Err(argument_error(format!("{name} must be an integer")));
    };
    if integer < 0 {
        return Err(argument_error(format!("{name} must be >= {min}")));
    }
    let integer =
        usize::try_from(integer).map_err(|_| argument_error(format!("{name} must be <= {max}")))?;
    if integer < min {
        return Err(argument_error(format!("{name} must be >= {min}")));
    }
    if integer > max {
        return Err(argument_error(format!("{name} must be <= {max}")));
    }
    Ok(Some(integer))
}

pub(super) fn non_negative_i64_arg(args: &Value, name: &str) -> Result<Option<i64>> {
    let Some(value) = args.get(name) else {
        return Ok(None);
    };
    let Some(integer) = value.as_i64() else {
        return Err(argument_error(format!("{name} must be an integer")));
    };
    if integer < 0 {
        return Err(argument_error(format!("{name} must be >= 0")));
    }
    Ok(Some(integer))
}

pub(super) fn signed_i64_arg(args: &Value, name: &str) -> Result<Option<i64>> {
    let Some(value) = args.get(name) else {
        return Ok(None);
    };
    value
        .as_i64()
        .map(Some)
        .ok_or_else(|| argument_error(format!("{name} must be an integer")))
}

pub(super) fn bool_arg(args: &Value, name: &str) -> Result<Option<bool>> {
    let Some(value) = args.get(name) else {
        return Ok(None);
    };
    value
        .as_bool()
        .map(Some)
        .ok_or_else(|| argument_error(format!("{name} must be a boolean")))
}

pub(super) fn non_negative_i64_arg_alias(
    args: &Value,
    primary: &str,
    alias: &str,
) -> Result<Option<i64>> {
    match non_negative_i64_arg(args, primary)? {
        Some(value) => Ok(Some(value)),
        None => non_negative_i64_arg(args, alias),
    }
}

pub(super) fn non_negative_timestamp_arg_aliases(
    args: &Value,
    names: &[&str],
    bound: SearchTimeBound,
) -> Result<Option<i64>> {
    for name in names {
        if args.get(name).is_some() {
            return non_negative_timestamp_arg(args, name, bound);
        }
    }
    Ok(None)
}

pub(super) fn non_negative_timestamp_arg(
    args: &Value,
    name: &str,
    bound: SearchTimeBound,
) -> Result<Option<i64>> {
    let Some(value) = args.get(name) else {
        return Ok(None);
    };
    let timestamp = match value {
        Value::Number(number) => number
            .as_i64()
            .ok_or_else(|| timestamp_argument_error(name))?,
        Value::String(text) => parse_timestamp_string(text, name, bound)?,
        _ => return Err(timestamp_argument_error(name)),
    };
    if timestamp < 0 {
        return Err(argument_error(format!("{name} must be >= 0")));
    }
    Ok(Some(timestamp))
}

pub(super) fn parse_timestamp_string(
    value: &str,
    name: &str,
    bound: SearchTimeBound,
) -> Result<i64> {
    let text = value.trim();
    if text.is_empty() {
        return Err(argument_error(format!("{name} must not be empty")));
    }
    if let Ok(timestamp) = text.parse::<i64>() {
        if timestamp >= 0 {
            return Ok(timestamp);
        }
        return Err(argument_error(format!("{name} must be >= 0")));
    }
    let now = crate::tracedecay::current_timestamp();
    crate::timeutil::parse_search_time_filter_bound(text, now, bound)
        .ok_or_else(|| timestamp_argument_error(name))
}

pub(super) fn message_search_time_range(args: &Value) -> Result<SessionSearchTimeRange> {
    Ok(SessionSearchTimeRange {
        start_time: non_negative_timestamp_arg_aliases(
            args,
            &["since", "start_time", "time_from"],
            SearchTimeBound::Start,
        )?,
        end_time: non_negative_timestamp_arg_aliases(
            args,
            &["until", "end_time", "time_to"],
            SearchTimeBound::End,
        )?,
    })
}

pub(super) fn timestamp_argument_error(name: &str) -> TraceDecayError {
    argument_error(format!(
        "{name} must be a non-negative Unix timestamp, timezone-aware ISO/RFC3339 string, YYYY-MM-DD date, or relative time like 'last hour'"
    ))
}

pub(super) fn provider_or_all_arg(args: &Value) -> Result<&str> {
    Ok(optional_search_provider_arg(args)?.unwrap_or("all"))
}

pub(super) fn required_specific_provider_arg(args: &Value) -> Result<&str> {
    match string_arg(args, "provider") {
        Some("all") => Err(argument_error(
            "provider must name a specific provider for this tool",
        )),
        Some(provider) => Ok(provider),
        None => Err(argument_error("provider is required for this tool")),
    }
}

pub(super) fn optional_search_provider_arg(args: &Value) -> Result<Option<&str>> {
    Ok(optional_non_empty_string_arg(args, "provider")?
        .filter(|provider| !provider.is_empty() && *provider != "all"))
}

pub(super) fn lcm_cursor_arg(args: &Value) -> Result<Option<String>> {
    if args.get("after_store_id").is_some() {
        return Err(argument_error(
            "after_store_id is no longer supported; use the opaque cursor returned as next_cursor",
        ));
    }
    let cursor = match args.get("cursor") {
        None => None,
        Some(Value::String(cursor)) if !cursor.trim().is_empty() => Some(cursor.clone()),
        Some(_) => return Err(argument_error("cursor must be a non-empty opaque string")),
    };
    Ok(cursor)
}

pub(super) fn lcm_roles_arg(args: &Value) -> Result<Vec<String>> {
    let mut roles = match args.get("roles") {
        None => Vec::new(),
        Some(Value::Array(values)) => values
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string)
                    .ok_or_else(|| argument_error("roles must contain only non-empty strings"))
            })
            .collect::<Result<Vec<_>>>()?,
        Some(_) => return Err(argument_error("roles must be an array")),
    };
    if let Some(role) = optional_non_empty_string_arg(args, "role")?
        && !roles.iter().any(|candidate| candidate == role)
    {
        roles.push(role.to_string());
    }
    Ok(roles)
}

pub(super) fn lcm_temporal_mode(args: &Value, default: TemporalModeV1) -> Result<TemporalModeV1> {
    match optional_non_empty_string_arg(args, "temporal_mode")? {
        None => Ok(default),
        Some("current") => Ok(TemporalModeV1::Current),
        Some("forensic") => Ok(TemporalModeV1::Forensic),
        Some("evolution") => Ok(TemporalModeV1::Evolution),
        Some("as_of") => {
            let cutoff = non_negative_i64_arg(args, "as_of_micros")?.ok_or_else(|| {
                argument_error("as_of_micros is required when temporal_mode=as_of")
            })?;
            Ok(TemporalModeV1::AsOf {
                cutoff: UtcMicros(cutoff),
            })
        }
        Some(_) => Err(argument_error(
            "temporal_mode must be one of current, as_of, evolution, forensic",
        )),
    }
}

pub(super) fn messages_arg(args: &Value) -> Result<Vec<Value>> {
    let Some(messages) = args.get("messages") else {
        return Ok(Vec::new());
    };
    let Some(messages) = messages.as_array() else {
        return Err(argument_error("messages must be an array"));
    };
    Ok(messages.clone())
}

pub(super) fn string_array_arg(args: &Value, name: &str) -> Result<Vec<String>> {
    let Some(value) = args.get(name) else {
        return Ok(Vec::new());
    };
    let Some(values) = value.as_array() else {
        return Err(argument_error(format!("{name} must be an array")));
    };
    values
        .iter()
        .map(|value| {
            if let Some(text) = value
                .as_str()
                .map(str::trim)
                .filter(|text| !text.is_empty())
            {
                return Ok(text.to_string());
            }
            if let Some(integer) = value.as_i64()
                && integer >= 0
            {
                return Ok(integer.to_string());
            }
            Err(argument_error(format!(
                "{name} must contain only non-empty strings or non-negative integers"
            )))
        })
        .collect()
}

pub(super) fn string_only_array_arg(args: &Value, name: &str) -> Result<Vec<String>> {
    let Some(value) = args.get(name) else {
        return Ok(Vec::new());
    };
    let values = value
        .as_array()
        .ok_or_else(|| argument_error(format!("{name} must be an array")))?;
    values
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .ok_or_else(|| {
                    argument_error(format!("{name} must contain only non-empty strings"))
                })
        })
        .collect()
}

/// The summarizer the caller asked for, taken at face value.
///
/// An explicit `summarizer: "noop"` used to be silently rewritten to the
/// auxiliary summarizer under hard compression pressure, which made the tool
/// do something the caller never requested. The request is now honored and the
/// pressure is reported through [`summarizer_pressure_advisory`] instead.
pub(super) fn summarizer_arg(args: &Value) -> Result<LcmSummarizerMode> {
    match args.get("summarizer") {
        Some(summarizer) => {
            serde_json::from_value(summarizer.clone()).map_err(|err| TraceDecayError::Config {
                message: format!("invalid summarizer: {err}"),
            })
        }
        None => Ok(LcmSummarizerMode::HermesAuxiliary),
    }
}

/// Typed advisory for an explicit no-op summarizer requested while the session
/// is already under hard compression pressure. `None` means there is nothing
/// to advise.
pub(super) fn summarizer_pressure_advisory(args: &Value) -> Result<Option<Value>> {
    let mode = summarizer_arg(args)?;
    if !matches!(mode, LcmSummarizerMode::Noop) || !hard_compression_pressure(args)? {
        return Ok(None);
    }
    Ok(Some(serde_json::json!({
        "code": "noop_summarizer_under_hard_pressure",
        "message": "summarizer 'noop' was honored while the session is over its compression threshold; no summaries will be produced",
        "requested_summarizer": "noop",
        "recommended_summarizer": "hermes_auxiliary",
    })))
}

pub(super) fn hard_compression_pressure(args: &Value) -> Result<bool> {
    let Some(current_tokens) = non_negative_i64_arg(args, "current_tokens")? else {
        return Ok(false);
    };
    if non_negative_i64_arg(args, "threshold_tokens")?
        .is_some_and(|threshold| threshold > 0 && current_tokens >= threshold)
    {
        return Ok(true);
    }
    let assembly_cap = compression_decision::effective_assembly_token_cap(AssemblyCapInput {
        max_assembly_tokens: non_negative_i64_arg(args, "max_assembly_tokens")?,
        context_length: non_negative_i64_arg(args, "context_length")?,
        reserve_tokens_floor: non_negative_i64_arg(args, "reserve_tokens_floor")?,
    });
    Ok(assembly_cap.is_some_and(|cap| current_tokens >= cap))
}

pub(super) fn lcm_content_slice(args: &Value) -> Result<LcmContentSlice> {
    Ok(LcmContentSlice {
        offset: bounded_usize_arg(args, "content_offset", 0, usize::MAX)?.unwrap_or(0),
        limit: bounded_usize_arg(args, "content_limit", 1, MAX_LCM_CONTENT_LIMIT)?
            .unwrap_or(DEFAULT_LCM_CONTENT_LIMIT),
    })
}

pub(super) fn lcm_load_content_slice(args: &Value) -> Result<(LcmContentSlice, Option<usize>)> {
    let offset = bounded_usize_arg(args, "content_offset", 0, usize::MAX)?.unwrap_or(0);
    let requested_limit = match args.get("content_limit") {
        Some(value) => {
            let Some(integer) = value.as_i64() else {
                return Err(argument_error("content_limit must be an integer"));
            };
            if integer <= 0 {
                return Err(argument_error("content_limit must be >= 1"));
            }
            usize::try_from(integer).map_err(|_| {
                argument_error(format!(
                    "content_limit must be <= {MAX_LCM_LOAD_CONTENT_LIMIT}"
                ))
            })?
        }
        None => DEFAULT_LCM_CONTENT_LIMIT,
    };
    let limit = requested_limit.min(MAX_LCM_LOAD_CONTENT_LIMIT);
    let clamped_from = (requested_limit > limit).then_some(requested_limit);
    Ok((LcmContentSlice { offset, limit }, clamped_from))
}

pub(super) fn lcm_gc_config(args: &Value) -> Result<LcmGcConfig> {
    match args.get("gc_config") {
        Some(value) => serde_json::from_value::<LcmGcConfig>(value.clone()).map_err(|err| {
            argument_error(format!(
                "gc_config must be a valid LcmGcConfig object: {err}"
            ))
        }),
        None => Ok(LcmGcConfig::default()),
    }
}

// By-value so it can be used point-free as a `map_err` adapter.
#[allow(clippy::needless_pass_by_value)]
pub(super) fn lcm_error(err: crate::sessions::lcm::LcmError) -> TraceDecayError {
    TraceDecayError::Config {
        message: err.to_string(),
    }
}

pub(super) fn parse_lcm_scope(args: &Value) -> Result<LcmScope> {
    let Some(value) = args.get("scope") else {
        return Ok(LcmScope::All);
    };
    let Some(scope) = value
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Err(argument_error("scope must be one of current, session, all"));
    };
    match scope {
        "current" => Ok(LcmScope::Current),
        "session" => Ok(LcmScope::Session),
        "all" => Ok(LcmScope::All),
        _ => Err(argument_error("scope must be one of current, session, all")),
    }
}

pub(super) fn lcm_grep_provider_arg(args: &Value) -> Result<&str> {
    Ok(optional_search_provider_arg(args)?.unwrap_or("all"))
}

pub(super) fn parse_lcm_grep_sort(args: &Value) -> Result<LcmGrepSort> {
    let Some(value) = args.get("sort") else {
        // Term-bearing queries default to relevance (FTS rank primary, recency
        // as tiebreak) so distinct queries do not all collapse onto the same
        // few most-recent sessions. Pass `sort` explicitly for recency/hybrid.
        return Ok(LcmGrepSort::Relevance);
    };
    let sort = value
        .as_str()
        .ok_or_else(|| argument_error("sort must be a string"))?;
    sort.parse::<LcmGrepSort>()
        .map_err(|()| argument_error("sort must be one of recency, relevance, hybrid"))
}

pub(super) fn parse_lcm_summary_node_id(target: &Value) -> Result<String> {
    required_string_arg(target, "node_id")
        .map(str::to_string)
        .map_err(|_| TraceDecayError::Config {
            message: "target.node_id is required when target.kind is summary_node".to_string(),
        })
}

pub(super) fn parse_lcm_external_payload_ref(target: &Value) -> Result<String> {
    required_string_arg(target, "payload_ref")
        .map(str::to_string)
        .map_err(|_| TraceDecayError::Config {
            message: "target.payload_ref is required when target.kind is external_payload"
                .to_string(),
        })
}

fn validate_lcm_target_keys(target: &Value, allowed: &[&str]) -> Result<()> {
    let object = target
        .as_object()
        .ok_or_else(|| argument_error("target must be an object"))?;
    if let Some(key) = object.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(argument_error(format!(
            "target.{key} is not valid when target.kind is {}",
            string_arg(target, "kind").unwrap_or("<missing>")
        )));
    }
    Ok(())
}

pub(super) fn parse_lcm_describe_target(args: &Value) -> Result<LcmDescribeTarget> {
    let Some(target) = args.get("target") else {
        return Ok(LcmDescribeTarget::Session);
    };
    match string_arg(target, "kind").unwrap_or_default() {
        "summary_node" => {
            validate_lcm_target_keys(target, &["kind", "node_id"])?;
            Ok(LcmDescribeTarget::SummaryNode {
                node_id: parse_lcm_summary_node_id(target)?,
            })
        }
        "external_payload" => {
            validate_lcm_target_keys(target, &["kind", "payload_ref"])?;
            Ok(LcmDescribeTarget::ExternalPayload {
                payload_ref: parse_lcm_external_payload_ref(target)?,
            })
        }
        "session" => {
            validate_lcm_target_keys(target, &["kind"])?;
            Ok(LcmDescribeTarget::Session)
        }
        _ => Err(TraceDecayError::Config {
            message: "target.kind must be one of session, summary_node, external_payload"
                .to_string(),
        }),
    }
}

pub(super) fn parse_lcm_expand_target(args: &Value) -> Result<LcmExpandTarget> {
    let target = args.get("target").ok_or_else(|| TraceDecayError::Config {
        message: "missing required parameter: target".to_string(),
    })?;
    match string_arg(target, "kind").unwrap_or_default() {
        "raw_message" => {
            validate_lcm_target_keys(target, &["kind", "store_id"])?;
            let store_id = non_negative_i64_arg(target, "store_id")?.ok_or_else(|| {
                TraceDecayError::Config {
                    message: "target.store_id is required when target.kind is raw_message"
                        .to_string(),
                }
            })?;
            Ok(LcmExpandTarget::RawMessage { store_id })
        }
        "summary_node" => {
            validate_lcm_target_keys(target, &["kind", "node_id"])?;
            Ok(LcmExpandTarget::SummaryNode {
                node_id: parse_lcm_summary_node_id(target)?,
            })
        }
        "external_payload" => {
            validate_lcm_target_keys(target, &["kind", "payload_ref"])?;
            Ok(LcmExpandTarget::ExternalPayload {
                payload_ref: parse_lcm_external_payload_ref(target)?,
            })
        }
        _ => Err(TraceDecayError::Config {
            message: "target.kind must be one of raw_message, summary_node, external_payload"
                .to_string(),
        }),
    }
}

/// Parses the `scope` argument for `tracedecay_message_search`. Like
/// [`parse_lcm_scope`], invalid values are a hard error naming the valid set —
/// never silently broadened to `all`.
pub(super) fn parse_message_search_scope(args: &Value) -> Result<SessionSearchScope> {
    let Some(value) = args.get("scope") else {
        return Ok(SessionSearchScope::All);
    };
    value
        .as_str()
        .and_then(SessionSearchScope::parse)
        .ok_or_else(|| argument_error("scope must be one of all, parents_only, subagents_only"))
}

pub(super) fn parse_session_message_type(args: &Value) -> Result<SessionMessageType> {
    let Some(value) = args.get("message_type") else {
        return Ok(SessionMessageType::All);
    };
    value
        .as_str()
        .and_then(SessionMessageType::parse)
        .ok_or_else(|| argument_error("message_type must be one of all, direct_user, tool_result"))
}

pub(super) fn parse_lcm_relationship_scope(args: &Value) -> Result<SessionSearchScope> {
    let Some(value) = args.get("relationship_scope") else {
        return Ok(SessionSearchScope::All);
    };
    value
        .as_str()
        .and_then(SessionSearchScope::parse)
        .ok_or_else(|| {
            argument_error("relationship_scope must be one of all, parents_only, subagents_only")
        })
}

pub(super) fn parse_message_search_provider_scope(args: &Value) -> Result<ProviderScope> {
    let provider = match args.get("provider") {
        None => None,
        Some(value) => Some(
            value
                .as_str()
                .ok_or_else(|| argument_error("provider must be a string"))?,
        ),
    };
    ProviderScope::parse_optional(provider).map_err(argument_error)
}

/// Parses the optional `branch` / `worktree` / `commit` git-scope filter
/// arguments shared by `tracedecay_message_search` and `tracedecay_lcm_grep`.
pub(super) fn parse_git_scope_filter(args: &Value) -> Result<GitScopeFilter> {
    GitScopeFilter::from_args(
        optional_non_empty_string_arg(args, "branch")?,
        optional_non_empty_string_arg(args, "worktree")?,
        optional_non_empty_string_arg(args, "commit")?,
    )
    .map_err(|err| argument_error(err.to_string()))
}
