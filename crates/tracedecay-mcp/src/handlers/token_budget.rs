//! Fixed-share token budgets for the ranked sections of one answer.
//!
//! A token is `json_bytes.div_ceil(4)`. Section weights are shares of the
//! caller's budget. The last section receives the remainder so the shares
//! add up to the budget. A row that is larger than its section quota is kept
//! alone and the cut says `over_ceiling`.

use serde::Serialize;
use tracedecay_contracts::retrieval::{TokenBudgetCutV1, TokenBudgetSectionV1};
use tracedecay_domain::errors::{Result, TraceDecayError};

pub(crate) struct SectionCut {
    pub section: &'static str,
    pub total: u32,
    pub shown: u32,
    pub est_tokens: u32,
    pub over_ceiling: bool,
}

pub(crate) fn quotas(budget: u32, weights: &[u32]) -> Vec<u32> {
    let sum = weights.iter().copied().sum::<u32>().max(1);
    let mut shares = weights
        .iter()
        .map(|weight| (u64::from(budget) * u64::from(*weight) / u64::from(sum)) as u32)
        .collect::<Vec<_>>();
    let used = shares.iter().copied().sum::<u32>();
    if let Some(last) = shares.last_mut() {
        *last = last.saturating_add(budget.saturating_sub(used));
    }
    shares
}

pub(crate) fn trim_section<T: Serialize>(
    section: &'static str,
    rows: &mut Vec<T>,
    quota: u32,
) -> Result<SectionCut> {
    let total = u32::try_from(rows.len()).map_err(|error| TraceDecayError::Config {
        message: format!("token-budget section row count exceeds supported range: {error}"),
    })?;
    let mut used = 0_u32;
    let mut shown = 0_u32;
    let mut over_ceiling = false;
    for row in rows.iter() {
        let cost = estimated_tokens(row)?.max(1);
        if u64::from(used) + u64::from(cost) > u64::from(quota) {
            if shown == 0 {
                shown = 1;
                used = cost;
                over_ceiling = true;
            }
            break;
        }
        used += cost;
        shown = shown.saturating_add(1);
    }
    rows.truncate(shown as usize);
    Ok(SectionCut {
        section,
        total,
        shown,
        est_tokens: used,
        over_ceiling,
    })
}

pub(crate) fn cut(budget_tokens: u32, sections: Vec<SectionCut>) -> Result<TokenBudgetCutV1> {
    let est_tokens = sections.iter().try_fold(0_u32, |total, section| {
        total
            .checked_add(section.est_tokens)
            .ok_or_else(|| TraceDecayError::Config {
                message: "token-budget estimate exceeds supported range".to_owned(),
            })
    })?;
    Ok(TokenBudgetCutV1 {
        budget_tokens,
        est_tokens,
        over_ceiling: sections.iter().any(|section| section.over_ceiling),
        sections: sections
            .into_iter()
            .map(|section| TokenBudgetSectionV1 {
                section: section.section.to_owned(),
                total: section.total,
                shown: section.shown,
            })
            .collect(),
    })
}

pub(crate) fn estimated_tokens(value: &impl Serialize) -> Result<u32> {
    let bytes = serde_json::to_vec(value)?;
    u32::try_from(bytes.len().div_ceil(4)).map_err(|error| TraceDecayError::Config {
        message: format!("token-budget row estimate exceeds supported range: {error}"),
    })
}

#[cfg(test)]
mod tests {
    use super::{SectionCut, cut, quotas, trim_section};
    use serde::{Serialize, Serializer};
    use tracedecay_domain::errors::TraceDecayError;

    #[test]
    fn a_two_token_quota_keeps_the_first_row_and_names_the_cut() {
        let mut rows = vec!["aaaa".to_owned(), "bbbb".to_owned()];
        let section = trim_section("rows", &mut rows, 2).unwrap();
        let budget = cut(2, vec![section]).unwrap();
        assert_eq!(rows, vec!["aaaa".to_owned()]);
        assert_eq!(budget.budget_tokens, 2);
        assert_eq!(budget.est_tokens, 2);
        assert!(!budget.over_ceiling);
        assert_eq!(budget.sections[0].section, "rows");
        assert_eq!(budget.sections[0].total, 2);
        assert_eq!(budget.sections[0].shown, 1);
    }

    #[test]
    fn a_row_larger_than_its_quota_is_kept_and_marked_over_ceiling() {
        let mut rows = vec!["aaaaaaaaaaaaaaaaaaaa".to_owned()];
        let section = trim_section("rows", &mut rows, 1).unwrap();
        let budget = cut(1, vec![section]).unwrap();
        assert_eq!(rows, vec!["aaaaaaaaaaaaaaaaaaaa".to_owned()]);
        assert!(budget.over_ceiling);
        assert_eq!(budget.sections[0].shown, 1);
        assert_eq!(budget.sections[0].total, 1);
    }

    #[test]
    fn section_quotas_use_the_whole_budget() {
        assert_eq!(quotas(10, &[40, 25, 25, 10]), vec![4, 2, 2, 2]);
        assert_eq!(
            quotas(u32::MAX, &[40, 25, 25, 10]),
            vec![1_717_986_918, 1_073_741_823, 1_073_741_823, 429_496_731]
        );
    }

    #[test]
    fn a_serialization_failure_refuses_the_section_without_truncating_it() {
        struct Unserializable;
        impl Serialize for Unserializable {
            fn serialize<S: Serializer>(&self, _serializer: S) -> Result<S::Ok, S::Error> {
                Err(serde::ser::Error::custom("row cannot be serialized"))
            }
        }
        let mut rows = vec![Unserializable, Unserializable];
        let error = match trim_section("rows", &mut rows, 1) {
            Ok(_) => panic!("serialization failure must refuse the token estimate"),
            Err(error) => error,
        };
        assert!(matches!(error, TraceDecayError::Json(_)));
        assert!(error.to_string().contains("row cannot be serialized"));
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn an_unrepresentable_total_refuses_instead_of_wrapping() {
        let sections = [u32::MAX, 1]
            .into_iter()
            .map(|est_tokens| SectionCut {
                section: "rows",
                total: 1,
                shown: 1,
                est_tokens,
                over_ceiling: true,
            })
            .collect();
        let error = cut(1, sections).unwrap_err();
        assert!(matches!(error, TraceDecayError::Config { .. }));
        assert_eq!(
            error.to_string(),
            "config error: token-budget estimate exceeds supported range"
        );
    }
}
