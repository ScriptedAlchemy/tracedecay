use super::super::ports::ExecutionControl;
use super::{ContextError, TokenPolicy};

pub const TOKEN_SCAN_CHUNK_BYTES: usize = 4 * 1024;
const MAX_TOKEN_PATTERN_BYTES: usize = 64;
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TokenSummary {
    Whitespace {
        tokens: u64,
        starts_token: bool,
        ends_token: bool,
        empty: bool,
    },
    Characters(u64),
    Substring {
        pattern: &'static str,
        matches: u64,
        prefix: [u8; MAX_TOKEN_PATTERN_BYTES],
        prefix_len: usize,
        suffix: [u8; MAX_TOKEN_PATTERN_BYTES],
        suffix_len: usize,
        total_len: u64,
    },
    JsonDocument {
        first: Option<char>,
        last: Option<char>,
    },
}

impl TokenSummary {
    pub fn empty(policy: TokenPolicy) -> Result<Self, ContextError> {
        match policy {
            TokenPolicy::Whitespace => Ok(Self::Whitespace {
                tokens: 0,
                starts_token: false,
                ends_token: false,
                empty: true,
            }),
            TokenPolicy::Characters => Ok(Self::Characters(0)),
            TokenPolicy::Substring(pattern) => {
                validate_token_pattern(pattern)?;
                Ok(Self::Substring {
                    pattern,
                    matches: 0,
                    prefix: [0; MAX_TOKEN_PATTERN_BYTES],
                    prefix_len: 0,
                    suffix: [0; MAX_TOKEN_PATTERN_BYTES],
                    suffix_len: 0,
                    total_len: 0,
                })
            }
            TokenPolicy::JsonDocument => Ok(Self::JsonDocument {
                first: None,
                last: None,
            }),
        }
    }

    pub fn scan(
        policy: TokenPolicy,
        fragment: &str,
        control: &ExecutionControl,
    ) -> Result<Self, ContextError> {
        let mut summary = Self::empty(policy)?;
        match &mut summary {
            Self::Whitespace {
                tokens,
                starts_token,
                ends_token,
                empty,
            } => {
                let mut in_token = false;
                let mut first = true;
                let mut scanned = 0_usize;
                for character in fragment.chars() {
                    scanned = scanned.saturating_add(character.len_utf8());
                    if scanned >= TOKEN_SCAN_CHUNK_BYTES {
                        control.checkpoint()?;
                        scanned = 0;
                    }
                    let token = !character.is_whitespace();
                    if first {
                        *starts_token = token;
                        first = false;
                    }
                    if token && !in_token {
                        *tokens = tokens
                            .checked_add(1)
                            .ok_or(ContextError::BudgetExceeded { resource: "token" })?;
                    }
                    in_token = token;
                    *ends_token = token;
                }
                *empty = first;
            }
            Self::Characters(count) => {
                let mut scanned = 0_usize;
                for character in fragment.chars() {
                    scanned = scanned.saturating_add(character.len_utf8());
                    if scanned >= TOKEN_SCAN_CHUNK_BYTES {
                        control.checkpoint()?;
                        scanned = 0;
                    }
                    *count = count
                        .checked_add(1)
                        .ok_or(ContextError::BudgetExceeded { resource: "token" })?;
                }
            }
            Self::Substring {
                pattern,
                matches,
                prefix,
                prefix_len,
                suffix,
                suffix_len,
                total_len,
            } => {
                for chunk in fragment.as_bytes().chunks(TOKEN_SCAN_CHUNK_BYTES) {
                    control.checkpoint()?;
                    *matches = matches
                        .checked_add(count_substrings(chunk, pattern.as_bytes()) as u64)
                        .ok_or(ContextError::BudgetExceeded { resource: "token" })?;
                }
                let keep = pattern.len().saturating_sub(1);
                *prefix_len = keep.min(fragment.len());
                prefix[..*prefix_len].copy_from_slice(&fragment.as_bytes()[..*prefix_len]);
                *suffix_len = keep.min(fragment.len());
                suffix[..*suffix_len]
                    .copy_from_slice(&fragment.as_bytes()[fragment.len() - *suffix_len..]);
                *total_len = fragment.len() as u64;
            }
            Self::JsonDocument { first, last } => {
                let mut scanned = 0_usize;
                for character in fragment.chars() {
                    scanned = scanned.saturating_add(character.len_utf8());
                    if scanned >= TOKEN_SCAN_CHUNK_BYTES {
                        control.checkpoint()?;
                        scanned = 0;
                    }
                    if first.is_none() {
                        *first = Some(character);
                    }
                    *last = Some(character);
                }
            }
        }
        control.checkpoint()?;
        Ok(summary)
    }

    pub fn concatenate(&self, right: &Self) -> Result<Self, ContextError> {
        match (self, right) {
            (
                Self::Whitespace {
                    tokens: left_tokens,
                    starts_token: left_starts,
                    ends_token: left_ends,
                    empty: left_empty,
                },
                Self::Whitespace {
                    tokens: right_tokens,
                    starts_token: right_starts,
                    ends_token: right_ends,
                    empty: right_empty,
                },
            ) => {
                if *left_empty {
                    return Ok(right.clone());
                }
                if *right_empty {
                    return Ok(self.clone());
                }
                let joined = u64::from(*left_ends && *right_starts);
                Ok(Self::Whitespace {
                    tokens: left_tokens
                        .checked_add(*right_tokens)
                        .and_then(|value| value.checked_sub(joined))
                        .ok_or(ContextError::BudgetExceeded { resource: "token" })?,
                    starts_token: *left_starts,
                    ends_token: *right_ends,
                    empty: false,
                })
            }
            (Self::Characters(left), Self::Characters(right)) => Ok(Self::Characters(
                left.checked_add(*right)
                    .ok_or(ContextError::BudgetExceeded { resource: "token" })?,
            )),
            (
                Self::Substring {
                    pattern,
                    matches: left_matches,
                    prefix: left_prefix,
                    prefix_len: left_prefix_len,
                    suffix: left_suffix,
                    suffix_len: left_suffix_len,
                    total_len: left_len,
                },
                Self::Substring {
                    pattern: right_pattern,
                    matches: right_matches,
                    prefix: right_prefix,
                    prefix_len: right_prefix_len,
                    suffix: right_suffix,
                    suffix_len: right_suffix_len,
                    total_len: right_len,
                },
            ) if pattern == right_pattern => {
                let mut boundary = [0_u8; MAX_TOKEN_PATTERN_BYTES * 2];
                boundary[..*left_suffix_len].copy_from_slice(&left_suffix[..*left_suffix_len]);
                boundary[*left_suffix_len..*left_suffix_len + *right_prefix_len]
                    .copy_from_slice(&right_prefix[..*right_prefix_len]);
                let cross = count_crossing_substrings(
                    &boundary[..*left_suffix_len + *right_prefix_len],
                    *left_suffix_len,
                    pattern.as_bytes(),
                ) as u64;
                let total_len = left_len
                    .checked_add(*right_len)
                    .ok_or(ContextError::BudgetExceeded { resource: "token" })?;
                let keep = pattern.len().saturating_sub(1);
                let mut prefix = [0_u8; MAX_TOKEN_PATTERN_BYTES];
                let mut suffix = [0_u8; MAX_TOKEN_PATTERN_BYTES];
                let prefix_len = keep.min(total_len as usize);
                if *left_len as usize >= prefix_len {
                    prefix[..prefix_len].copy_from_slice(&left_prefix[..prefix_len]);
                } else {
                    let left_count = *left_prefix_len;
                    prefix[..left_count].copy_from_slice(&left_prefix[..left_count]);
                    prefix[left_count..prefix_len]
                        .copy_from_slice(&right_prefix[..prefix_len - left_count]);
                }
                let suffix_len = keep.min(total_len as usize);
                if *right_len as usize >= suffix_len {
                    suffix[..suffix_len].copy_from_slice(&right_suffix[..suffix_len]);
                } else {
                    let left_count = suffix_len - *right_suffix_len;
                    suffix[..left_count].copy_from_slice(
                        &left_suffix[*left_suffix_len - left_count..*left_suffix_len],
                    );
                    suffix[left_count..suffix_len]
                        .copy_from_slice(&right_suffix[..*right_suffix_len]);
                }
                Ok(Self::Substring {
                    pattern,
                    matches: left_matches
                        .checked_add(*right_matches)
                        .and_then(|value| value.checked_add(cross))
                        .ok_or(ContextError::BudgetExceeded { resource: "token" })?,
                    prefix,
                    prefix_len,
                    suffix,
                    suffix_len,
                    total_len,
                })
            }
            (
                Self::JsonDocument {
                    first: left_first,
                    last: left_last,
                },
                Self::JsonDocument {
                    first: right_first,
                    last: right_last,
                },
            ) => Ok(Self::JsonDocument {
                first: left_first.or(*right_first),
                last: right_last.or(*left_last),
            }),
            _ => Err(ContextError::InvalidBundle(
                "token summary policies do not match".to_string(),
            )),
        }
    }

    pub fn tokens(&self) -> u64 {
        match self {
            Self::Whitespace { tokens, .. } | Self::Characters(tokens) => *tokens,
            Self::Substring { matches, .. } => *matches,
            Self::JsonDocument { first, last } => {
                u64::from(!matches!((first, last), (Some('{'), Some('}'))))
            }
        }
    }
}

fn validate_token_pattern(pattern: &str) -> Result<(), ContextError> {
    if pattern.is_empty() || pattern.len() > MAX_TOKEN_PATTERN_BYTES || !pattern.is_ascii() {
        return Err(ContextError::InvalidBundle(
            "token substring pattern must be bounded non-empty ASCII".to_string(),
        ));
    }
    Ok(())
}

fn count_substrings(bytes: &[u8], pattern: &[u8]) -> usize {
    if bytes.len() < pattern.len() {
        return 0;
    }
    bytes
        .windows(pattern.len())
        .filter(|window| *window == pattern)
        .count()
}

fn count_crossing_substrings(bytes: &[u8], boundary: usize, pattern: &[u8]) -> usize {
    if bytes.len() < pattern.len() {
        return 0;
    }
    bytes
        .windows(pattern.len())
        .enumerate()
        .filter(|(start, window)| {
            *start < boundary && start + pattern.len() > boundary && *window == pattern
        })
        .count()
}
