//! Instance calls backed by parser-attested receiver bindings. All name
//! lookups use the sealed TypeScript module authority, including the home
//! module of a factory's declared return type.

use std::sync::Arc;

use tracedecay_code_extraction::{TypeScriptReceiverV1, TypeScriptTypeRowV1};
use tracedecay_domain::RelationEdgeKindV1;

use super::CodeIndexProductionErrorV1;
use super::resolution_view::{ResolutionFileFactsV1, ResolutionFileV1, SymbolsByNameV1};
use super::typescript_resolution::{
    ImportBindingOutcomeV1, TypeScriptModuleIndexV1, unique_local_import,
};
use crate::chunks::CodeIndexUnresolvedReferenceV1;

pub(super) fn receiver_at<'a>(
    file: &'a dyn ResolutionFileFactsV1,
    reference: &CodeIndexUnresolvedReferenceV1,
) -> Option<&'a TypeScriptReceiverV1> {
    if reference.kind != RelationEdgeKindV1::Calls {
        return None;
    }
    let mut matches = file.typescript_types().iter().filter_map(|row| {
        if row.occurrence != reference.from_occurrence {
            return None;
        }
        match &row.row {
            TypeScriptTypeRowV1::ReceiverCall { span, receiver }
                if *span == reference.evidence_span =>
            {
                Some(receiver)
            }
            _ => None,
        }
    });
    let receiver = matches.next()?;
    matches.next().is_none().then_some(receiver)
}

pub(super) fn receiver_name(receiver: &TypeScriptReceiverV1) -> &str {
    match receiver {
        TypeScriptReceiverV1::Named { name, .. }
        | TypeScriptReceiverV1::AwaitedFactory { name } => name,
    }
}

/// Additional lookup inputs beyond the spelling of `value.member`.
pub(super) fn referenced_type_names(
    file: &dyn ResolutionFileFactsV1,
) -> impl Iterator<Item = &str> {
    file.typescript_types()
        .iter()
        .filter_map(|row| match &row.row {
            TypeScriptTypeRowV1::ReceiverCall { receiver, .. } => Some(receiver_name(receiver)),
            TypeScriptTypeRowV1::CallableReturn { name, .. } => Some(name.as_str()),
            TypeScriptTypeRowV1::InstanceMethod => None,
        })
}

fn module_symbol<T: ResolutionFileV1>(
    files: &[T],
    by_name: &dyn SymbolsByNameV1,
    modules: &TypeScriptModuleIndexV1,
    index: usize,
    name: &str,
    kind: RelationEdgeKindV1,
) -> Result<ImportBindingOutcomeV1, CodeIndexProductionErrorV1> {
    let file = files[index].binding()?;
    let (head, members) = name
        .split_once('.')
        .map_or((name, None), |(head, rest)| (head, Some(rest)));
    if let Some(binding) = unique_local_import(file, head, kind) {
        return match members {
            Some(members) => modules.resolve_member_call(files, by_name, binding, members, kind),
            None => modules.resolve_import_binding(files, by_name, binding, kind),
        };
    }
    // Several imports, or a type-only import used as a constructor, cannot
    // fall through to a same-named local declaration.
    if members.is_some()
        || file
            .imports()
            .iter()
            .any(|binding| !binding.is_public && binding.local_name.as_deref() == Some(head))
    {
        return Ok(ImportBindingOutcomeV1::Unresolved);
    }
    let qualified = format!("{}::{name}", file.logical_path());
    let candidates = by_name.get(name)?.unwrap_or_default();
    let mut matches = candidates.iter().filter(|(candidate_index, symbol)| {
        *candidate_index == index
            && symbol.qualified_name == qualified
            && crate::chunks::relation_target_kind_is_compatible(kind, &symbol.kind)
    });
    let first = matches.next();
    Ok(match (first, matches.next()) {
        (Some((index, symbol)), None) => ImportBindingOutcomeV1::Bound(*index, Arc::clone(symbol)),
        _ => ImportBindingOutcomeV1::Unresolved,
    })
}

/// `None` means the parser has no proof for this call. A present unresolved
/// outcome must remain a gap; it never permits a same-name method fallback.
pub(super) fn resolve_receiver_call<T: ResolutionFileV1>(
    files: &[T],
    by_name: &dyn SymbolsByNameV1,
    modules: &TypeScriptModuleIndexV1,
    index: usize,
    reference: &CodeIndexUnresolvedReferenceV1,
) -> Result<Option<ImportBindingOutcomeV1>, CodeIndexProductionErrorV1> {
    let Some(receiver) = receiver_at(files[index].binding()?, reference) else {
        return Ok(None);
    };
    let Some((_, member)) = reference.reference_name.rsplit_once('.') else {
        return Ok(Some(ImportBindingOutcomeV1::Unresolved));
    };
    let owner = match receiver {
        TypeScriptReceiverV1::Named { name, constructor } => module_symbol(
            files,
            by_name,
            modules,
            index,
            name,
            if *constructor {
                RelationEdgeKindV1::Calls
            } else {
                RelationEdgeKindV1::TypeOf
            },
        )?,
        TypeScriptReceiverV1::AwaitedFactory { name } => {
            let factory = module_symbol(
                files,
                by_name,
                modules,
                index,
                name,
                RelationEdgeKindV1::Calls,
            )?;
            let ImportBindingOutcomeV1::Bound(home, factory) = factory else {
                return Ok(Some(factory));
            };
            if !matches!(factory.kind.as_str(), "function" | "arrow_function") {
                return Ok(Some(ImportBindingOutcomeV1::Unresolved));
            }
            let facts = files[home].binding()?;
            let mut returns = facts.typescript_types().iter().filter_map(|row| {
                if row.occurrence != factory.occurrence {
                    return None;
                }
                match &row.row {
                    TypeScriptTypeRowV1::CallableReturn { name, promise } => Some((name, *promise)),
                    _ => None,
                }
            });
            let Some((name, promise)) = returns.next() else {
                return Ok(Some(ImportBindingOutcomeV1::Unresolved));
            };
            if returns.next().is_some()
                || (factory.is_async && !promise)
                || (promise
                    && facts.imports().iter().any(|binding| {
                        !binding.is_public && binding.local_name.as_deref() == Some("Promise")
                    }))
            {
                return Ok(Some(ImportBindingOutcomeV1::Unresolved));
            }
            module_symbol(
                files,
                by_name,
                modules,
                home,
                name,
                RelationEdgeKindV1::TypeOf,
            )?
        }
    };
    let ImportBindingOutcomeV1::Bound(owner_index, owner) = owner else {
        return Ok(Some(owner));
    };
    if !matches!(owner.kind.as_str(), "class" | "interface") {
        return Ok(Some(ImportBindingOutcomeV1::Unresolved));
    }
    let qualified = format!("{}::{member}", owner.qualified_name);
    let owner_facts = files[owner_index].binding()?;
    let candidates = by_name.get(member)?.unwrap_or_default();
    let mut methods = candidates.iter().filter(|(candidate_index, symbol)| {
        *candidate_index == owner_index
            && symbol.qualified_name == qualified
            && owner_facts.typescript_types().iter().any(|row| {
                row.occurrence == symbol.occurrence
                    && row.row == TypeScriptTypeRowV1::InstanceMethod
            })
    });
    let first = methods.next();
    Ok(Some(match (first, methods.next()) {
        (Some((index, symbol)), None) => ImportBindingOutcomeV1::Bound(*index, Arc::clone(symbol)),
        _ => ImportBindingOutcomeV1::Unresolved,
    }))
}
