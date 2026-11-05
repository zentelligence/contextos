//! The script files a workflow splits its code into, and the only code a script can import.
//!
//! A package lists its modules in `modules` of its `workflows.toml` or `library.toml`, each a `.rhai` file relative to
//! the package root: a vault package's folder under `registry/workflows/`, or an installed release. Every workflow of
//! the package may import them. A script imports one by that path
//! without the extension, `import "lib/dates" as dates;`, whichever file the import is in. The built-in library is
//! imported as `contextos:<module>`, and no declared module may be in `contextos/`, so an old name cannot shadow it. Nothing else can be imported: the
//! engine's resolver serves these and refuses every other name without looking for a file.
//!
//! A module holds functions and the imports they need, and nothing else, so importing one defines functions and runs
//! none of its code. Its imports are at its top level: Rhai resolves an import inside a function body against the
//! script that called the function, not the module it came from, so one there could resolve to the wrong module. Its text is part of the job's version, so a changed module is a new version of every job that
//! declares it.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Mutex, PoisonError};

use rhai::module_resolvers::ModuleResolver;
use rhai::{Engine, EvalAltResult, Module, Position, Scope, Shared};
use thiserror::Error;

use crate::caller::WorkflowFailureKind;
use crate::import_rules::{ImportRules, ImportTarget, ModuleKey, ModuleOwner};
use crate::package_contents::PackageKind;
use crate::package_name::PackageName;
use crate::script_library::built_in_module;
use crate::vault_libraries::LibraryCode;
use crate::workflow_files::{
    MAX_WORKFLOW_FILE_BYTES, WorkflowFile, WorkflowFileFailure, WorkflowFilePath, WorkflowFileSource,
};

/// The most modules one workflow or package may declare.
pub const MAX_MODULES: usize = 100;
/// The extension every module file has, and which its import name leaves out.
const MODULE_EXTENSION: &str = ".rhai";
/// The folder the built-in library is imported from, which no declared module may be in.
pub(crate) const BUILT_IN_PREFIX: &str = "contextos/";

/// The `.rhai` files a workflow or package declares in `modules`.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DeclaredModules(Vec<WorkflowFilePath>);

/// Why a `modules` list cannot be used.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("modules entry {path:?} {reason}")]
pub struct DeclaredModulesError {
    pub path: String,
    pub reason: String,
}

impl TryFrom<Vec<String>> for DeclaredModules {
    type Error = DeclaredModulesError;

    fn try_from(raw: Vec<String>) -> Result<Self, Self::Error> {
        if raw.len() > MAX_MODULES {
            return Err(DeclaredModulesError {
                path: raw.get(MAX_MODULES).cloned().unwrap_or_default(),
                reason: format!("is one more than the {MAX_MODULES} modules a workflow may declare"),
            });
        }
        let mut seen = BTreeSet::new();
        let mut modules = Vec::with_capacity(raw.len());
        for entry in raw {
            let refuse = |reason: &str| DeclaredModulesError {
                path: entry.clone(),
                reason: reason.to_owned(),
            };
            let path = WorkflowFilePath::try_from(entry.as_str()).map_err(|error| {
                refuse(&format!(
                    "is not a plain relative path within the workflow: {}",
                    error.reason
                ))
            })?;
            let named = path
                .segments()
                .last()
                .and_then(|name| name.strip_suffix(MODULE_EXTENSION))
                .is_some_and(|stem| !stem.is_empty());
            if !named {
                return Err(refuse("is not a `.rhai` file"));
            }
            if entry.starts_with(BUILT_IN_PREFIX) {
                return Err(refuse("is in `contextos/`, which holds the built-in library"));
            }
            if !seen.insert(entry.clone()) {
                return Err(refuse("is declared twice"));
            }
            modules.push(path);
        }
        Ok(Self(modules))
    }
}

impl DeclaredModules {
    /// Each declared module, in the order declared.
    #[must_use]
    pub fn entries(&self) -> &[WorkflowFilePath] {
        &self.0
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// A declared module that could not be read, and why.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("module {artefact} could not be read: {failure}", artefact = .owner.artefact_for(.path))]
pub struct ModuleUnreadable {
    /// Whose module it is, so a library's module is not taken for the job's own package's.
    pub owner: ModuleOwner,
    pub path: WorkflowFilePath,
    pub failure: WorkflowFileFailure,
}

impl ModuleUnreadable {
    /// The file as a failure reports it: `package:path` for a package's module, and the path alone for a vault
    /// workflow's.
    #[must_use]
    pub fn artefact(&self) -> String {
        self.owner.artefact_for(&self.path)
    }
}

/// One declared module and its text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModuleSource {
    owner: ModuleOwner,
    path: WorkflowFilePath,
    text: String,
}

impl ModuleSource {
    /// Who the module belongs to.
    #[must_use]
    pub fn owner(&self) -> &ModuleOwner {
        &self.owner
    }

    /// The module as imports and diagnostics key it: its owner and its import name.
    #[must_use]
    pub fn key(&self) -> ModuleKey {
        ModuleKey {
            owner: self.owner.clone(),
            name: self.import_name().to_owned(),
        }
    }

    /// The name the module's parsed source carries, which tells the resolver whose file an import is in. A package's
    /// module is `package:path` and a vault workflow's is its path, which cannot hold a colon, so no two owners share one.
    pub(crate) fn source_name(&self) -> String {
        self.owner.artefact_for(&self.path)
    }

    /// The module's file, relative to its workflow's root.
    #[must_use]
    pub fn path(&self) -> &WorkflowFilePath {
        &self.path
    }

    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The name a script imports the module by: its path without the extension.
    #[must_use]
    pub fn import_name(&self) -> &str {
        self.path
            .as_str()
            .strip_suffix(MODULE_EXTENSION)
            .unwrap_or(self.path.as_str())
    }
}

/// A module of `owner` at a path, and what reading it gave.
pub type ModuleRead<'a> = (
    &'a ModuleOwner,
    &'a WorkflowFilePath,
    Result<WorkflowFile, WorkflowFileFailure>,
);

impl TryFrom<ModuleRead<'_>> for ModuleSource {
    type Error = ModuleUnreadable;

    /// The module at `path` from what reading it gave, refused when it could not be read or is over
    /// [`MAX_WORKFLOW_FILE_BYTES`], which is checked again here so a source that read past the limit still cannot hand
    /// an oversized module to a script.
    fn try_from((owner, path, read): ModuleRead<'_>) -> Result<Self, Self::Error> {
        let unreadable = |failure| ModuleUnreadable {
            owner: owner.clone(),
            path: path.clone(),
            failure,
        };
        let file = read.map_err(unreadable)?;
        if u64::try_from(file.content.len()).map_or(true, |length| length > MAX_WORKFLOW_FILE_BYTES) {
            return Err(unreadable(WorkflowFileFailure::new(
                WorkflowFailureKind::FileTooLarge,
                format!("{path} is larger than the {MAX_WORKFLOW_FILE_BYTES} byte limit"),
            )));
        }
        Ok(Self {
            owner: owner.clone(),
            path: path.clone(),
            text: file.content,
        })
    }
}

/// A package's code in a script: its name, its package file, which says what it exports and which libraries it names,
/// and every module it declares.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageCode {
    name: PackageName,
    kind: PackageKind,
    modules: Vec<ModuleSource>,
}

/// A package whose modules are to be read: its name and its package file.
#[derive(Clone, Debug)]
pub struct PackageToRead {
    pub name: PackageName,
    pub kind: PackageKind,
}

impl PackageCode {
    /// Reads every module the package declares with `read`, for a host that reads its files without waiting, such as an
    /// installed package on a blocking thread. Every workflow of the package may import all of them: a module is named
    /// by its path from the package root, which is one file.
    ///
    /// # Errors
    ///
    /// Returns the first module that cannot be read, as `read` refuses it, or `FileTooLarge` for one over
    /// [`MAX_WORKFLOW_FILE_BYTES`].
    pub fn read(
        package: PackageToRead,
        mut read: impl FnMut(&WorkflowFilePath) -> Result<WorkflowFile, WorkflowFileFailure>,
    ) -> Result<Self, ModuleUnreadable> {
        let PackageToRead { name, kind } = package;
        let owner = ModuleOwner::Package(name.clone());
        let modules = kind
            .modules()
            .entries()
            .iter()
            .map(|path| ModuleSource::try_from((&owner, path, read(path))))
            .collect::<Result<_, _>>()?;
        Ok(Self { name, kind, modules })
    }

    /// The package's name.
    #[must_use]
    pub fn name(&self) -> &PackageName {
        &self.name
    }
}

impl From<&LibraryCode> for PackageCode {
    /// A library read from a vault's source as the package a run imports from, so it is reached as an installed
    /// library release is: each module belongs to the library's package, and the library's own `library.toml` says what
    /// it exports and names.
    fn from(library: &LibraryCode) -> Self {
        Self {
            name: PackageName::from(library.name()),
            kind: PackageKind::Library(library.manifest().clone()),
            modules: library.modules().to_vec(),
        }
    }
}

/// What a run's code is made of: the entry script, whose file it is in, and the code of every package it may reach.
#[derive(Clone, Debug)]
pub struct ScriptParts {
    pub entry: String,
    /// Whose file the entry script is: the package whose workflow it is, or none for a workflow kept in a vault.
    pub package: Option<PackageName>,
    /// Every package whose modules the run may load, keyed by name so none is there twice.
    pub packages: BTreeMap<PackageName, PackageCode>,
}

/// A workflow's code: its entry script and every module it may import, each keyed by its owner and import name.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WorkflowScript {
    entry: String,
    /// Whose file the entry script is, and so what its imports may name.
    root: ModuleOwner,
    modules: BTreeMap<ModuleKey, ModuleSource>,
    rules: ImportRules,
}

impl From<String> for WorkflowScript {
    /// A workflow that declares no modules.
    fn from(entry: String) -> Self {
        Self {
            entry,
            ..Self::default()
        }
    }
}

impl From<ScriptParts> for WorkflowScript {
    fn from(parts: ScriptParts) -> Self {
        let mut rules = ImportRules::default();
        let mut modules = BTreeMap::new();
        for code in parts.packages.into_values() {
            rules.add_package(&code.name, &code.kind);
            modules.extend(code.modules.into_iter().map(|module| (module.key(), module)));
        }
        Self {
            entry: parts.entry,
            root: parts.package.map_or(ModuleOwner::Workflow, ModuleOwner::Package),
            modules,
            rules,
        }
    }
}

impl WorkflowScript {
    /// Reads every module `declared` names through `source`, the same way a script reads its workflow's files, for a
    /// workflow kept in a vault.
    ///
    /// # Errors
    ///
    /// Returns the first module that cannot be read, as its source refuses it, or `FileTooLarge` for one over
    /// [`MAX_WORKFLOW_FILE_BYTES`].
    pub async fn load(
        entry: String,
        declared: &DeclaredModules,
        source: &dyn WorkflowFileSource,
    ) -> Result<Self, ModuleUnreadable> {
        let mut modules = BTreeMap::new();
        for path in declared.entries() {
            let module = ModuleSource::try_from((&ModuleOwner::Workflow, path, source.read(path).await))?;
            modules.insert(module.key(), module);
        }
        Ok(Self {
            entry,
            modules,
            ..Self::default()
        })
    }

    /// As [`WorkflowScript::load`], for a host that reads its files without waiting.
    ///
    /// # Errors
    ///
    /// As [`WorkflowScript::load`].
    pub fn read(
        entry: String,
        declared: &DeclaredModules,
        mut read: impl FnMut(&WorkflowFilePath) -> Result<WorkflowFile, WorkflowFileFailure>,
    ) -> Result<Self, ModuleUnreadable> {
        let mut modules = BTreeMap::new();
        for path in declared.entries() {
            let module = ModuleSource::try_from((&ModuleOwner::Workflow, path, read(path)))?;
            modules.insert(module.key(), module);
        }
        Ok(Self {
            entry,
            modules,
            ..Self::default()
        })
    }

    #[must_use]
    pub fn entry(&self) -> &str {
        &self.entry
    }

    /// Whose file the entry script is.
    #[must_use]
    pub fn root(&self) -> &ModuleOwner {
        &self.root
    }

    /// Every module, ordered by owner and then import name.
    pub fn modules(&self) -> impl Iterator<Item = &ModuleSource> {
        self.modules.values()
    }

    /// The packages other than the workflow's own that the script's files import from, directly or through the modules
    /// they import in turn. The walk starts at the entry script and follows import statements, so a name in a comment or a
    /// string, and a module nothing imports, reach nothing.
    pub(crate) fn packages_imported_from(&self, engine: &Engine) -> BTreeSet<PackageName> {
        self.modules_reached(engine)
            .into_iter()
            .filter_map(|key| match key.owner {
                ModuleOwner::Package(package) if ModuleOwner::Package(package.clone()) != self.root => Some(package),
                ModuleOwner::Package(_) | ModuleOwner::Workflow => None,
            })
            .collect()
    }

    /// Every module the walk from the entry script reaches through import statements.
    fn modules_reached(&self, engine: &Engine) -> BTreeSet<ModuleKey> {
        let imports_of = |owner: &ModuleOwner, text: &str| -> Vec<ModuleKey> {
            let Ok(ast) = engine.compile(text) else {
                return Vec::new();
            };
            let mut keys = Vec::new();
            ast.walk(&mut |nodes: &[rhai::ASTNode]| {
                if let Some(rhai::ASTNode::Stmt(rhai::Stmt::Import(import, _))) = nodes.last()
                    && let rhai::Expr::StringConstant(name, _) = &import.0
                    && let Ok(ImportTarget::Module(key)) = self.import_target(owner, name)
                {
                    keys.push(key);
                }
                true
            });
            keys
        };
        // The walk starts at the entry script and follows imports, so a declared module nothing imports adds nothing.
        let mut pending: Vec<ModuleKey> = imports_of(&self.root, &self.entry);
        let mut reached = BTreeSet::new();
        while let Some(key) = pending.pop() {
            if !reached.insert(key.clone()) {
                continue;
            }
            if let Some(module) = self.modules.get(&key) {
                pending.extend(imports_of(module.owner(), module.text()));
            }
        }
        reached
    }

    /// The module `key` names, if the script carries it.
    pub(crate) fn module(&self, key: &ModuleKey) -> Option<&ModuleSource> {
        self.modules.get(key)
    }

    /// What `import "{name}"` in a file of `importer` names.
    pub(crate) fn import_target(
        &self,
        importer: &ModuleOwner,
        name: &str,
    ) -> Result<ImportTarget, crate::package::PackageError> {
        self.rules.target(importer, name)
    }
}

/// What a refused import says when the name is neither declared nor built in.
pub(crate) fn undeclared(name: &str) -> String {
    format!(
        "`import \"{name}\"` names no module: a script imports a module its workflow declares in `modules`, by its path \
         without `.rhai`, an exported module of a package it depends on, such as `acme-text:strings`, or a built-in \
         module, such as `contextos:text`"
    )
}

/// What an import that closes a loop of modules says.
pub(crate) fn cyclic(name: &str) -> String {
    format!("`import \"{name}\"` imports a module that is already being imported, so the modules import each other")
}

/// Serves a run's imports: the built-in library and the modules its workflow declares, each built once, when first
/// imported. Every other name is refused without a file being looked for.
pub(crate) struct DeclaredModuleResolver {
    /// Whose file each parsed module's source name stands for, so an import is checked against what its own file's owner
    /// may import.
    owners: BTreeMap<String, ModuleOwner>,
    script: WorkflowScript,
    built: Mutex<BTreeMap<ModuleKey, Shared<Module>>>,
    building: Mutex<BTreeSet<ModuleKey>>,
}

impl DeclaredModuleResolver {
    pub(crate) fn new(script: WorkflowScript) -> Self {
        Self {
            owners: script
                .modules()
                .map(|module| (module.source_name(), module.owner().clone()))
                .collect(),
            script,
            built: Mutex::new(BTreeMap::new()),
            building: Mutex::new(BTreeSet::new()),
        }
    }

    fn build(&self, engine: &Engine, module: &ModuleSource) -> Result<Shared<Module>, Box<EvalAltResult>> {
        let mut ast = engine.compile(module.text()).map_err(|error| {
            Box::new(EvalAltResult::ErrorInModule(
                module.path().to_string(),
                Box::new(error.into()),
                Position::NONE,
            ))
        })?;
        ast.set_source(module.source_name());
        // Validation refuses a module holding anything but functions and imports. Checked again here, because a run
        // does not depend on validation having happened, and evaluating the module runs its top-level statements.
        if let Some(statement) = ast
            .statements()
            .iter()
            .find(|statement| !is_module_statement(statement))
        {
            return Err(Box::new(EvalAltResult::ErrorInModule(
                module.path().to_string(),
                Box::new(EvalAltResult::ErrorRuntime(
                    MODULE_HOLDS_CODE.into(),
                    statement.position(),
                )),
                Position::NONE,
            )));
        }
        if let Some(position) = import_inside_a_function(&ast) {
            return Err(Box::new(EvalAltResult::ErrorInModule(
                module.path().to_string(),
                Box::new(EvalAltResult::ErrorRuntime(
                    MODULE_IMPORT_NOT_TOP_LEVEL.into(),
                    position,
                )),
                Position::NONE,
            )));
        }
        let built = Shared::new(Module::eval_ast_as_new(Scope::new(), &ast, engine)?);
        self.built
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(module.key(), Shared::clone(&built));
        Ok(built)
    }
}

/// What a module holding a statement other than a function, an import, or an exported constant says.
pub(crate) const MODULE_HOLDS_CODE: &str = "a module holds only functions, the imports they need, and exported \
     constants whose values are written out, such as `export const FOLDERS = [\"inbox\"];`: move this statement into a \
     function";

/// Whether `statement` may stand at a module's top level: an import, or `export const` with a value written out in
/// full (literals, and arrays and maps of them), so building the module runs none of its code.
pub(crate) fn is_module_statement(statement: &rhai::Stmt) -> bool {
    match statement {
        rhai::Stmt::Import(..) => true,
        rhai::Stmt::Var(definition, flags, _) => {
            flags.contains(rhai::ASTFlags::EXPORTED)
                && flags.contains(rhai::ASTFlags::CONSTANT)
                && definition.1.is_constant()
        }
        _ => false,
    }
}
/// What an import inside a module's function says.
pub(crate) const MODULE_IMPORT_NOT_TOP_LEVEL: &str = "a module imports only at its top level, because an import inside \
     a function would be resolved against whichever script called it: move this import above the functions";

/// What an import inside the entry script's function says.
pub(crate) const ENTRY_IMPORT_NOT_TOP_LEVEL: &str = "the entry script imports only at its top level, because a function \
     can be handed to a module's function and run there, where an import in its body would not resolve against this \
     file: move this import above the functions";

/// Where the first import inside one of `ast`'s functions is, if there is one.
pub(crate) fn import_inside_a_function(ast: &rhai::AST) -> Option<Position> {
    let mut found = None;
    ast.clone_functions_only().walk(&mut |nodes: &[rhai::ASTNode]| {
        if let Some(rhai::ASTNode::Stmt(statement @ rhai::Stmt::Import(..))) = nodes.last() {
            found = Some(statement.position());
            return false;
        }
        true
    });
    found
}

impl ModuleResolver for DeclaredModuleResolver {
    fn resolve(
        &self,
        engine: &Engine,
        source: Option<&str>,
        path: &str,
        position: Position,
    ) -> Result<Shared<Module>, Box<EvalAltResult>> {
        let refuse = |why: String| Box::new(EvalAltResult::ErrorRuntime(why.into(), position));
        // The entry script carries no source name; every module's parsed source names the file it came from. A source
        // that is neither is refused rather than guessed at, since the owner decides what may be imported.
        let importer = match source {
            None => self.script.root().clone(),
            Some(name) => self.owners.get(name).cloned().ok_or_else(|| {
                refuse(format!(
                    "`import \"{path}\"` is in a file that is not part of this workflow"
                ))
            })?,
        };
        let key = match self.script.import_target(&importer, path) {
            Err(error) => return Err(refuse(error.to_string())),
            Ok(ImportTarget::BuiltIn) => {
                return built_in_module(path).ok_or_else(|| refuse(undeclared(path)));
            }
            Ok(ImportTarget::Nothing) => return Err(refuse(undeclared(path))),
            Ok(ImportTarget::Module(key)) => key,
        };
        if let Some(module) = self.built.lock().unwrap_or_else(PoisonError::into_inner).get(&key) {
            return Ok(Shared::clone(module));
        }
        let Some(module) = self.script.module(&key) else {
            return Err(refuse(undeclared(path)));
        };
        if !self
            .building
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key.clone())
        {
            return Err(refuse(cyclic(path)));
        }
        let built = self.build(engine, module);
        self.building
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&key);
        built
    }
}

#[cfg(test)]
#[path = "script_modules_test.rs"]
mod tests;

#[cfg(test)]
#[path = "script_owners_test.rs"]
mod owner_tests;
