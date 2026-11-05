//! What static validation of a job reports, and the one check that can be made on a script without running it.
//!
//! Validation reports every failure it finds, each with its own stable code and the artefact it concerns, rather than
//! stopping at the first. A check that needs something that cannot be reached is reported as unverifiable, which keeps
//! the job from passing without recording an outage as a defect in the job.

use std::collections::{BTreeMap, BTreeSet};

use rhai::{AST, ASTNode, Expr, ParseError, Position, Stmt};

use crate::import_rules::{ImportTarget, ModuleKey, ModuleOwner};
use crate::package::PackageError;
use crate::script_engine::{on_script_thread, refusal, script_engine};
use crate::script_modules::{
    ENTRY_IMPORT_NOT_TOP_LEVEL, MODULE_HOLDS_CODE, MODULE_IMPORT_NOT_TOP_LEVEL, ModuleSource, WorkflowScript, cyclic,
    import_inside_a_function, is_module_statement, undeclared,
};
use crate::workflow_files::WorkflowFilePath;

/// The revision of the rules static validation applies. A stored result made under an earlier revision is made again
/// once, so tightening the rules reaches a job whose version has not changed. Revision 2 refuses `eval` and an import of anything
/// but a declared or built-in module, and sets the nesting limits every build shares.
pub const RULES_REVISION: u32 = 2;

/// A kind of failure static validation can find.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ValidationCode {
    /// A file the job needs is not there.
    ArtefactMissing,
    /// A file the job needs does not parse or does not match its schema.
    ArtefactInvalid,
    /// The entry script or a module does not compile, or imports what it cannot.
    ScriptSyntax,
    /// The job's values do not satisfy the workflow's declared inputs.
    InputInvalid,
    /// The workflow may call a server that is not configured.
    ServerNotConfigured,
    /// The job's owner does not resolve.
    OwnerUnresolved,
    /// A tool the workflow may call is not offered by its server.
    ToolNotFound,
    /// The `entity` a job or its workflow declares is neither `vault` nor a configured entity.
    EntityUnresolved,
}

/// A package error that is not a failure to resolve a package's dependencies.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NotADependencyFailure;

impl TryFrom<&PackageError> for ValidationCode {
    type Error = NotADependencyFailure;

    /// The code a failure to resolve a package's libraries is reported with. A library or release that is not there
    /// (a library that is missing or unreadable, or requirements nothing satisfies)
    /// is a missing artefact, as a release of the job's own package that does not resolve is. A loop, and a set of
    /// libraries past a limit, are an invalid artefact, as is an installed release whose manifest no longer parses or no
    /// longer matches its schema: it is there, but not valid. A release of the job's own package that is not installed is
    /// not a dependency failure: it is a release the job names that is not there.
    fn try_from(error: &PackageError) -> Result<Self, NotADependencyFailure> {
        match error {
            PackageError::DependencyCycle { .. }
            | PackageError::TooManyDependencies { .. }
            | PackageError::DependencyTooDeep { .. }
            | PackageError::SnapshotOverLimit { .. }
            | PackageError::InstalledManifestInvalid { .. } => Ok(Self::ArtefactInvalid),
            PackageError::LibraryNotInstalled { .. }
            | PackageError::NoReleaseSatisfies { .. }
            | PackageError::RequirementExcludesChosen { .. }
            | PackageError::CatalogueUnreadable { .. } => Ok(Self::ArtefactMissing),
            _ => Err(NotADependencyFailure),
        }
    }
}

impl ValidationCode {
    /// The stable machine-readable code.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ArtefactMissing => "validation/artefact-missing",
            Self::ArtefactInvalid => "validation/artefact-invalid",
            Self::ScriptSyntax => "validation/script-syntax",
            Self::InputInvalid => "validation/input-invalid",
            Self::ServerNotConfigured => "validation/server-not-configured",
            Self::OwnerUnresolved => "validation/owner-unresolved",
            Self::ToolNotFound => "validation/tool-not-found",
            Self::EntityUnresolved => "validation/entity-unresolved",
        }
    }
}

/// One thing wrong with a job.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidationFailure {
    pub code: ValidationCode,
    /// The file or part of the job it concerns, for example the entry script's name.
    pub artefact: String,
    pub message: String,
}

/// A check that could not be made, because something it needs could not be reached.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Unverifiable {
    pub check: String,
    pub reason: String,
}

/// Everything static validation found.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ValidationReport {
    pub failures: Vec<ValidationFailure>,
    pub unverifiable: Vec<Unverifiable>,
}

impl ValidationReport {
    /// Whether the job passed: nothing failed, and nothing could not be checked.
    #[must_use]
    pub fn passed(&self) -> bool {
        self.failures.is_empty() && self.unverifiable.is_empty()
    }
}

/// A script or module that does not compile, or imports what it cannot, with where.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScriptSyntaxError {
    /// The module the error is in, relative to its workflow's root, or nothing for the entry script.
    pub module: Option<WorkflowFilePath>,
    /// Whose module that is, so a library's file is not taken for one of the job's own with the same path. `None` for
    /// the entry script.
    pub owner: Option<ModuleOwner>,
    pub message: String,
    /// The line, counted from 1, when the error has a position.
    pub line: Option<usize>,
    /// The column within the line, counted from 1.
    pub position: Option<usize>,
}

/// What a non-literal import says.
const IMPORT_NOT_LITERAL: &str = "an import names its module with a string literal, such as `import \"lib/dates\" as \
     dates;`, so the module can be checked before the script runs";

/// Compiles the entry script and every module and checks every import they make, without running anything, and
/// reports the first problem in each file: a syntax error, an import that does not name its module with a string
/// literal or names one that is neither declared nor built in, a statement in a module other than a function or an
/// import, or modules that import each other. No host function is registered, because a parse does not need one. The
/// checks run on the same kind of thread a run does, so a script nested to the limits is checked without exhausting
/// the caller's stack.
#[must_use]
pub fn check_script(script: &WorkflowScript) -> Vec<ScriptSyntaxError> {
    let owned = script.clone();
    on_script_thread(move || check_on_this_thread(&owned))
        .map_err(|error| error.to_string())
        .and_then(|thread| {
            thread
                .join()
                .map_err(|_panic| "the script could not be checked".to_owned())
        })
        .unwrap_or_else(|message| {
            vec![ScriptSyntaxError {
                module: None,
                owner: None,
                message,
                line: None,
                position: None,
            }]
        })
}

fn check_on_this_thread(script: &WorkflowScript) -> Vec<ScriptSyntaxError> {
    let engine = script_engine();
    let mut errors = Vec::new();
    match engine.compile(script.entry()) {
        Err(error) => errors.push(syntax_error(None, &error)),
        Ok(ast) => match import_inside_a_function(&ast) {
            Some(position) => errors.push(error_at(None, ENTRY_IMPORT_NOT_TOP_LEVEL, position)),
            None => errors.extend(first_bad_import(&ast, script, script.root(), None).err()),
        },
    }
    // The declared modules each imports, by import name, for finding modules that import each other.
    let mut imports_of = ImportsOf::new();
    for module in script.modules() {
        match check_module(&engine, script, module) {
            Ok(imports) => {
                imports_of.insert(module.key(), (module.path().clone(), imports));
            }
            Err(mut error) => {
                error.owner = Some(module.owner().clone());
                errors.push(error);
            }
        }
    }
    errors.extend(first_cycle(&imports_of));
    errors
}

/// Compiles one declared module and checks it holds only functions and top-level imports, and what it imports.
fn check_module(
    engine: &rhai::Engine,
    script: &WorkflowScript,
    module: &ModuleSource,
) -> Result<Vec<(ModuleKey, Position)>, ScriptSyntaxError> {
    let path = Some(module.path().clone());
    let ast = engine
        .compile(module.text())
        .map_err(|error| syntax_error(path.clone(), &error))?;
    if let Some(statement) = ast
        .statements()
        .iter()
        .find(|statement| !is_module_statement(statement))
    {
        return Err(error_at(path, MODULE_HOLDS_CODE, statement.position()));
    }
    if let Some(position) = import_inside_a_function(&ast) {
        return Err(error_at(path, MODULE_IMPORT_NOT_TOP_LEVEL, position));
    }
    first_bad_import(&ast, script, module.owner(), path.as_ref())
}

/// Every import `ast` makes of a module the script carries, with where, or the first that names no module, names one by
/// anything but a string literal, or names one that a file of `importer` may not import.
fn first_bad_import(
    ast: &AST,
    script: &WorkflowScript,
    importer: &ModuleOwner,
    module: Option<&WorkflowFilePath>,
) -> Result<Vec<(ModuleKey, Position)>, ScriptSyntaxError> {
    let mut imports = Vec::new();
    let mut bad = None;
    ast.walk(&mut |nodes: &[ASTNode]| {
        let Some(ASTNode::Stmt(Stmt::Import(import, position))) = nodes.last() else {
            return true;
        };
        let Expr::StringConstant(name, _) = &import.0 else {
            bad = Some(error_at(module.cloned(), IMPORT_NOT_LITERAL, *position));
            return false;
        };
        match script.import_target(importer, name) {
            Err(error) => {
                bad = Some(error_at(module.cloned(), &error.to_string(), *position));
                false
            }
            Ok(ImportTarget::BuiltIn) => true,
            Ok(ImportTarget::Module(key)) if script.module(&key).is_some() => {
                imports.push((key, *position));
                true
            }
            Ok(ImportTarget::Module(_) | ImportTarget::Nothing) => {
                bad = Some(error_at(module.cloned(), &undeclared(name), *position));
                false
            }
        }
    });
    bad.map_or(Ok(imports), Err)
}

/// Each module's file and the modules it imports, with where, by key.
type ImportsOf = BTreeMap<ModuleKey, (WorkflowFilePath, Vec<(ModuleKey, Position)>)>;

/// The import that closes the first loop of modules importing each other, reported in the module that makes it.
fn first_cycle(imports_of: &ImportsOf) -> Option<ScriptSyntaxError> {
    // Depth first from each module in turn: an import of a module still on the path closes a loop.
    fn visit<'a>(
        key: &'a ModuleKey,
        imports_of: &'a ImportsOf,
        on_path: &mut BTreeSet<&'a ModuleKey>,
        finished: &mut BTreeSet<&'a ModuleKey>,
    ) -> Option<ScriptSyntaxError> {
        let (path, imports) = imports_of.get(key)?;
        on_path.insert(key);
        for (imported, position) in imports {
            if on_path.contains(imported) {
                let mut error = error_at(Some(path.clone()), &cyclic(&imported.to_string()), *position);
                error.owner = Some(key.owner.clone());
                return Some(error);
            }
            if !finished.contains(imported)
                && let Some(error) = visit(imported, imports_of, on_path, finished)
            {
                return Some(error);
            }
        }
        on_path.remove(key);
        finished.insert(key);
        None
    }
    let mut finished = BTreeSet::new();
    imports_of.keys().find_map(|key| {
        if finished.contains(key) {
            return None;
        }
        visit(key, imports_of, &mut BTreeSet::new(), &mut finished)
    })
}

fn syntax_error(module: Option<WorkflowFilePath>, error: &ParseError) -> ScriptSyntaxError {
    let position = error.position();
    let message = refusal(&error.0).map_or_else(|| error.to_string(), |why| format!("{why} ({position})"));
    ScriptSyntaxError {
        module,
        owner: None,
        message,
        line: position.line(),
        position: position.position(),
    }
}

fn error_at(module: Option<WorkflowFilePath>, why: &str, position: Position) -> ScriptSyntaxError {
    ScriptSyntaxError {
        module,
        owner: None,
        message: format!("{why} ({position})"),
        line: position.line(),
        position: position.position(),
    }
}

#[cfg(test)]
#[path = "validation_test.rs"]
mod tests;
