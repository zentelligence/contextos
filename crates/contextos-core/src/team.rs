//! `.contextos/team.toml` as a job reads it: the members a task or a job can name, and the one chief of staff.
//!
//! [`TeamFile`] is the file's schema, shared by every crate that reads it, as [`crate::EntitiesFile`] is for the
//! entities. [`TeamRoster`] is built only from a file that satisfies the rules a job depends on: slugs in the slug
//! form, unique slugs, and exactly one member who is the chief of staff. A screen that merely lists the team stays
//! lenient and does not use it.

use std::fmt;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Where a vault keeps its team, relative to the vault root.
pub const TEAM_PATH: &str = ".contextos/team.toml";

/// Whether a member is an AI agent or a person.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum MemberKind {
    #[default]
    Agent,
    Human,
}

/// `.contextos/team.toml`'s contents as far as a job depends on them. Keys a screen reads and a job does not are left
/// to that screen's own schema.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct TeamFile {
    #[serde(default)]
    pub member: Vec<TeamMemberRecord>,
}

/// One `[[member]]` entry as it is written.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct TeamMemberRecord {
    pub slug: String,
    #[serde(default)]
    pub name: String,
    #[serde(default, rename = "type")]
    pub kind: MemberKind,
    #[serde(default)]
    pub chief_of_staff: bool,
}

/// A member's slug: lowercase ASCII letters, digits, and hyphens, never empty. Matched exactly, never by display name
/// or without regard to case.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct MemberSlug(String);

impl MemberSlug {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for MemberSlug {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// A string that is not a member slug.
#[derive(Debug, Error, Eq, PartialEq)]
#[error("a member slug is lowercase letters, digits, and hyphens, and is not empty")]
pub struct InvalidMemberSlug;

impl TryFrom<&str> for MemberSlug {
    type Error = InvalidMemberSlug;

    fn try_from(text: &str) -> Result<Self, InvalidMemberSlug> {
        let well_formed = !text.is_empty()
            && text
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
        if well_formed {
            Ok(Self(text.to_owned()))
        } else {
            Err(InvalidMemberSlug)
        }
    }
}

/// One member of a valid team.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TeamMember {
    pub slug: MemberSlug,
    pub name: String,
    pub kind: MemberKind,
    pub chief_of_staff: bool,
}

/// The rule a team broke. Names the rule and never a member, so it is safe to show and to log.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TeamRule {
    /// A slug is empty, has capitals, or has characters other than letters, digits, and hyphens.
    InvalidSlug,
    /// Two members share a slug.
    DuplicateSlug,
    /// The team has no members at all.
    Empty,
    /// No member is the chief of staff.
    NoChiefOfStaff,
    /// More than one member is the chief of staff.
    SeveralChiefsOfStaff,
    /// The vault has no `team.toml` at all.
    FileAbsent,
    /// `team.toml` exists and is not text.
    FileNotText,
    /// `team.toml` is text that does not parse as a team.
    FileUnparsable,
}

impl TeamRule {
    /// A stable word for the rule.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidSlug => "invalid-slug",
            Self::DuplicateSlug => "duplicate-slug",
            Self::Empty => "team-empty",
            Self::NoChiefOfStaff => "no-chief-of-staff",
            Self::SeveralChiefsOfStaff => "several-chiefs-of-staff",
            Self::FileAbsent => "file-absent",
            Self::FileNotText => "file-not-text",
            Self::FileUnparsable => "file-unparsable",
        }
    }
}

/// A team that breaks a rule a job depends on.
#[derive(Debug, Error, Eq, PartialEq)]
#[error("the team is invalid: {}", .rule.as_str())]
pub struct TeamRosterError {
    rule: TeamRule,
}

impl TeamRosterError {
    /// The stable machine-readable code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        "workflow/team-invalid"
    }

    /// Which rule was broken.
    #[must_use]
    pub const fn rule(&self) -> TeamRule {
        self.rule
    }
}

impl From<TeamRule> for TeamRosterError {
    fn from(rule: TeamRule) -> Self {
        Self { rule }
    }
}

/// A team that satisfies every rule a job depends on, so the chief of staff always exists.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TeamRoster {
    members: Vec<TeamMember>,
    chief_of_staff: TeamMember,
}

impl TryFrom<TeamFile> for TeamRoster {
    type Error = TeamRosterError;

    fn try_from(file: TeamFile) -> Result<Self, TeamRosterError> {
        let mut members: Vec<TeamMember> = Vec::with_capacity(file.member.len());
        for record in file.member {
            let slug = MemberSlug::try_from(record.slug.as_str()).map_err(|_| TeamRule::InvalidSlug)?;
            if members.iter().any(|member| member.slug == slug) {
                return Err(TeamRule::DuplicateSlug.into());
            }
            members.push(TeamMember {
                slug,
                name: record.name,
                kind: record.kind,
                chief_of_staff: record.chief_of_staff,
            });
        }
        if members.is_empty() {
            return Err(TeamRule::Empty.into());
        }
        let mut chiefs = members.iter().filter(|member| member.chief_of_staff);
        let chief_of_staff = match (chiefs.next(), chiefs.next()) {
            (Some(chief), None) => chief.clone(),
            (Some(_), Some(_)) => return Err(TeamRule::SeveralChiefsOfStaff.into()),
            (None, _) => return Err(TeamRule::NoChiefOfStaff.into()),
        };
        Ok(Self {
            members,
            chief_of_staff,
        })
    }
}

impl TeamRoster {
    /// The member with exactly this slug.
    #[must_use]
    pub fn member(&self, slug: &MemberSlug) -> Option<&TeamMember> {
        self.members.iter().find(|member| &member.slug == slug)
    }

    /// The one member who is the chief of staff.
    #[must_use]
    pub const fn chief_of_staff(&self) -> &TeamMember {
        &self.chief_of_staff
    }

    /// Every member, in the order the file lists them.
    #[must_use]
    pub fn members(&self) -> &[TeamMember] {
        &self.members
    }
}

/// How a job's declared owner stands against the team.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OwnerStanding<'a> {
    /// The owner is a member.
    Member(&'a TeamMember),
    /// An owner is declared and is not a member.
    NotAMember(MemberSlug),
    /// The job declares an owner whose text is not a member slug at all (capitals, spaces, or other characters), so it
    /// can match no member. The declared text is kept as written, never lowercased or repaired.
    InvalidSlug(String),
    /// The job declares no owner.
    Undeclared,
}

/// The owner a job file declares, as written: a slug when the text has the slug form, otherwise the text itself, so a
/// job with a mistyped owner still loads and can say what it declared.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DeclaredOwner {
    Slug(MemberSlug),
    Invalid(String),
}

impl DeclaredOwner {
    /// The declared text exactly as written.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Slug(slug) => slug.as_str(),
            Self::Invalid(text) => text,
        }
    }
}

impl From<&str> for DeclaredOwner {
    fn from(text: &str) -> Self {
        MemberSlug::try_from(text).map_or_else(|_| Self::Invalid(text.to_owned()), Self::Slug)
    }
}

impl<'a> From<(Option<&DeclaredOwner>, &'a TeamRoster)> for OwnerStanding<'a> {
    /// Decides how a declared owner stands: only an exact slug matches a member, and nothing is lowercased or
    /// repaired.
    fn from((declared, roster): (Option<&DeclaredOwner>, &'a TeamRoster)) -> Self {
        match declared {
            Some(DeclaredOwner::Invalid(text)) => Self::InvalidSlug(text.clone()),
            Some(DeclaredOwner::Slug(slug)) => roster
                .member(slug)
                .map_or_else(|| Self::NotAMember(slug.clone()), Self::Member),
            None => Self::Undeclared,
        }
    }
}

#[cfg(test)]
#[path = "team_test.rs"]
mod tests;
