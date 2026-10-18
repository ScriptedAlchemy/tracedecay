//! Go interface satisfaction, decided once every Go method set of a
//! generation is known. Go never names the interfaces a type implements, so
//! a type implements an interface exactly when its method set (the union of
//! its value and pointer receiver methods) covers the interface's.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, HashSet};

use tracedecay_code_extraction::{GoMethodSetRowV1, GoMethodSignatureV1, GoTypeTokenV1};
use tracedecay_domain::{
    CanonicalRelationEdgeV1, EdgeAuthorityV1, NodeKind, RelationEdgeKindV1, SourceSpan,
    SymbolOccurrenceId,
};

use crate::chunks::CodeIndexUnresolvedReferenceV1;

use super::FileGenerationArtifactsV1;
use super::helpers::edge_order;
use super::module_resolution::ModuleImportIndexV1;
use super::resolution_view::ResolutionFileV1;
use super::typescript_resolution::split_parent;

#[derive(Default)]
pub(super) struct GoSatisfactionV1 {
    pub(super) edges: Vec<CanonicalRelationEdgeV1>,
    /// One row per interface whose implementors the seal cannot decide.
    pub(super) gaps: Vec<CodeIndexUnresolvedReferenceV1>,
}

enum TypeKeyV1<'a> {
    Dir(String),
    ImportPath(&'a str),
}

/// A Go type with every name qualified by the package declaring it. Equal
/// strings name identical types.
type QualifiedTypeV1 = String;

#[derive(Clone, PartialEq, Eq, Hash)]
struct MethodKeyV1 {
    name: String,
    /// The declaring package of an unexported method: only that package's
    /// types can satisfy it.
    package: Option<String>,
    params: Vec<QualifiedTypeV1>,
    results: Vec<QualifiedTypeV1>,
}

/// A selector name: a method or field name, with its declaring package when
/// unexported.
type NameV1<'a> = (&'a str, Option<&'a str>);

fn name_of(method: &MethodKeyV1) -> NameV1<'_> {
    (method.name.as_str(), method.package.as_deref())
}

struct GoFileV1<'a> {
    /// The file's package: its dir, or its dir and package name for an
    /// external test package, which shares the dir but not its names.
    scope: &'a str,
    aliases: HashMap<&'a str, TypeKeyV1<'a>>,
}

fn package_scope(file: &FileGenerationArtifactsV1) -> Cow<'_, str> {
    let dir = split_parent(file.authority.logical_path.as_str()).0;
    let package = file
        .artifacts
        .symbols
        .iter()
        .find(|symbol| symbol.kind == NodeKind::GoPackage.as_str())
        .map(|symbol| symbol.simple_name.as_str());
    match package {
        Some(package) if package.ends_with("_test") => Cow::Owned(format!("{dir}\u{1f}{package}")),
        _ => Cow::Borrowed(dir),
    }
}

impl GoFileV1<'_> {
    fn package_key(&self, package: &str) -> String {
        match self.aliases.get(package) {
            Some(TypeKeyV1::Dir(dir)) => format!("d:{dir}"),
            Some(TypeKeyV1::ImportPath(path)) => format!("i:{path}"),
            None => format!("?:{package}"),
        }
    }

    fn qualify(&self, tokens: &[GoTypeTokenV1]) -> QualifiedTypeV1 {
        let mut qualified = String::new();
        for token in tokens {
            match token {
                GoTypeTokenV1::Text(text) => qualified.push_str(text),
                GoTypeTokenV1::Local(name) => {
                    qualified.push_str(&format!("\u{1f}d:{}#{name}\u{1f}", self.scope));
                }
                GoTypeTokenV1::Qualified { package, name } => {
                    let key = self.package_key(package);
                    qualified.push_str(&format!("\u{1f}{key}#{name}\u{1f}"));
                }
            }
        }
        qualified
    }

    fn method_key(&self, method: &GoMethodSignatureV1) -> MethodKeyV1 {
        let exported = method.name.chars().next().is_some_and(char::is_uppercase);
        MethodKeyV1 {
            name: method.name.clone(),
            package: (!exported).then(|| self.scope.to_owned()),
            params: method.params.iter().map(|ty| self.qualify(ty)).collect(),
            results: method.results.iter().map(|ty| self.qualify(ty)).collect(),
        }
    }

    /// The `(dir, name)` of the project interface an embedding names, when it
    /// is a single project type name.
    fn embedded_name(&self, tokens: &[GoTypeTokenV1]) -> Option<(String, String)> {
        match tokens {
            [GoTypeTokenV1::Local(name)] => Some((self.scope.to_owned(), name.clone())),
            [GoTypeTokenV1::Qualified { package, name }] => {
                match self.aliases.get(package.as_str()) {
                    Some(TypeKeyV1::Dir(dir)) => Some((dir.clone(), name.clone())),
                    _ => None,
                }
            }
            _ => None,
        }
    }
}

fn render(tokens: &[GoTypeTokenV1]) -> String {
    tokens
        .iter()
        .map(|token| match token {
            GoTypeTokenV1::Text(text) | GoTypeTokenV1::Local(text) => text.clone(),
            GoTypeTokenV1::Qualified { package, name } => format!("{package}.{name}"),
        })
        .collect()
}

struct InterfaceV1<'a> {
    occurrence: &'a SymbolOccurrenceId,
    span: SourceSpan,
    qualified_name: &'a str,
    methods: Vec<MethodKeyV1>,
    embeds: Vec<EmbedV1>,
    generic: bool,
}

/// An embedding's project type name, when it names one, and its source text.
type EmbedV1 = (Option<(String, String)>, String);

type ExpandedV1 = Result<HashSet<MethodKeyV1>, String>;

type OwnerV1<'a> = (&'a str, &'a str);

/// A named type's methods, its own and those its embeddings and alias
/// target promote to it.
struct EffectiveV1<'a> {
    methods: HashSet<MethodKeyV1>,
    /// Names found where the walk cannot decide which method, if any, they
    /// select: beside an embedding the project cannot see, or declared with
    /// a signature that depends on its receiver's type arguments.
    loose: HashSet<NameV1<'a>>,
    /// Whether an embedding the project cannot see may supply more methods.
    open: bool,
    /// What first kept the walk from deciding, when something did.
    reason: Option<String>,
}

pub(super) fn go_satisfaction<T>(
    files: &[T],
    modules: &ModuleImportIndexV1<'_, T>,
) -> GoSatisfactionV1
where
    T: ResolutionFileV1,
{
    let scopes = files
        .iter()
        .map(|file| {
            let file = file.as_ref();
            (file.extraction.language.as_str() == "go" && !file.artifacts.go_method_sets.is_empty())
                .then(|| package_scope(file))
        })
        .collect::<Vec<_>>();
    let mut named_types = HashMap::<(&str, &str), Vec<(&SymbolOccurrenceId, SourceSpan)>>::new();
    let mut method_sets = HashMap::<OwnerV1<'_>, HashMap<MethodKeyV1, bool>>::new();
    let mut promotes = HashMap::<OwnerV1<'_>, Vec<PromotedV1>>::new();
    let mut fields = HashMap::<OwnerV1<'_>, Vec<NameV1<'_>>>::new();
    let mut interfaces = Vec::<InterfaceV1<'_>>::new();
    let mut interface_rows = HashMap::<&SymbolOccurrenceId, usize>::new();
    let mut interface_names = HashMap::<(&str, &str), Vec<usize>>::new();
    for (file, scope) in files.iter().zip(&scopes) {
        let Some(scope) = scope.as_deref() else {
            continue;
        };
        let file = file.as_ref();
        let aliases = file
            .artifacts
            .imports
            .iter()
            .filter(|row| !row.is_glob && !row.is_public)
            .filter_map(|row| {
                let key = match modules.go_project_package_dir(&row.module_specifier) {
                    Some(dir) => TypeKeyV1::Dir(dir),
                    None => TypeKeyV1::ImportPath(row.module_specifier.as_str()),
                };
                Some((row.local_name.as_deref()?, key))
            })
            .collect();
        let go_file = GoFileV1 { scope, aliases };
        let symbols = file
            .artifacts
            .symbols
            .iter()
            .map(|symbol| (&symbol.occurrence, symbol))
            .collect::<HashMap<_, _>>();
        for bound in &file.artifacts.go_method_sets {
            let Some(symbol) = symbols.get(&bound.occurrence) else {
                continue;
            };
            let owner = (scope, symbol.simple_name.as_str());
            let interface = match &bound.row {
                GoMethodSetRowV1::NamedType => {
                    named_types
                        .entry(owner)
                        .or_default()
                        .push((&bound.occurrence, bound.span));
                    continue;
                }
                GoMethodSetRowV1::Field { name } => {
                    let exported = name.chars().next().is_some_and(char::is_uppercase);
                    fields
                        .entry(owner)
                        .or_default()
                        .push((name.as_str(), (!exported).then_some(scope)));
                    continue;
                }
                GoMethodSetRowV1::Receiver {
                    type_name,
                    method,
                    generic,
                } => {
                    method_sets
                        .entry((scope, type_name.as_str()))
                        .or_default()
                        .insert(go_file.method_key(method), *generic);
                    continue;
                }
                GoMethodSetRowV1::Promotes { embedded } => {
                    let promoted = match embedded.as_slice() {
                        // Of the predeclared types only `error` has a method.
                        [GoTypeTokenV1::Text(name)] if name == "error" => PromotedV1::Error,
                        [GoTypeTokenV1::Text(_)] => continue,
                        _ => PromotedV1::Embed(go_file.embedded_name(embedded), render(embedded)),
                    };
                    promotes.entry(owner).or_default().push(promoted);
                    continue;
                }
                _ => {
                    let index = *interface_rows.entry(&bound.occurrence).or_insert_with(|| {
                        interface_names
                            .entry(owner)
                            .or_default()
                            .push(interfaces.len());
                        interfaces.push(InterfaceV1 {
                            occurrence: &bound.occurrence,
                            span: bound.span,
                            qualified_name: symbol.qualified_name.as_str(),
                            methods: Vec::new(),
                            embeds: Vec::new(),
                            generic: false,
                        });
                        interfaces.len() - 1
                    });
                    &mut interfaces[index]
                }
            };
            match &bound.row {
                GoMethodSetRowV1::InterfaceMethod { method } => {
                    interface.methods.push(go_file.method_key(method));
                }
                GoMethodSetRowV1::Embeds { embedded } => interface
                    .embeds
                    .push((go_file.embedded_name(embedded), render(embedded))),
                GoMethodSetRowV1::GenericInterface => interface.generic = true,
                // `expand` reports an interface without methods as a gap.
                GoMethodSetRowV1::EmptyInterface
                | GoMethodSetRowV1::NamedType
                | GoMethodSetRowV1::Field { .. }
                | GoMethodSetRowV1::Receiver { .. }
                | GoMethodSetRowV1::Promotes { .. } => {}
            }
        }
    }

    let mut expanded = vec![None; interfaces.len()];
    let expansions = (0..interfaces.len())
        .map(|index| {
            expand(
                index,
                &interfaces,
                &interface_names,
                &mut expanded,
                &mut HashSet::new(),
            )
        })
        .collect::<Vec<_>>();
    let mut owners = method_sets
        .keys()
        .chain(promotes.keys())
        .copied()
        .collect::<Vec<_>>();
    owners.sort_unstable();
    owners.dedup();
    let error = MethodKeyV1 {
        name: "Error".to_owned(),
        package: None,
        params: Vec::new(),
        results: vec!["string".to_owned()],
    };
    let promotion = PromotionV1 {
        named_types: &named_types,
        interface_names: &interface_names,
        expansions: &expansions,
        promotes: &promotes,
        own: &method_sets,
        fields: &fields,
        error: &error,
    };
    let effective = owners
        .iter()
        .map(|owner| (*owner, promotion.effective(*owner)))
        .collect::<HashMap<_, _>>();
    let mut by_method = HashMap::<&MethodKeyV1, Vec<OwnerV1<'_>>>::new();
    for owner in &owners {
        for method in &effective[owner].methods {
            by_method.entry(method).or_default().push(*owner);
        }
    }
    let holders = |method| by_method.get(method).map_or(&[][..], Vec::as_slice);
    // The owners that may hold a method the walk cannot decide, indexed by
    // the names they may hold.
    let mut by_name = HashMap::<NameV1<'_>, Vec<OwnerV1<'_>>>::new();
    let mut open_owners = Vec::new();
    for owner in &owners {
        let owner_effective = &effective[owner];
        if owner_effective.reason.is_none() {
            continue;
        }
        if owner_effective.open {
            open_owners.push(*owner);
        }
        for name in owner_effective
            .methods
            .iter()
            .map(name_of)
            .chain(owner_effective.loose.iter().copied())
        {
            by_name.entry(name).or_default().push(*owner);
        }
    }
    let mut edges = Vec::new();
    let mut gaps = Vec::new();
    for (interface, expansion) in interfaces.iter().zip(&expansions) {
        let gap = |reference_name: &String| CodeIndexUnresolvedReferenceV1 {
            from_occurrence: interface.occurrence.clone(),
            reference_name: reference_name.clone(),
            kind: RelationEdgeKindV1::Implements,
            evidence_span: interface.span,
            unmodeled_import: None,
            argument_count: None,
            ambiguous_local: false,
        };
        let methods = match expansion {
            Ok(methods) => methods,
            Err(reference_name) => {
                gaps.push(gap(reference_name));
                continue;
            }
        };
        let Some(candidates) = methods.iter().map(holders).min_by_key(|c| c.len()) else {
            continue;
        };
        for owner in candidates {
            if !methods.is_subset(&effective[owner].methods) {
                continue;
            }
            for (occurrence, span) in named_types.get(owner).into_iter().flatten() {
                edges.push(CanonicalRelationEdgeV1 {
                    from_occurrence: (*occurrence).clone(),
                    to_occurrence: interface.occurrence.clone(),
                    kind: RelationEdgeKindV1::Implements,
                    authority: EdgeAuthorityV1::NameResolved,
                    evidence_span: *span,
                });
            }
        }
        // An open owner may supply a missing method through a type the
        // project cannot see, unless that method needs this project's types
        // or package.
        let suppliable = |method: &MethodKeyV1| {
            method.package.is_none()
                && !method
                    .params
                    .iter()
                    .chain(&method.results)
                    .any(|ty| ty.contains("\u{1f}d:"))
        };
        let could = |owner: &EffectiveV1<'_>, method: &MethodKeyV1| {
            owner.methods.contains(method)
                || owner.loose.contains(&name_of(method))
                || (owner.open && suppliable(method))
        };
        let suspects = methods
            .iter()
            .map(|method| {
                let named = by_name.get(&name_of(method)).map_or(&[][..], Vec::as_slice);
                let open = if suppliable(method) {
                    &open_owners[..]
                } else {
                    &[]
                };
                (named.len() + open.len(), named.iter().chain(open))
            })
            .min_by_key(|(len, _)| *len);
        let Some((_, suspects)) = suspects else {
            continue;
        };
        let undecided = suspects
            .filter(|owner| {
                let owner = &effective[*owner];
                !methods.is_subset(&owner.methods)
                    && methods.iter().all(|method| could(owner, method))
            })
            .min();
        if let Some(reference_name) = undecided.and_then(|owner| effective[owner].reason.as_ref()) {
            gaps.push(gap(reference_name));
        }
    }
    edges.sort_by(edge_order);
    edges.dedup();
    gaps.sort();
    gaps.dedup();
    GoSatisfactionV1 { edges, gaps }
}

/// What an embedded field or alias target promotes to its owner.
enum PromotedV1 {
    /// The predeclared `error` interface.
    Error,
    Embed(Option<(String, String)>, String),
}

/// A type an embedding walk reaches.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum ReachedV1<'a> {
    Named(OwnerV1<'a>),
    Interface(usize),
    Error,
}

struct PromotionV1<'a> {
    named_types: &'a HashMap<OwnerV1<'a>, Vec<(&'a SymbolOccurrenceId, SourceSpan)>>,
    interface_names: &'a HashMap<OwnerV1<'a>, Vec<usize>>,
    expansions: &'a [ExpandedV1],
    promotes: &'a HashMap<OwnerV1<'a>, Vec<PromotedV1>>,
    own: &'a HashMap<OwnerV1<'a>, HashMap<MethodKeyV1, bool>>,
    fields: &'a HashMap<OwnerV1<'a>, Vec<NameV1<'a>>>,
    error: &'a MethodKeyV1,
}

impl<'a> PromotionV1<'a> {
    /// `owner`'s methods, walked one embedding depth at a time as Go
    /// selectors resolve: a name found at a shallower depth hides deeper
    /// ones, and a name reached by more than one path at its shallowest depth
    /// is ambiguous and belongs to no method set. Field names take part, so
    /// a field hides or ties with a promoted method of its name.
    fn effective(&self, owner: OwnerV1<'a>) -> EffectiveV1<'a> {
        let mut decided = HashMap::<NameV1<'a>, Option<&'a MethodKeyV1>>::new();
        let mut loose = HashSet::new();
        // The shallowest depth an embedding the project cannot see promotes
        // names to. It may hide or tie with any name found there or deeper.
        let mut open_depth = usize::MAX;
        let mut reason = None;
        let mut walked = HashSet::from([owner]);
        // Each reached type with the number of embedding paths reaching it.
        let mut level = BTreeMap::from([(ReachedV1::Named(owner), 1_usize)]);
        let mut depth = 0_usize;
        while !level.is_empty() {
            // Each name with its method (none for a field), the paths
            // reaching it, and whether its signature depends on its
            // receiver's type arguments.
            let mut found = HashMap::<NameV1<'a>, (Option<&'a MethodKeyV1>, usize, bool)>::new();
            let mut next = BTreeMap::new();
            for (reached, paths) in level {
                let names: Box<dyn Iterator<Item = (NameV1<'a>, Option<&'a MethodKeyV1>, bool)>> =
                    match reached {
                        ReachedV1::Named(named) => {
                            for promoted in self.promotes.get(&named).into_iter().flatten() {
                                match self.reach(promoted) {
                                    // Every name of a type reached at a
                                    // shallower depth is already decided.
                                    Ok(ReachedV1::Named(inner)) if walked.contains(&inner) => {}
                                    Ok(inner) => {
                                        let count = next.entry(inner).or_default();
                                        *count = paths.saturating_add(*count);
                                    }
                                    Err(text) => {
                                        open_depth = open_depth.min(depth + 1);
                                        reason.get_or_insert_with(|| text.to_owned());
                                    }
                                }
                            }
                            let own = self.own.get(&named).into_iter().flatten();
                            if own.clone().any(|(_, generic)| *generic) {
                                reason.get_or_insert_with(|| named.1.to_owned());
                            }
                            let fields = self.fields.get(&named).into_iter().flatten();
                            Box::new(fields.map(|name| (*name, None, false)).chain(own.map(
                                |(method, generic)| (name_of(method), Some(method), *generic),
                            )))
                        }
                        ReachedV1::Interface(index) => Box::new(
                            self.expansions[index]
                                .iter()
                                .flatten()
                                .map(|method| (name_of(method), Some(method), false)),
                        ),
                        ReachedV1::Error => Box::new(std::iter::once((
                            name_of(self.error),
                            Some(self.error),
                            false,
                        ))),
                    };
                for (name, method, generic) in names {
                    if !decided.contains_key(&name) {
                        let entry = found.entry(name).or_insert((method, 0, false));
                        entry.1 = paths.saturating_add(entry.1);
                        entry.2 |= generic;
                    }
                }
            }
            for (name, (method, count, generic)) in found {
                let selected = if depth >= open_depth || (generic && count == 1) {
                    loose.insert(name);
                    None
                } else {
                    method.filter(|_| count == 1)
                };
                decided.insert(name, selected);
            }
            walked.extend(next.keys().filter_map(|reached| match reached {
                ReachedV1::Named(named) => Some(*named),
                _ => None,
            }));
            level = next;
            depth += 1;
        }
        EffectiveV1 {
            methods: decided.into_values().flatten().cloned().collect(),
            loose,
            open: open_depth != usize::MAX,
            reason,
        }
    }

    /// The type `promoted` names, or its source text when the project cannot
    /// see that type's methods.
    fn reach(&self, promoted: &'a PromotedV1) -> Result<ReachedV1<'a>, &'a str> {
        let (name, text) = match promoted {
            PromotedV1::Error => return Ok(ReachedV1::Error),
            PromotedV1::Embed(name, text) => (name, text.as_str()),
        };
        let key = name
            .as_ref()
            .map(|(dir, name)| (dir.as_str(), name.as_str()));
        let interface = key.and_then(|key| self.interface_names.get(&key));
        let named = key.and_then(|key| self.named_types.get_key_value(&key));
        match (
            interface.map(Vec::as_slice),
            named.map(|(target, sites)| (target, sites.as_slice())),
        ) {
            (Some([index]), None) if self.expansions[*index].is_ok() => {
                Ok(ReachedV1::Interface(*index))
            }
            (None, Some((target, [_]))) => Ok(ReachedV1::Named(*target)),
            _ => Err(text),
        }
    }
}

/// The method set of interface `index` with its embeddings expanded, or the
/// name of what keeps it undecidable.
fn expand(
    index: usize,
    interfaces: &[InterfaceV1<'_>],
    interface_names: &HashMap<(&str, &str), Vec<usize>>,
    expanded: &mut [Option<ExpandedV1>],
    visiting: &mut HashSet<usize>,
) -> ExpandedV1 {
    if let Some(done) = &expanded[index] {
        return done.clone();
    }
    if !visiting.insert(index) {
        return Ok(HashSet::new());
    }
    let interface = &interfaces[index];
    let result = (|| {
        if interface.generic {
            return Err(interface.qualified_name.to_owned());
        }
        let mut methods = interface.methods.iter().cloned().collect::<HashSet<_>>();
        for (name, text) in &interface.embeds {
            let target = name.as_ref().and_then(|(dir, name)| {
                match interface_names
                    .get(&(dir.as_str(), name.as_str()))?
                    .as_slice()
                {
                    [target] => Some(*target),
                    _ => None,
                }
            });
            let Some(target) = target else {
                return Err(text.clone());
            };
            methods.extend(expand(
                target,
                interfaces,
                interface_names,
                expanded,
                visiting,
            )?);
        }
        if methods.is_empty() {
            return Err(interface.qualified_name.to_owned());
        }
        Ok(methods)
    })();
    visiting.remove(&index);
    expanded[index] = Some(result.clone());
    result
}
