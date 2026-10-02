//! Go interface satisfaction, decided once every Go method set of a
//! generation is known. Go never names the interfaces a type implements, so
//! a type implements an interface exactly when its method set (the union of
//! its value and pointer receiver methods) covers the interface's.

use std::collections::{BTreeMap, HashMap, HashSet};

use tracedecay_code_extraction::{GoMethodSetRowV1, GoMethodSignatureV1, GoTypeTokenV1};
use tracedecay_domain::{
    CanonicalRelationEdgeV1, EdgeAuthorityV1, RelationEdgeKindV1, SourceSpan, SymbolOccurrenceId,
};

use crate::chunks::CodeIndexUnresolvedReferenceV1;

use super::FileGenerationArtifactsV1;
use super::helpers::edge_order;
use super::module_resolution::ModuleImportIndexV1;
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

struct GoFileV1<'a> {
    dir: &'a str,
    aliases: HashMap<&'a str, TypeKeyV1<'a>>,
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
                    qualified.push_str(&format!("\u{1f}d:{}#{name}\u{1f}", self.dir));
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
            package: (!exported).then(|| self.dir.to_owned()),
            params: method.params.iter().map(|ty| self.qualify(ty)).collect(),
            results: method.results.iter().map(|ty| self.qualify(ty)).collect(),
        }
    }

    /// The `(dir, name)` of the project interface an embedding names, when it
    /// is a single project type name.
    fn embedded_name(&self, tokens: &[GoTypeTokenV1]) -> Option<(String, String)> {
        match tokens {
            [GoTypeTokenV1::Local(name)] => Some((self.dir.to_owned(), name.clone())),
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
struct EffectiveV1 {
    methods: HashSet<MethodKeyV1>,
    /// The first promoting type whose methods the project cannot see: the
    /// type may carry more methods than `methods`.
    open: Option<String>,
}

pub(super) fn go_satisfaction<T>(files: &[T], modules: &ModuleImportIndexV1<'_>) -> GoSatisfactionV1
where
    T: AsRef<FileGenerationArtifactsV1>,
{
    let mut named_types = HashMap::<(&str, &str), Vec<(&SymbolOccurrenceId, SourceSpan)>>::new();
    let mut method_sets = HashMap::<OwnerV1<'_>, HashSet<MethodKeyV1>>::new();
    let mut promotes = HashMap::<OwnerV1<'_>, Vec<PromotedV1>>::new();
    let mut interfaces = Vec::<InterfaceV1<'_>>::new();
    let mut interface_rows = HashMap::<&SymbolOccurrenceId, usize>::new();
    let mut interface_names = HashMap::<(&str, &str), Vec<usize>>::new();
    for file in files {
        let file = file.as_ref();
        if file.extraction.language.as_str() != "go" || file.artifacts.go_method_sets.is_empty() {
            continue;
        }
        let dir = split_parent(file.authority.logical_path.as_str()).0;
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
        let go_file = GoFileV1 { dir, aliases };
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
            let interface = match &bound.row {
                GoMethodSetRowV1::NamedType => {
                    named_types
                        .entry((dir, symbol.simple_name.as_str()))
                        .or_default()
                        .push((&bound.occurrence, bound.span));
                    continue;
                }
                GoMethodSetRowV1::Receiver { type_name, method } => {
                    method_sets
                        .entry((dir, type_name.as_str()))
                        .or_default()
                        .insert(go_file.method_key(method));
                    continue;
                }
                GoMethodSetRowV1::Promotes { embedded } => {
                    let promoted = match embedded.as_slice() {
                        // Of the predeclared types only `error` has a method.
                        [GoTypeTokenV1::Text(name)] if name == "error" => PromotedV1::Error,
                        [GoTypeTokenV1::Text(_)] => continue,
                        _ => PromotedV1::Embed(go_file.embedded_name(embedded), render(embedded)),
                    };
                    promotes
                        .entry((dir, symbol.simple_name.as_str()))
                        .or_default()
                        .push(promoted);
                    continue;
                }
                _ => {
                    let index = *interface_rows.entry(&bound.occurrence).or_insert_with(|| {
                        interface_names
                            .entry((dir, symbol.simple_name.as_str()))
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
                GoMethodSetRowV1::NamedType
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
    let promotion = PromotionV1 {
        named_types: &named_types,
        interface_names: &interface_names,
        expansions: &expansions,
        promotes: &promotes,
        own: &method_sets,
        error: MethodKeyV1 {
            name: "Error".to_owned(),
            package: None,
            params: Vec::new(),
            results: vec!["string".to_owned()],
        },
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
    let open_owners = owners
        .iter()
        .copied()
        .filter(|owner| effective[owner].open.is_some())
        .collect::<Vec<_>>();
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
        let suspects = methods
            .iter()
            .filter(|method| !suppliable(method))
            .map(holders)
            .min_by_key(|holders| holders.len())
            .unwrap_or(&open_owners[..]);
        let undecided = suspects
            .iter()
            .filter(|owner| {
                let owner = &effective[*owner];
                owner.open.is_some()
                    && !methods.is_subset(&owner.methods)
                    && methods
                        .iter()
                        .all(|method| owner.methods.contains(method) || suppliable(method))
            })
            .min();
        if let Some(reference_name) = undecided.and_then(|owner| effective[owner].open.as_ref()) {
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
    own: &'a HashMap<OwnerV1<'a>, HashSet<MethodKeyV1>>,
    error: MethodKeyV1,
}

impl<'a> PromotionV1<'a> {
    /// `owner`'s methods, walked one embedding depth at a time as Go
    /// selectors resolve: a name found at a shallower depth hides deeper
    /// ones, and a name reached by more than one path at its shallowest depth
    /// is ambiguous and belongs to no method set.
    fn effective(&self, owner: OwnerV1<'a>) -> EffectiveV1 {
        let mut decided = HashMap::<(&str, Option<&str>), Option<&MethodKeyV1>>::new();
        let mut open = None;
        let mut walked = HashSet::from([owner]);
        // Each reached type with the number of embedding paths reaching it.
        let mut level = BTreeMap::from([(ReachedV1::Named(owner), 1_usize)]);
        while !level.is_empty() {
            let mut found = HashMap::<(&str, Option<&str>), (&MethodKeyV1, usize)>::new();
            let mut next = BTreeMap::new();
            for (reached, paths) in level {
                let methods: Box<dyn Iterator<Item = &MethodKeyV1>> = match reached {
                    ReachedV1::Named(named) => {
                        for promoted in self.promotes.get(&named).into_iter().flatten() {
                            match self.reach(promoted) {
                                // Every name of a type reached at a shallower
                                // depth is already decided.
                                Ok(ReachedV1::Named(inner)) if walked.contains(&inner) => {}
                                Ok(inner) => {
                                    let count = next.entry(inner).or_default();
                                    *count = paths.saturating_add(*count);
                                }
                                Err(text) => {
                                    open.get_or_insert_with(|| text.to_owned());
                                }
                            }
                        }
                        Box::new(self.own.get(&named).into_iter().flatten())
                    }
                    ReachedV1::Interface(index) => {
                        Box::new(self.expansions[index].iter().flatten())
                    }
                    ReachedV1::Error => Box::new(std::iter::once(&self.error)),
                };
                for method in methods {
                    let name = (method.name.as_str(), method.package.as_deref());
                    if !decided.contains_key(&name) {
                        let (_, count) = found.entry(name).or_insert((method, 0));
                        *count = paths.saturating_add(*count);
                    }
                }
            }
            decided.extend(
                found
                    .into_iter()
                    .map(|(name, (method, count))| (name, (count == 1).then_some(method))),
            );
            walked.extend(next.keys().filter_map(|reached| match reached {
                ReachedV1::Named(named) => Some(*named),
                _ => None,
            }));
            level = next;
        }
        EffectiveV1 {
            methods: decided.into_values().flatten().cloned().collect(),
            open,
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
