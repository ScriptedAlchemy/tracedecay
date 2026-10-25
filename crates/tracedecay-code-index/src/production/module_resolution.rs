//! Python, Go, Java, and Ruby call binding over the sealed file set.
//!
//! Each language binds a call through its own import form and one module
//! rule, using source-path metadata and fallibly loaded compact binding
//! facts: each file's parser-attested import rows and the package, module,
//! and class symbols its extractor published.
//!
//! - Python: `from m import f`, `import m as a` / `import a.b` with dotted
//!   member paths, relative imports, globs, and names a package's
//!   `__init__.py` imports. An absolute module is found under a source root,
//!   a directory that is not itself a regular package.
//! - Go: `pkg.F()` through an import whose path the `go.mod` module path
//!   maps to a project directory, bare calls into the same package, and dot
//!   imports.
//! - Java: `C.m()` through a class import, a same-package class, or a
//!   package glob, and static member imports.
//! - Ruby: `A::B.m()` on a constant defined by the files a
//!   `require`/`require_relative` chain loads.
//!
//! A binding into project code that names no unique callable is a typed
//! gap, disclosed so `callers` and `file_dependents` report partial
//! coverage; a binding of an external module binds nothing and is no gap.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

use tracedecay_code_extraction::{CallableArityV1, ImportModuleKindV1, ImportNamespaceV1};
use tracedecay_domain::{NodeKind, RelationEdgeKindV1, SymbolOccurrenceId};

use super::CodeIndexProductionErrorV1;
use super::resolution_view::ResolutionMemoryChangeV1;
use super::resolution_view::{
    ResolutionFileFactsV1, ResolutionFileV1, reserve_resolution_map, reserve_resolution_set,
    reserve_resolution_vec,
};
use super::typescript_resolution::{ImportBindingOutcomeV1, join_normalized, split_parent};
use crate::chunks::relation_target_kind_is_compatible;
use crate::chunks::{CodeIndexImportEvidenceV1, CodeIndexUnresolvedReferenceV1};
use crate::lineage::LineageSymbolRecordV1;

/// Deepest re-export chain (`__init__.py` imports, globs) followed before a
/// cycle or an unusually deep forwarding tree is treated as unresolved.
const MAX_FORWARDING_DEPTH: usize = 16;

/// Languages whose calls bind through this index.
pub(super) fn is_module_import_language(language: &str) -> bool {
    matches!(language, "python" | "go" | "java" | "ruby")
}

pub(super) fn caller_scope<'r>(
    language: &str,
    reference: &'r CodeIndexUnresolvedReferenceV1,
) -> Option<&'r SymbolOccurrenceId> {
    (language == "ruby").then_some(&reference.from_occurrence)
}

type SymbolRef = (usize, Arc<LineageSymbolRecordV1>);

/// A module path under a source root to its `(root, target)` candidates.
type PythonModulesV1 = HashMap<String, Vec<(String, PythonModuleV1)>>;

/// What one step of module resolution reached.
#[derive(Clone, Debug)]
enum TargetV1 {
    Symbol(SymbolRef),
    /// A Python module file (`m.py` or a package `__init__.py`).
    ModuleFile(usize),
    /// A Python namespace package directory without `__init__.py`.
    ModuleDir(String),
    /// A member of a function or value: a runtime attribute no module rule
    /// claims.
    Opaque,
    External,
    /// Project code that does not define the name, or several candidates.
    Unresolved,
}

pub(super) struct ModuleImportIndexV1<'a, T> {
    /// Import-module source paths to their file index.
    sources: HashMap<&'a str, usize>,
    /// Every source, by index; only import-module binding facts are read.
    files: &'a [T],
    /// `(language, directory)` to the source files directly inside it.
    dirs: HashMap<(&'a str, &'a str), Vec<usize>>,
    /// Every symbol by exact qualified name.
    by_qualified: HashMap<&'a str, Vec<(usize, &'a Arc<LineageSymbolRecordV1>)>>,
    /// Directories holding an `__init__.py`: regular Python packages.
    python_packages: HashSet<&'a str>,
    python_modules: PythonModulesV1,
    /// Directories of Python sources that are not regular packages.
    python_namespace_dirs: HashSet<String>,
    /// `go.mod` module path and root directory, longest path first.
    go_modules: Vec<(&'a str, &'a str)>,
    /// Every trailing path of a directory holding Go sources.
    go_dir_suffixes: HashSet<&'a str>,
    /// File index to its Go package clause name.
    go_packages: HashMap<usize, &'a str>,
    /// Java package name to the files declaring it.
    java_packages: HashMap<&'a str, Vec<usize>>,
    /// File index to its Java package name.
    java_file_packages: HashMap<usize, &'a str>,
    /// Ruby symbols by occurrence: a caller's lexical module nesting.
    ruby_symbols: HashMap<&'a SymbolOccurrenceId, &'a LineageSymbolRecordV1>,
    /// Constant names a Ruby module or class definition introduces.
    ruby_constants: HashSet<&'a str>,
    /// A Ruby path under a `lib/` load path to the files it names.
    ruby_load_path: HashMap<&'a str, Vec<usize>>,
    /// Per Ruby file, the files its require chain loads (itself included).
    ruby_loaded: Vec<OnceLock<(Vec<usize>, bool)>>,
    /// Declared parameter lists, by symbol.
    arities: HashMap<&'a SymbolOccurrenceId, CallableArityV1>,
}

#[derive(Clone, Debug)]
enum PythonModuleV1 {
    File(usize),
    Dir(String),
}

impl<'a, T: ResolutionFileV1> ModuleImportIndexV1<'a, T> {
    pub(super) fn release_file_memo(&mut self, index: usize) {
        self.ruby_loaded[index].take();
    }

    pub(super) fn new(files: &'a [T]) -> Result<Self, CodeIndexProductionErrorV1> {
        Self::new_charged(files, &mut |_| Ok(()))
    }

    pub(super) fn new_charged(
        files: &'a [T],
        charge: &mut dyn FnMut(ResolutionMemoryChangeV1) -> Result<(), CodeIndexProductionErrorV1>,
    ) -> Result<Self, CodeIndexProductionErrorV1> {
        charge(ResolutionMemoryChangeV1::Retain(
            files
                .len()
                .saturating_mul(std::mem::size_of::<OnceLock<(Vec<usize>, bool)>>()),
        ))?;
        let mut index = Self {
            sources: HashMap::new(),
            files,
            dirs: HashMap::new(),
            by_qualified: HashMap::new(),
            python_packages: HashSet::new(),
            python_modules: HashMap::new(),
            python_namespace_dirs: HashSet::new(),
            go_modules: Vec::new(),
            go_dir_suffixes: HashSet::new(),
            go_packages: HashMap::new(),
            java_packages: HashMap::new(),
            java_file_packages: HashMap::new(),
            ruby_symbols: HashMap::new(),
            ruby_constants: HashSet::new(),
            ruby_load_path: HashMap::new(),
            ruby_loaded: (0..files.len()).map(|_| OnceLock::new()).collect(),
            arities: HashMap::new(),
        };
        for (file_index, file) in files.iter().enumerate() {
            if is_module_import_language(file.language()) {
                index.register(file_index, file.binding()?, charge)?;
            }
        }
        index
            .go_modules
            .sort_by(|left, right| right.0.len().cmp(&left.0.len()).then(left.cmp(right)));
        (index.python_modules, index.python_namespace_dirs) =
            index.python_module_candidates(charge)?;
        Ok(index)
    }

    /// Index one Python, Go, Java, or Ruby file: its path, directory, and
    /// symbols, and the package, module, and load-path facts its language
    /// resolves through.
    fn register(
        &mut self,
        file_index: usize,
        file: &'a dyn ResolutionFileFactsV1,
        charge: &mut dyn FnMut(ResolutionMemoryChangeV1) -> Result<(), CodeIndexProductionErrorV1>,
    ) -> Result<(), CodeIndexProductionErrorV1> {
        let language = file.language();
        let path = file.logical_path();
        let (dir, name) = split_parent(path);
        if language == "go" && name == "go.mod" {
            if let Some(module) = file
                .symbols()
                .iter()
                .find(|symbol| symbol.kind == NodeKind::Module.as_str())
            {
                reserve_resolution_vec(&mut self.go_modules, 1, charge)?;
                self.go_modules.push((module.simple_name.as_str(), dir));
            }
            return Ok(());
        }
        if !self.sources.contains_key(path) {
            reserve_resolution_map(&mut self.sources, charge)?;
        }
        self.sources.insert(path, file_index);
        if !self.dirs.contains_key(&(language, dir)) {
            reserve_resolution_map(&mut self.dirs, charge)?;
        }
        reserve_resolution_vec(self.dirs.entry((language, dir)).or_default(), 1, charge)?;
        self.dirs
            .entry((language, dir))
            .or_default()
            .push(file_index);
        for symbol in file.symbols() {
            if !self
                .by_qualified
                .contains_key(symbol.qualified_name.as_str())
            {
                reserve_resolution_map(&mut self.by_qualified, charge)?;
            }
            reserve_resolution_vec(
                self.by_qualified
                    .entry(symbol.qualified_name.as_str())
                    .or_default(),
                1,
                charge,
            )?;
            self.by_qualified
                .entry(symbol.qualified_name.as_str())
                .or_default()
                .push((file_index, symbol));
            self.register_declaration(language, file_index, symbol, charge)?;
        }
        for row in file.callable_arities() {
            if !self.arities.contains_key(&row.occurrence) {
                reserve_resolution_map(&mut self.arities, charge)?;
            }
            self.arities.insert(&row.occurrence, row.arity);
        }
        match language {
            "python" if name == "__init__.py" => {
                if !self.python_packages.contains(dir) {
                    reserve_resolution_set(&mut self.python_packages, charge)?;
                }
                self.python_packages.insert(dir);
            }
            "go" => {
                let mut suffix = dir;
                if !self.go_dir_suffixes.contains(suffix) {
                    reserve_resolution_set(&mut self.go_dir_suffixes, charge)?;
                }
                self.go_dir_suffixes.insert(suffix);
                while let Some((_, rest)) = suffix.split_once('/') {
                    suffix = rest;
                    if !self.go_dir_suffixes.contains(suffix) {
                        reserve_resolution_set(&mut self.go_dir_suffixes, charge)?;
                    }
                    self.go_dir_suffixes.insert(suffix);
                }
            }
            "ruby" => {
                let under_lib = path
                    .strip_prefix("lib/")
                    .or_else(|| path.rfind("/lib/").map(|at| &path[at + "/lib/".len()..]));
                if let Some(under_lib) = under_lib {
                    if !self.ruby_load_path.contains_key(under_lib) {
                        reserve_resolution_map(&mut self.ruby_load_path, charge)?;
                    }
                    reserve_resolution_vec(
                        self.ruby_load_path.entry(under_lib).or_default(),
                        1,
                        charge,
                    )?;
                    self.ruby_load_path
                        .entry(under_lib)
                        .or_default()
                        .push(file_index);
                }
            }
            "java" if !self.java_file_packages.contains_key(&file_index) => {
                if !self.java_packages.contains_key("") {
                    reserve_resolution_map(&mut self.java_packages, charge)?;
                }
                reserve_resolution_vec(self.java_packages.entry("").or_default(), 1, charge)?;
                self.java_packages.entry("").or_default().push(file_index);
            }
            _ => {}
        }
        Ok(())
    }

    /// Record a Go package clause, a Java package, or a Ruby constant.
    fn register_declaration(
        &mut self,
        language: &str,
        file_index: usize,
        symbol: &'a LineageSymbolRecordV1,
        charge: &mut dyn FnMut(ResolutionMemoryChangeV1) -> Result<(), CodeIndexProductionErrorV1>,
    ) -> Result<(), CodeIndexProductionErrorV1> {
        let name = symbol.simple_name.as_str();
        match language {
            "go" if symbol.kind == NodeKind::GoPackage.as_str() => {
                if !self.go_packages.contains_key(&file_index) {
                    reserve_resolution_map(&mut self.go_packages, charge)?;
                }
                self.go_packages.insert(file_index, name);
            }
            "java" if symbol.kind == NodeKind::Package.as_str() => {
                if !self.java_packages.contains_key(name) {
                    reserve_resolution_map(&mut self.java_packages, charge)?;
                }
                reserve_resolution_vec(self.java_packages.entry(name).or_default(), 1, charge)?;
                self.java_packages.entry(name).or_default().push(file_index);
                if !self.java_file_packages.contains_key(&file_index) {
                    reserve_resolution_map(&mut self.java_file_packages, charge)?;
                }
                self.java_file_packages.insert(file_index, name);
            }
            "ruby" => {
                if !self.ruby_symbols.contains_key(&symbol.occurrence) {
                    reserve_resolution_map(&mut self.ruby_symbols, charge)?;
                }
                self.ruby_symbols.insert(&symbol.occurrence, symbol);
                if symbol.kind == NodeKind::Module.as_str()
                    || symbol.kind == NodeKind::Class.as_str()
                {
                    if !self.ruby_constants.contains(name) {
                        reserve_resolution_set(&mut self.ruby_constants, charge)?;
                    }
                    self.ruby_constants.insert(name);
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Every module path under a source root a Python source can be
    /// imported as, with its root: `a/b.py` and `a/b/__init__.py` as `a/b`,
    /// and each directory of sources that is not a regular package as a
    /// namespace package. A root is any ancestor that is not itself a
    /// regular package.
    fn python_module_candidates(
        &self,
        charge: &mut dyn FnMut(ResolutionMemoryChangeV1) -> Result<(), CodeIndexProductionErrorV1>,
    ) -> Result<(PythonModulesV1, HashSet<String>), CodeIndexProductionErrorV1> {
        let mut modules = PythonModulesV1::new();
        let mut namespace_dirs = HashSet::new();
        for (path, file_index) in &self.sources {
            let Some(stem) = path.strip_suffix(".py") else {
                continue;
            };
            let module = match stem.strip_suffix("/__init__") {
                Some(package) => package,
                None if stem == "__init__" => continue,
                None => stem,
            };
            insert_python_roots(
                &mut modules,
                &self.python_packages,
                module,
                0,
                || PythonModuleV1::File(*file_index),
                charge,
            )?;
            let mut dir = split_parent(path).0;
            while !dir.is_empty() {
                if !self.python_packages.contains(dir) && !namespace_dirs.contains(dir) {
                    reserve_resolution_set(&mut namespace_dirs, charge)?;
                    charge(ResolutionMemoryChangeV1::Retain(dir.len()))?;
                    namespace_dirs.insert(dir.to_owned());
                    insert_python_roots(
                        &mut modules,
                        &self.python_packages,
                        dir,
                        dir.len(),
                        || PythonModuleV1::Dir(dir.to_owned()),
                        charge,
                    )?;
                }
                dir = split_parent(dir).0;
            }
        }
        Ok((modules, namespace_dirs))
    }

    /// How the retained call `reference` in file `index` binds, or `None`
    /// when no import, package, or loaded file names its callee (a call on
    /// a local value, a builtin).
    pub(super) fn call_outcome(
        &self,
        index: usize,
        reference: &CodeIndexUnresolvedReferenceV1,
        charge: &mut dyn FnMut(ResolutionMemoryChangeV1) -> Result<(), CodeIndexProductionErrorV1>,
    ) -> Result<Option<ImportBindingOutcomeV1>, CodeIndexProductionErrorV1> {
        if reference.kind != RelationEdgeKindV1::Calls {
            return Ok(None);
        }
        let source = &self.files[index];
        let name = reference.reference_name.as_str();
        let language = source.language();
        let target = match language {
            "python" | "go" | "java" => {
                let Some(segments) = identifier_path(name, &["."]) else {
                    return Ok(None);
                };
                let file = source.binding()?;
                match language {
                    "python" => self.python_call(file, &segments)?,
                    "go" => self.go_call(index, file, &segments),
                    "java" => self.java_call(index, file, &segments, reference.argument_count),
                    _ => None,
                }
            }
            "ruby" => {
                let (absolute, name) = match name.strip_prefix("::") {
                    Some(rest) => (true, rest),
                    None => (false, name),
                };
                let Some(segments) = identifier_path(name, &[".", "::"]) else {
                    return Ok(None);
                };
                let Some(scope) = caller_scope(language, reference) else {
                    return Ok(None);
                };
                self.ruby_call(index, scope, absolute, &segments, charge)?
            }
            _ => None,
        };
        let Some(target) = target else {
            return Ok(None);
        };
        Ok(Some(match target {
            TargetV1::Symbol((target_index, symbol))
                if relation_target_kind_is_compatible(RelationEdgeKindV1::Calls, &symbol.kind) =>
            {
                ImportBindingOutcomeV1::Bound(target_index, symbol)
            }
            // A class or value called or read: construction or a value's
            // member, which no module rule claims.
            TargetV1::Symbol(_)
            | TargetV1::ModuleFile(_)
            | TargetV1::ModuleDir(_)
            | TargetV1::Opaque => ImportBindingOutcomeV1::ValueMember,
            TargetV1::External => ImportBindingOutcomeV1::External,
            TargetV1::Unresolved => ImportBindingOutcomeV1::Unresolved,
        }))
    }

    /// Every retained Python, Go, Java, and Ruby call that is a caller gap.
    pub(super) fn call_gaps_charged<'r>(
        &self,
        references: impl Iterator<Item = (usize, &'r CodeIndexUnresolvedReferenceV1)>,
        charge: &mut dyn FnMut(ResolutionMemoryChangeV1) -> Result<(), CodeIndexProductionErrorV1>,
    ) -> Result<Vec<CodeIndexUnresolvedReferenceV1>, CodeIndexProductionErrorV1> {
        let mut gaps = Vec::new();
        for (index, reference) in references {
            if is_module_import_language(self.files[index].language())
                && self.is_call_gap(index, reference, charge)?
            {
                super::resolution_view::reserve_resolution_vec(&mut gaps, 1, charge)?;
                charge(ResolutionMemoryChangeV1::Retain(
                    super::resident_bytes::unresolved_heap_bytes(reference),
                ))?;
                gaps.push(reference.clone());
            }
        }
        Ok(gaps)
    }

    /// Whether the retained call `reference` in file `index` is a caller gap:
    /// it names project code the seal could not bind, or it is a qualified
    /// call on a runtime value (dynamic dispatch). A bare call no import
    /// names is a builtin or a local in Python and Go, and a call on `self`
    /// that may be inherited in Java and Ruby.
    fn is_call_gap(
        &self,
        index: usize,
        reference: &CodeIndexUnresolvedReferenceV1,
        charge: &mut dyn FnMut(ResolutionMemoryChangeV1) -> Result<(), CodeIndexProductionErrorV1>,
    ) -> Result<bool, CodeIndexProductionErrorV1> {
        if reference.kind != RelationEdgeKindV1::Calls {
            return Ok(false);
        }
        Ok(match self.call_outcome(index, reference, charge)? {
            Some(ImportBindingOutcomeV1::Unresolved) => true,
            Some(
                ImportBindingOutcomeV1::Bound(..)
                | ImportBindingOutcomeV1::External
                | ImportBindingOutcomeV1::ValueMember,
            ) => false,
            None => {
                reference.reference_name.contains('.')
                    || (matches!(self.files[index].language(), "java" | "ruby")
                        && identifier_path(&reference.reference_name, &["::"]).is_some())
            }
        })
    }

    /// The unique symbol `container::name` in `file_index`. A package
    /// clause is no member: `package main` shares `main.go::main` with
    /// `func main`.
    fn member(&self, file_index: usize, container: &str, name: &str) -> Option<TargetV1> {
        self.member_accepting(file_index, container, name, None)
    }

    /// [`Self::member`] among the overloads whose declared parameters accept
    /// a call passing `arguments`; overloads the count cannot tell apart,
    /// or none that accepts it, are a gap.
    fn member_accepting(
        &self,
        file_index: usize,
        container: &str,
        name: &str,
        arguments: Option<u32>,
    ) -> Option<TargetV1> {
        let found = self
            .by_qualified
            .get(format!("{container}::{name}").as_str())
            .into_iter()
            .flatten()
            .filter(|(index, symbol)| {
                *index == file_index
                    && symbol.kind != NodeKind::GoPackage.as_str()
                    && symbol.kind != NodeKind::Package.as_str()
            })
            .copied()
            .collect::<Vec<_>>();
        if found.is_empty() {
            return None;
        }
        let accepting = found
            .into_iter()
            .filter(|(_, symbol)| {
                arguments
                    .zip(self.arities.get(&symbol.occurrence))
                    .is_none_or(|(arguments, arity)| arity.accepts(arguments))
            })
            .collect::<Vec<_>>();
        Some(match accepting.as_slice() {
            [symbol] => TargetV1::Symbol((symbol.0, Arc::clone(symbol.1))),
            _ => TargetV1::Unresolved,
        })
    }

    // --- Python ---------------------------------------------------------

    fn python_call(
        &self,
        file: &dyn ResolutionFileFactsV1,
        segments: &[&str],
    ) -> Result<Option<TargetV1>, CodeIndexProductionErrorV1> {
        let path = file.logical_path();
        // The longest dotted prefix the file binds: `import a.b` binds the
        // path `a.b`, every other form one name.
        for split in (1..=segments.len()).rev() {
            let head = segments[..split].join(".");
            let rows = named_rows(file, &head);
            let row = match rows.as_slice() {
                [] => continue,
                [row] => *row,
                _ => return Ok(Some(TargetV1::Unresolved)),
            };
            let start = self.python_row_target(path, row, 0)?;
            return self.python_walk(start, &segments[split..], 0).map(Some);
        }
        if let [name] = segments {
            return self.python_glob_member(file, name, 0);
        }
        Ok(None)
    }

    fn python_row_target(
        &self,
        path: &str,
        row: &CodeIndexImportEvidenceV1,
        depth: usize,
    ) -> Result<TargetV1, CodeIndexProductionErrorV1> {
        let module = self.python_module(path, &row.module_specifier);
        match row.imported_name.as_deref() {
            Some("*") | None => Ok(module),
            Some(imported) => self.python_member(module, imported, depth + 1),
        }
    }

    fn python_walk(
        &self,
        mut target: TargetV1,
        rest: &[&str],
        depth: usize,
    ) -> Result<TargetV1, CodeIndexProductionErrorV1> {
        for segment in rest {
            target = match target {
                TargetV1::ModuleFile(_) | TargetV1::ModuleDir(_) => {
                    self.python_member(target, segment, depth)?
                }
                TargetV1::Symbol((file_index, symbol))
                    if symbol.kind == NodeKind::Class.as_str() =>
                {
                    self.member(file_index, &symbol.qualified_name, segment)
                        .unwrap_or(TargetV1::Unresolved)
                }
                TargetV1::Symbol(_) | TargetV1::Opaque => return Ok(TargetV1::Opaque),
                TargetV1::External | TargetV1::Unresolved => return Ok(target),
            };
        }
        Ok(target)
    }

    /// `name` read from a module: its own definition, a name it imports, a
    /// submodule of a package, or a name one of its globs provides.
    fn python_member(
        &self,
        module: TargetV1,
        name: &str,
        depth: usize,
    ) -> Result<TargetV1, CodeIndexProductionErrorV1> {
        if depth > MAX_FORWARDING_DEPTH {
            return Ok(TargetV1::Unresolved);
        }
        let (module_file, dir) = match &module {
            TargetV1::ModuleFile(file_index) => {
                let path = self.files[*file_index].logical_path();
                let (dir, file_name) = split_parent(path);
                (
                    Some(*file_index),
                    (file_name == "__init__.py").then_some(dir),
                )
            }
            TargetV1::ModuleDir(dir) => (None, Some(dir.as_str())),
            TargetV1::Symbol(_) | TargetV1::Opaque | TargetV1::External | TargetV1::Unresolved => {
                return Ok(module.clone());
            }
        };
        if let Some(file_index) = module_file {
            let source = &self.files[file_index];
            let path = source.logical_path();
            if let Some(found) = self.member(file_index, path, name) {
                return Ok(found);
            }
            match named_rows(source.binding()?, name).as_slice() {
                [] => {}
                [row] => return self.python_row_target(path, row, depth),
                _ => return Ok(TargetV1::Unresolved),
            }
        }
        if let Some(dir) = dir {
            let submodule = join_normalized(dir, name);
            if let Some(found) = self.python_exact_module(&submodule) {
                return Ok(found);
            }
        }
        if let Some(file_index) = module_file
            && let Some(found) =
                self.python_glob_member(self.files[file_index].binding()?, name, depth + 1)?
        {
            return Ok(found);
        }
        Ok(TargetV1::Unresolved)
    }

    /// `name` through the file's `from m import *` rows: the unique module
    /// that provides it, a gap when a project glob cannot be followed.
    fn python_glob_member(
        &self,
        file: &dyn ResolutionFileFactsV1,
        name: &str,
        depth: usize,
    ) -> Result<Option<TargetV1>, CodeIndexProductionErrorV1> {
        let path = file.logical_path();
        let mut found = Vec::new();
        let mut unresolved = false;
        for row in file.imports().iter().filter(|row| row.is_glob) {
            match self.python_module(path, &row.module_specifier) {
                TargetV1::External => {}
                TargetV1::Unresolved => unresolved = true,
                module => match self.python_member(module, name, depth)? {
                    TargetV1::Unresolved => {}
                    target => found.push(target),
                },
            }
        }
        Ok(match found.as_slice() {
            [target] => Some(target.clone()),
            [] if unresolved => Some(TargetV1::Unresolved),
            [] => None,
            _ => Some(TargetV1::Unresolved),
        })
    }

    /// The module a specifier names from the file at `from_path`.
    fn python_module(&self, from_path: &str, specifier: &str) -> TargetV1 {
        let dots = specifier.bytes().take_while(|byte| *byte == b'.').count();
        let dotted = &specifier[dots..];
        if dots > 0 {
            let mut base = split_parent(from_path).0.to_owned();
            for _ in 1..dots {
                base = split_parent(&base).0.to_owned();
            }
            let module = if dotted.is_empty() {
                base
            } else {
                join_normalized(&base, &dotted.replace('.', "/"))
            };
            return self
                .python_exact_module(&module)
                .unwrap_or(TargetV1::Unresolved);
        }
        let module = dotted.replace('.', "/");
        let Some(candidates) = self.python_modules.get(&module) else {
            // A missing submodule of a project package is project code.
            let top = module.split('/').next().unwrap_or(&module);
            return if top != module && self.python_modules.contains_key(top) {
                TargetV1::Unresolved
            } else {
                TargetV1::External
            };
        };
        let chosen = match candidates.as_slice() {
            [(_, only)] => Some(only),
            _ => {
                // Several roots: the one the importing file lives under.
                let mut sharing = candidates
                    .iter()
                    .filter(|(root, _)| from_path.starts_with(root.as_str()))
                    .collect::<Vec<_>>();
                sharing.sort_by_key(|(root, _)| std::cmp::Reverse(root.len()));
                match sharing.as_slice() {
                    [(_, only)] => Some(only),
                    [(first, only), (second, _), ..] if first.len() > second.len() => Some(only),
                    _ => None,
                }
            }
        };
        match chosen {
            Some(PythonModuleV1::File(file_index)) => TargetV1::ModuleFile(*file_index),
            Some(PythonModuleV1::Dir(dir)) => TargetV1::ModuleDir(dir.clone()),
            None => TargetV1::Unresolved,
        }
    }

    /// The module at exactly this project path: `path.py`,
    /// `path/__init__.py`, or a directory of sources.
    fn python_exact_module(&self, module: &str) -> Option<TargetV1> {
        if let Some(file_index) = self.sources.get(format!("{module}.py").as_str()) {
            return Some(TargetV1::ModuleFile(*file_index));
        }
        let init = if module.is_empty() {
            "__init__.py".to_owned()
        } else {
            format!("{module}/__init__.py")
        };
        if let Some(file_index) = self.sources.get(init.as_str()) {
            return Some(TargetV1::ModuleFile(*file_index));
        }
        self.python_namespace_dirs
            .contains(module)
            .then(|| TargetV1::ModuleDir(module.to_owned()))
    }

    // --- Go -------------------------------------------------------------

    fn go_call(
        &self,
        index: usize,
        file: &dyn ResolutionFileFactsV1,
        segments: &[&str],
    ) -> Option<TargetV1> {
        match segments {
            [name] => {
                let dir = split_parent(file.logical_path()).0;
                let package = self.go_packages.get(&index).copied();
                let mut found = self.go_package_functions(dir, package, name);
                for row in file.imports().iter().filter(|row| row.is_glob) {
                    if let TargetV1::ModuleDir(dir) = self.go_package_dir(&row.module_specifier) {
                        found.extend(self.go_package_functions(&dir, None, name));
                    }
                }
                match found.as_slice() {
                    [] => None,
                    [symbol] => Some(TargetV1::Symbol(symbol.clone())),
                    _ => Some(TargetV1::Unresolved),
                }
            }
            [head, name] => {
                let row = match named_rows(file, head).as_slice() {
                    [] => return None,
                    [row] => *row,
                    _ => return Some(TargetV1::Unresolved),
                };
                Some(match self.go_package_dir(&row.module_specifier) {
                    TargetV1::ModuleDir(dir) => {
                        match self.go_package_functions(&dir, None, name).as_slice() {
                            [symbol] => TargetV1::Symbol(symbol.clone()),
                            _ => TargetV1::Unresolved,
                        }
                    }
                    other => other,
                })
            }
            _ => None,
        }
    }

    /// Package-level functions named `name` in the Go sources of `dir`,
    /// restricted to one package clause when `package` is given.
    fn go_package_functions(&self, dir: &str, package: Option<&str>, name: &str) -> Vec<SymbolRef> {
        self.dirs
            .get(&("go", dir))
            .into_iter()
            .flatten()
            .filter(|file_index| {
                package.is_none_or(|package| self.go_packages.get(file_index) == Some(&package))
            })
            .filter_map(|file_index| {
                let path = self.files[*file_index].logical_path();
                match self.member(*file_index, path, name)? {
                    TargetV1::Symbol(symbol) if symbol.1.kind == NodeKind::Function.as_str() => {
                        Some(symbol)
                    }
                    _ => None,
                }
            })
            .collect()
    }

    /// The indexed project directory Go import path `import_path` names.
    pub(super) fn go_project_package_dir(&self, import_path: &str) -> Option<String> {
        match self.go_package_dir(import_path) {
            TargetV1::ModuleDir(dir) => Some(dir),
            _ => None,
        }
    }

    /// The project directory an import path names through the longest
    /// `go.mod` module path it starts with. Without a `go.mod`, a path whose
    /// tail names a source directory is project code the seal cannot place.
    fn go_package_dir(&self, import_path: &str) -> TargetV1 {
        let module = self.go_modules.iter().find_map(|(module, root)| {
            let rest = import_path.strip_prefix(module)?;
            match rest.strip_prefix('/') {
                Some(rest) => Some(join_normalized(root, rest)),
                None => rest.is_empty().then(|| (*root).to_owned()),
            }
        });
        if let Some(dir) = module {
            return if self.dirs.contains_key(&("go", dir.as_str())) {
                TargetV1::ModuleDir(dir)
            } else {
                TargetV1::Unresolved
            };
        }
        if !self.go_modules.is_empty() {
            return TargetV1::External;
        }
        let mut suffix = import_path;
        loop {
            if self.go_dir_suffixes.contains(suffix) {
                return TargetV1::Unresolved;
            }
            match suffix.split_once('/') {
                Some((_, rest)) => suffix = rest,
                None => return TargetV1::External,
            }
        }
    }

    // --- Java -----------------------------------------------------------

    fn java_call(
        &self,
        index: usize,
        file: &dyn ResolutionFileFactsV1,
        segments: &[&str],
        arguments: Option<u32>,
    ) -> Option<TargetV1> {
        let (method, qualifier) = segments.split_last()?;
        if qualifier.is_empty() {
            return self.java_static_import(file, method, arguments);
        }
        let class = match qualifier {
            [simple] => self.java_class_name(index, file, simple)?,
            // A fully qualified `a.b.C.m()`.
            _ => {
                let qualified = qualifier.join(".");
                self.java_class(&qualified)?;
                qualified
            }
        };
        Some(self.java_class_member(&class, method, arguments))
    }

    /// A bare call through `import static a.C.m` or `import static a.C.*`:
    /// the named import decides; otherwise the unique glob class declaring
    /// the method, and a gap when a project glob class does not.
    fn java_static_import(
        &self,
        file: &dyn ResolutionFileFactsV1,
        method: &str,
        arguments: Option<u32>,
    ) -> Option<TargetV1> {
        let statics = file
            .imports()
            .iter()
            .filter(|row| row.namespace == ImportNamespaceV1::Value);
        if let Some(row) = statics
            .clone()
            .find(|row| !row.is_glob && row.local_name.as_deref() == Some(method))
        {
            return Some(self.java_class_member(&row.module_specifier, method, arguments));
        }
        let mut found = Vec::new();
        let mut project_glob = false;
        for row in statics.filter(|row| row.is_glob) {
            match self.java_class_member(&row.module_specifier, method, arguments) {
                TargetV1::External => {}
                TargetV1::Symbol(symbol) => found.push(symbol),
                _ => project_glob = true,
            }
        }
        match found.as_slice() {
            [symbol] => Some(TargetV1::Symbol(symbol.clone())),
            [] if !project_glob => None,
            _ => Some(TargetV1::Unresolved),
        }
    }

    /// The fully qualified class a simple name denotes in `file`: a class
    /// import, a class of the file's own package, or a package glob.
    fn java_class_name(
        &self,
        index: usize,
        file: &dyn ResolutionFileFactsV1,
        simple: &str,
    ) -> Option<String> {
        let type_rows = file
            .imports()
            .iter()
            .filter(|row| row.namespace == ImportNamespaceV1::Type);
        if let Some(row) = type_rows
            .clone()
            .find(|row| !row.is_glob && row.local_name.as_deref() == Some(simple))
        {
            return Some(format!("{}.{simple}", row.module_specifier));
        }
        let package = self.java_file_packages.get(&index).copied().unwrap_or("");
        let own = if package.is_empty() {
            simple.to_owned()
        } else {
            format!("{package}.{simple}")
        };
        if self.java_class(&own).is_some() {
            return Some(own);
        }
        let mut globbed = type_rows
            .filter(|row| row.is_glob)
            .map(|row| format!("{}.{simple}", row.module_specifier))
            .filter(|qualified| self.java_class(qualified).is_some());
        let first = globbed.next()?;
        globbed.next().is_none().then_some(first)
    }

    /// The project file and symbol of the top-level class `qualified`.
    fn java_class(&self, qualified: &str) -> Option<SymbolRef> {
        let (package, simple) = qualified.rsplit_once('.').unwrap_or(("", qualified));
        self.java_packages
            .get(package)
            .into_iter()
            .flatten()
            .find_map(|file_index| {
                match self.member(*file_index, self.files[*file_index].logical_path(), simple)? {
                    TargetV1::Symbol(symbol) => Some(symbol),
                    _ => None,
                }
            })
    }

    /// Method `method` of class `class`, the overload that accepts the
    /// call's `arguments`; a project class without that method (inherited)
    /// or without one such overload is a gap, a class outside the project
    /// binds nothing.
    fn java_class_member(&self, class: &str, method: &str, arguments: Option<u32>) -> TargetV1 {
        let Some((file_index, symbol)) = self.java_class(class) else {
            let package = class.rsplit_once('.').map_or("", |(package, _)| package);
            return if self.java_packages.contains_key(package) {
                TargetV1::Unresolved
            } else {
                TargetV1::External
            };
        };
        self.member_accepting(file_index, &symbol.qualified_name, method, arguments)
            .unwrap_or(TargetV1::Unresolved)
    }

    // --- Ruby -----------------------------------------------------------

    /// `A::B.m()` binds through Ruby's lexical constant lookup: the
    /// innermost module enclosing the caller that, with `A::B` appended,
    /// names a module or class defining `m` in a loaded file. A `::`-rooted
    /// path is looked up at the top level only.
    fn ruby_call(
        &self,
        index: usize,
        from: &SymbolOccurrenceId,
        absolute: bool,
        segments: &[&str],
        charge: &mut dyn FnMut(ResolutionMemoryChangeV1) -> Result<(), CodeIndexProductionErrorV1>,
    ) -> Result<Option<TargetV1>, CodeIndexProductionErrorV1> {
        let Some((method, constant)) = segments.split_last() else {
            return Ok(None);
        };
        if constant.is_empty()
            || !constant
                .iter()
                .all(|segment| segment.starts_with(|c: char| c.is_ascii_uppercase()))
        {
            return Ok(None);
        }
        let constant = constant.join("::");
        let path = self.files[index].logical_path();
        let nesting = if absolute {
            Vec::new()
        } else {
            // The caller's own name ends its qualified path.
            let Some(mut scope) = self
                .ruby_symbols
                .get(from)
                .and_then(|symbol| symbol.qualified_name.strip_prefix(path)?.strip_prefix("::"))
                .map(|scope| scope.split("::").collect::<Vec<_>>())
            else {
                return Ok(Some(TargetV1::Unresolved));
            };
            scope.pop();
            scope
        };
        let closure = &self.ruby_loaded[index];
        let (loaded, unresolved_require) = match closure.get() {
            Some(loaded) => loaded,
            None => {
                let loaded = self.ruby_require_closure(index, charge)?;
                closure.get_or_init(|| loaded)
            }
        };
        for depth in (0..=nesting.len()).rev() {
            let qualified = match nesting[..depth].join("::") {
                prefix if prefix.is_empty() => constant.clone(),
                prefix => format!("{prefix}::{constant}"),
            };
            let found = loaded
                .iter()
                .filter_map(|file_index| {
                    let path = self.files[*file_index].logical_path();
                    self.member(*file_index, &format!("{path}::{qualified}"), method)
                })
                .collect::<Vec<_>>();
            match found.as_slice() {
                [] => {}
                [target] => return Ok(Some(target.clone())),
                _ => return Ok(Some(TargetV1::Unresolved)),
            }
        }
        Ok(Some(
            if !*unresolved_require && !self.ruby_constants.contains(segments[0]) {
                TargetV1::External
            } else {
                TargetV1::Unresolved
            },
        ))
    }

    /// The files `index` loads through `require_relative` and project
    /// `require` chains, itself first, and whether a project-relative
    /// require named no indexed file.
    fn ruby_require_closure(
        &self,
        index: usize,
        charge: &mut dyn FnMut(ResolutionMemoryChangeV1) -> Result<(), CodeIndexProductionErrorV1>,
    ) -> Result<(Vec<usize>, bool), CodeIndexProductionErrorV1> {
        let mut loaded = Vec::new();
        let mut seen = HashSet::new();
        reserve_resolution_vec(&mut loaded, 1, charge)?;
        reserve_resolution_set(&mut seen, charge)?;
        loaded.push(index);
        seen.insert(index);
        let mut cursor = 0;
        let mut unresolved = false;
        // The retained discovery order is also the breadth-first queue.
        while cursor < loaded.len() {
            let current = loaded[cursor];
            cursor += 1;
            let file = self.files[current].binding()?;
            let (dir, _) = split_parent(file.logical_path());
            for row in file.imports() {
                let target = self.ruby_require_target(dir, row);
                match target {
                    Some(target) => {
                        if !seen.contains(&target) {
                            reserve_resolution_vec(&mut loaded, 1, charge)?;
                            reserve_resolution_set(&mut seen, charge)?;
                            seen.insert(target);
                            loaded.push(target);
                        }
                    }
                    None if row.module_kind == ImportModuleKindV1::ProjectRelative => {
                        unresolved = true;
                    }
                    None => {}
                }
            }
        }
        Ok((loaded, unresolved))
    }

    /// The indexed file one require names: relative to the requiring file,
    /// or a bare path under a `lib/` load path.
    fn ruby_require_target(&self, dir: &str, row: &CodeIndexImportEvidenceV1) -> Option<usize> {
        let specifier = row.module_specifier.as_str();
        let with_extension = if specifier.ends_with(".rb") {
            specifier.to_owned()
        } else {
            format!("{specifier}.rb")
        };
        if row.module_kind == ImportModuleKindV1::ProjectRelative {
            return self
                .sources
                .get(join_normalized(dir, &with_extension).as_str())
                .copied();
        }
        match self.ruby_load_path.get(with_extension.as_str())?.as_slice() {
            [only] => Some(*only),
            _ => None,
        }
    }
}

/// Insert `module` (a `/`-separated project path) under every root it can
/// be imported from: each ancestor directory that is not a regular package.
fn insert_python_roots(
    modules: &mut PythonModulesV1,
    packages: &HashSet<&str>,
    module: &str,
    target_heap_bytes: usize,
    target: impl Fn() -> PythonModuleV1,
    charge: &mut dyn FnMut(ResolutionMemoryChangeV1) -> Result<(), CodeIndexProductionErrorV1>,
) -> Result<(), CodeIndexProductionErrorV1> {
    let mut root_end = 0;
    loop {
        let root = &module[..root_end];
        if !packages.contains(root.strip_suffix('/').unwrap_or(root)) {
            let key = &module[root_end..];
            if !modules.contains_key(key) {
                reserve_resolution_map(modules, charge)?;
                charge(ResolutionMemoryChangeV1::Retain(key.len()))?;
                modules.insert(key.to_owned(), Vec::new());
            }
            let rows = modules.get_mut(key).ok_or_else(|| {
                CodeIndexProductionErrorV1::Contract(
                    "Python module insertion lost its rows".to_owned(),
                )
            })?;
            reserve_resolution_vec(rows, 1, charge)?;
            charge(ResolutionMemoryChangeV1::Retain(
                root.len().saturating_add(target_heap_bytes),
            ))?;
            rows.push((root.to_owned(), target()));
        }
        match module[root_end..].find('/') {
            Some(offset) => root_end += offset + 1,
            None => break,
        }
    }
    Ok(())
}

/// The file's private import rows binding `local`.
fn named_rows<'f>(
    file: &'f dyn ResolutionFileFactsV1,
    local: &str,
) -> Vec<&'f CodeIndexImportEvidenceV1> {
    file.imports()
        .iter()
        .filter(|row| !row.is_public && !row.is_glob && row.local_name.as_deref() == Some(local))
        .collect()
}

/// A call path split on `separators`, `None` unless every segment is a
/// plain identifier (a call on a call result, an index, or a literal).
fn identifier_path<'n>(name: &'n str, separators: &[&str]) -> Option<Vec<&'n str>> {
    let mut segments = vec![name];
    for separator in separators {
        segments = segments
            .into_iter()
            .flat_map(|segment| segment.split(separator))
            .collect();
    }
    // Ruby method names may end in `?` or `!`.
    segments
        .iter()
        .all(|segment| {
            let body = segment.strip_suffix(['?', '!']).unwrap_or(segment);
            body.starts_with(|c: char| c.is_alphabetic() || c == '_')
                && body.chars().all(|c| c.is_alphanumeric() || c == '_')
        })
        .then_some(segments)
}
