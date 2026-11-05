//! The `Record conformance` doctor check: records that differ from their series template, base properties that do not
//! name a base selecting the note, and sidecars whose template cannot be used. Read-only, and never repaired by the
//! server: which value is right, or whether a note belongs elsewhere, is the operator's decision.
//!
//! Entries name paths, fields, series, and template paths only. A value from a note or a template is never shown.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use contextos_core::{VaultRoot, VaultSet};
use contextos_fs::Filesystem;
use contextos_obsidian::{BaseKeyScheme, BaseLinkTarget, BaseSidecar};
use serde_json::{Map, Value};

use super::{DoctorCheck, DoctorError};
use crate::doctor_bases::{RecordNote, Resolved, VaultBase, VaultBases, load_record_notes};
use crate::doctor_kind::DoctorKind;
use crate::tools::base_record_template::{
    EVERY_SERIES, ResolvedTemplate, TemplateProblem, TemplateReader, TemplateSpec, TemplateVault,
};

const MAX_REPORTED: usize = 10;

const ACTION: &str = "Correct each listed note's fields to its series template, or move it to the series or base it \
                      belongs to; correct each listed base property, for example to the one base named; and correct \
                      each listed sidecar's template declaration. Then rerun the doctor.";

/// One finding's entries, each led by the path it is sorted under.
type Entries = Vec<(String, String)>;

/// Why `stored`, the `base` property of the note at `note`, does not name a base that selects it; `None` when it does.
fn base_problem(note: &str, stored: &Value, bases: &VaultBases) -> Option<String> {
    let Some(target) = stored.as_str().and_then(|text| BaseLinkTarget::try_from(text).ok()) else {
        return Some(format!("{note}: base is not a link to a .base file"));
    };
    let link = target.as_str();
    let base = match bases.resolve(note, link) {
        Resolved::Found(base) if base.is_readable() => base,
        Resolved::Found(base) => {
            return Some(format!(
                "{note}: base names {link}, whose records cannot be read: {}",
                base.unreadable_reason().unwrap_or("unknown reason")
            ));
        }
        Resolved::Missing => {
            return Some(format!("{note}: base names {link}, which cannot be read as a base"));
        }
        Resolved::Ambiguous => {
            return Some(format!(
                "{note}: base names {link}, which more than one base file matches"
            ));
        }
    };
    if base.selects(note) {
        return None;
    }
    let hint = match bases.selecting(note).as_slice() {
        [] => "no base selects it".to_owned(),
        [only] => format!("{} is the one base that selects it", only.path),
        several => format!("{} bases select it", several.len()),
    };
    Some(format!(
        "{note}: outside the records that {} selects; {hint}",
        base.path
    ))
}

/// The series whose key `id` is a record's: the declared prefix `id` is `<prefix>-<digits>` for.
fn series_of<'a>(sidecar: &'a BaseSidecar, id: &str) -> Option<&'a str> {
    sidecar
        .series()
        .iter()
        .map(contextos_obsidian::KeySeries::prefix)
        .find(|prefix| {
            id.strip_prefix(prefix)
                .and_then(|rest| rest.strip_prefix('-'))
                .is_some_and(|digits| !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit()))
        })
}

/// The templates one sidecar declares, read once each, with what is wrong with the ones that cannot be used.
struct SidecarTemplates {
    /// By the link as written.
    resolved: BTreeMap<String, Result<ResolvedTemplate, TemplateProblem>>,
}

impl SidecarTemplates {
    fn read(base: &VaultBase, sidecar: &BaseSidecar, reader: &TemplateReader<'_>) -> Self {
        let declaration = sidecar.series_templates();
        let sidecar_path = base.sidecar_path();
        let links = declaration
            .per_series_templates()
            .iter()
            .map(|(_, link)| link)
            .chain(declaration.template_for(None));
        let resolved = links
            .map(|link| {
                let read = reader.read(TemplateSpec {
                    sidecar: &sidecar_path,
                    link: link.as_ref(),
                    fixed_fields: declaration.fixed_fields(),
                });
                (link.as_ref().to_owned(), read)
            })
            .collect();
        Self { resolved }
    }

    /// The template the series (or `None` for a `FILEPATH` base) follows, when it has one.
    fn for_series<'a>(
        &'a self,
        sidecar: &BaseSidecar,
        series: Option<&str>,
    ) -> Option<&'a Result<ResolvedTemplate, TemplateProblem>> {
        let link = sidecar.series_templates().template_for(series)?;
        self.resolved.get(link.as_ref())
    }
}

/// What is wrong with one sidecar's template declaration, as one entry; `None` when nothing is.
fn sidecar_problem(base: &VaultBase, sidecar: &BaseSidecar, templates: &SidecarTemplates) -> Option<String> {
    let declaration = sidecar.series_templates();
    let mut problems = Vec::new();
    let series: Vec<Option<&str>> = match sidecar.scheme() {
        BaseKeyScheme::Sequential { series, .. } => series.iter().map(|series| Some(series.prefix())).collect(),
        BaseKeyScheme::FilePath => vec![None],
    };
    if declaration.lists_several_templates() {
        let without: Vec<&str> = series
            .iter()
            .filter(|series| declaration.template_for(**series).is_none())
            .map(|series| series.unwrap_or(EVERY_SERIES))
            .collect();
        if !without.is_empty() {
            problems.push(format!(
                "template lists several links, so {} {} no template of {} own",
                without.join(", "),
                if without.len() == 1 { "has" } else { "have" },
                if without.len() == 1 { "its" } else { "their" },
            ));
        }
    }
    let mut seen: Vec<&str> = Vec::new();
    for series in series {
        let Some(link) = declaration.template_for(series) else {
            continue;
        };
        if seen.contains(&link.as_ref()) {
            continue;
        }
        seen.push(link.as_ref());
        if let Some(Err(problem)) = templates.for_series(sidecar, series) {
            let own = series.is_some_and(|series| {
                declaration
                    .per_series_templates()
                    .iter()
                    .any(|(declared, _)| declared == series)
            });
            let label = match series {
                Some(series) if own => format!("series {series}"),
                _ => EVERY_SERIES.to_owned(),
            };
            problems.push(format!(
                "{label} follows {}, which cannot be used: {problem}",
                link.as_ref()
            ));
        }
    }
    (!problems.is_empty()).then(|| format!("{}: {}", base.sidecar_path(), problems.join("; ")))
}

/// The base that governs `note` for its template: the base its `base` property names, when that base selects it; else,
/// with no `base` property, the one base selecting it that declares a template.
fn governing_base<'a>(note: &RecordNote, bases: &'a VaultBases) -> Option<&'a VaultBase> {
    if let Some(stored) = &note.base {
        let target = stored.as_str().and_then(|text| BaseLinkTarget::try_from(text).ok())?;
        return match bases.resolve(&note.path, target.as_str()) {
            Resolved::Found(base) if base.selects(&note.path) => Some(base),
            _ => None,
        };
    }
    let mut templated = bases.selecting(&note.path).into_iter().filter(|base| {
        base.sidecar
            .as_ref()
            .is_some_and(|sidecar| sidecar.series_templates().declares_any())
    });
    let only = templated.next()?;
    templated.next().is_none().then_some(only)
}

/// The fixed fields of `template` that `frontmatter` lacks or holds a different value for.
fn differing_fields(template: &ResolvedTemplate, frontmatter: &Map<String, Value>) -> Vec<String> {
    template
        .fixed
        .fields()
        .iter()
        .filter(|(name, value)| frontmatter.get(name.as_str()) != Some(*value))
        .map(|(name, _)| name.clone())
        .collect()
}

/// Lists at most [`MAX_REPORTED`] entries in path order, followed by how many more there are.
fn listed(mut entries: Entries) -> String {
    entries.sort();
    let mut text = entries
        .iter()
        .take(MAX_REPORTED)
        .map(|(_, entry)| entry.as_str())
        .collect::<Vec<_>>()
        .join("; ");
    if entries.len() > MAX_REPORTED {
        let _ = write!(text, ", and {} more", entries.len() - MAX_REPORTED);
    }
    text
}

/// What the check found, before it is worded.
#[derive(Default)]
struct Findings {
    /// Records checked against a template, and those that differ.
    record_checked: usize,
    records: Entries,
    /// Notes that name a base, and those whose base does not select them.
    base_checked: usize,
    bases: Entries,
    /// Sidecars whose template declaration cannot be used.
    sidecars: Entries,
}

/// The templates of every sidecar that declares any, by the path of its base, with a finding for each that cannot be
/// used.
fn read_templates(
    bases: &VaultBases,
    reader: &TemplateReader<'_>,
    findings: &mut Findings,
) -> BTreeMap<String, SidecarTemplates> {
    let mut templates = BTreeMap::new();
    for base in bases.iter() {
        let Some(sidecar) = &base.sidecar else {
            continue;
        };
        if !sidecar.series_templates().declares_any() && !sidecar.series_templates().lists_several_templates() {
            continue;
        }
        let read = SidecarTemplates::read(base, sidecar, reader);
        if let Some(entry) = sidecar_problem(base, sidecar, &read) {
            findings.sidecars.push((base.sidecar_path(), entry));
        }
        templates.insert(base.path.clone(), read);
    }
    templates
}

/// Checks one record against its governing base's template for its series, when it has one that can be used.
fn check_record(
    note: &RecordNote,
    bases: &VaultBases,
    templates: &BTreeMap<String, SidecarTemplates>,
    findings: &mut Findings,
) {
    let Some(base) = governing_base(note, bases) else {
        return;
    };
    let Some(sidecar) = &base.sidecar else {
        return;
    };
    let series = match sidecar.scheme() {
        BaseKeyScheme::FilePath => None,
        BaseKeyScheme::Sequential { .. } => {
            let Some(prefix) = note
                .frontmatter
                .get("id")
                .and_then(Value::as_str)
                .and_then(|id| series_of(sidecar, id))
            else {
                return;
            };
            Some(prefix)
        }
    };
    let Some(Ok(template)) = templates
        .get(&base.path)
        .and_then(|templates| templates.for_series(sidecar, series))
    else {
        return;
    };
    findings.record_checked += 1;
    let fields = differing_fields(template, &note.frontmatter);
    if !fields.is_empty() {
        findings.records.push((
            note.path.clone(),
            format!(
                "{}: {} differ from the template of {} ({})",
                note.path,
                fields.join(", "),
                series.map_or_else(|| EVERY_SERIES.to_owned(), |series| format!("series {series}")),
                template.path
            ),
        ));
    }
}

impl From<Findings> for DoctorCheck {
    fn from(findings: Findings) -> Self {
        let Findings {
            record_checked,
            records,
            base_checked,
            bases,
            sidecars,
        } = findings;
        if records.is_empty() && bases.is_empty() && sidecars.is_empty() {
            return Self::pass(
                DoctorKind::RecordConformance,
                format!(
                    "{record_checked} records match their series template, and {base_checked} notes name a base that selects them"
                ),
            );
        }
        let mut parts = Vec::new();
        if !records.is_empty() {
            parts.push(format!(
                "{} of {record_checked} records differ from their series template: {}",
                records.len(),
                listed(records)
            ));
        }
        if !bases.is_empty() {
            parts.push(format!(
                "{} of {base_checked} notes name a base that does not select them: {}",
                bases.len(),
                listed(bases)
            ));
        }
        if !sidecars.is_empty() {
            parts.push(format!(
                "{} sidecars declare a template that cannot be used: {}",
                sidecars.len(),
                listed(sidecars)
            ));
        }
        Self::fail(DoctorKind::RecordConformance, parts.join("; "), ACTION)
    }
}

pub(super) fn record_conformance_check(
    root: &VaultRoot,
    roots: &VaultSet,
    filesystem: &Filesystem,
) -> Result<DoctorCheck, DoctorError> {
    let bases = VaultBases::load(root, roots, filesystem)?;
    let reader = TemplateReader::try_from(TemplateVault {
        filesystem,
        roots,
        name: root.name(),
    })?;
    let mut findings = Findings::default();
    let templates = read_templates(&bases, &reader, &mut findings);
    for note in load_record_notes(root, roots, filesystem)? {
        if bases.is_sidecar(&note.path) {
            continue;
        }
        if let Some(stored) = &note.base {
            findings.base_checked += 1;
            if let Some(problem) = base_problem(&note.path, stored, &bases) {
                findings.bases.push((note.path.clone(), problem));
                continue;
            }
        }
        check_record(&note, &bases, &templates, &mut findings);
    }
    Ok(findings.into())
}

#[cfg(test)]
#[path = "doctor_record_conformance_test.rs"]
mod tests;

#[cfg(test)]
#[path = "doctor_record_conformance_base_test.rs"]
mod base_tests;
