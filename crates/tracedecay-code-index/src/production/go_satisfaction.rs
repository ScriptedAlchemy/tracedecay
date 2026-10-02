//! Go interface satisfaction, decided once every Go method set of a
//! generation is known. Go never names the interfaces a type implements, so
//! a type implements an interface exactly when its method set (the union of
//! its value and pointer receiver methods) covers the interface's.

use std::collections::{HashMap, HashSet};

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
    /// Each embedding's project interface name, when it names one, and its
    /// source text.
    embeds: Vec<(Option<(String, String)>, String)>,
    generic: bool,
}

type ExpandedV1 = Result<HashSet<MethodKeyV1>, String>;

pub(super) fn go_satisfaction<T>(files: &[T], modules: &ModuleImportIndexV1<'_>) -> GoSatisfactionV1
where
    T: AsRef<FileGenerationArtifactsV1>,
{
    let mut named_types = HashMap::<(&str, &str), Vec<(&SymbolOccurrenceId, SourceSpan)>>::new();
    let mut method_sets = HashMap::<(&str, &str), HashSet<MethodKeyV1>>::new();
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
                GoMethodSetRowV1::NamedType | GoMethodSetRowV1::Receiver { .. } => {}
            }
        }
    }

    let mut by_method = HashMap::<&MethodKeyV1, Vec<(&str, &str)>>::new();
    for (owner, methods) in &method_sets {
        for method in methods {
            by_method.entry(method).or_default().push(*owner);
        }
    }
    let mut expanded = vec![None; interfaces.len()];
    let mut edges = Vec::new();
    let mut gaps = Vec::new();
    for index in 0..interfaces.len() {
        let interface = &interfaces[index];
        let methods = match expand(
            index,
            &interfaces,
            &interface_names,
            &mut expanded,
            &mut HashSet::new(),
        ) {
            Ok(methods) => methods,
            Err(reference_name) => {
                gaps.push(CodeIndexUnresolvedReferenceV1 {
                    from_occurrence: interface.occurrence.clone(),
                    reference_name,
                    kind: RelationEdgeKindV1::Implements,
                    evidence_span: interface.span,
                    unmodeled_import: None,
                    argument_count: None,
                });
                continue;
            }
        };
        let Some(candidates) = methods
            .iter()
            .map(|method| by_method.get(method).map_or(&[][..], Vec::as_slice))
            .min_by_key(|candidates| candidates.len())
        else {
            continue;
        };
        for owner in candidates {
            if !methods
                .iter()
                .all(|method| method_sets[owner].contains(method))
            {
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
    }
    edges.sort_by(edge_order);
    edges.dedup();
    gaps.sort();
    gaps.dedup();
    GoSatisfactionV1 { edges, gaps }
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
