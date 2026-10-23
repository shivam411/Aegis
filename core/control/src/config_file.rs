//! Configuration as code: the `aegis.toml` a deployment would use, so the
//! dashboard can show what a settings change does to it before saving, and
//! operators can copy the result back into their repository.

use crate::{validate_settings, ControlError, ControlPlane, ControlResult};
use aegis_config::{ProjectFile, ProjectOverrides};
use aegis_projection::ProjectState;
use aegis_types::ProjectId;
use serde::Serialize;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DiffLine {
    /// " " unchanged, "-" removed, "+" added.
    pub op: &'static str,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ConfigPreview {
    /// Where the base file comes from: "source" (the project's directory),
    /// "release" (the live release) or "none" (no aegis.toml yet).
    pub base: &'static str,
    /// aegis.toml with the current settings applied.
    pub current: String,
    /// aegis.toml with the proposed settings applied.
    pub proposed: String,
    pub diff: Vec<DiffLine>,
    pub changed: bool,
}

const HEADER: &str = "# Settings from the Aegis dashboard were merged into this file.\n";

fn render(file: &ProjectFile) -> ControlResult<String> {
    let body = toml::to_string_pretty(file).map_err(anyhow::Error::from)?;
    Ok(format!("{}{}", HEADER, body))
}

/// A line diff (longest common subsequence); fine for files this size.
pub(crate) fn diff_lines(old: &str, new: &str) -> Vec<DiffLine> {
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();
    let mut lcs = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            lcs[i][j] = if a[i] == b[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let line = |op, text: &str| DiffLine {
        op,
        text: text.to_string(),
    };
    let (mut i, mut j, mut out) = (0, 0, Vec::new());
    while i < a.len() && j < b.len() {
        if a[i] == b[j] {
            out.push(line(" ", a[i]));
            i += 1;
            j += 1;
        } else if lcs[i + 1][j] >= lcs[i][j + 1] {
            out.push(line("-", a[i]));
            i += 1;
        } else {
            out.push(line("+", b[j]));
            j += 1;
        }
    }
    out.extend(a[i..].iter().map(|t| line("-", t)));
    out.extend(b[j..].iter().map(|t| line("+", t)));
    out
}

impl ControlPlane {
    /// The aegis.toml the dashboard settings are merged into: the project's
    /// own file when deploying from a directory, else the live release's.
    pub(crate) fn base_project_file(
        &self,
        project: &ProjectState,
    ) -> ControlResult<(&'static str, ProjectFile)> {
        let invalid = |e: anyhow::Error| {
            ControlError::FailedPrecondition(format!("The project's aegis.toml is invalid: {}", e))
        };
        if let Some(dir) = &project.source_dir {
            if let Some(file) = ProjectFile::load_from_dir(&PathBuf::from(dir)).map_err(invalid)? {
                return Ok(("source", file));
            }
        }
        if let Some(version) = &project.current_release {
            let dir = self.switcher(project.id).get_version_dir(version);
            if let Some(file) = ProjectFile::load_from_dir(&dir).map_err(invalid)? {
                return Ok(("release", file));
            }
        }
        let mut file = ProjectFile::default();
        file.project.id = Some(project.id.to_string());
        file.project.name = Some(project.name.clone());
        Ok(("none", file))
    }

    /// Shows how `proposed` settings would change the project's aegis.toml.
    pub fn preview_settings(
        &self,
        project_id: ProjectId,
        proposed: ProjectOverrides,
    ) -> ControlResult<ConfigPreview> {
        let project = self
            .inner
            .projection
            .get_project(&project_id)
            .ok_or_else(|| ControlError::NotFound(format!("Unknown project {}", project_id)))?;
        let proposed = validate_settings(proposed)?;
        let (base, file) = self.base_project_file(&project)?;
        let with = |settings: &ProjectOverrides| {
            let mut f = file.clone();
            settings.apply_to(&mut f);
            render(&f)
        };
        let current = with(&project.settings)?;
        let proposed = with(&proposed)?;
        let diff = diff_lines(&current, &proposed);
        Ok(ConfigPreview {
            base,
            changed: current != proposed,
            current,
            proposed,
            diff,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_marks_changed_lines() {
        let ops = |d: Vec<DiffLine>| {
            d.into_iter()
                .map(|l| format!("{}{}", l.op, l.text))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            ops(diff_lines("a\nb\nc\n", "a\nx\nc\nd\n")),
            vec![" a", "-b", "+x", " c", "+d"]
        );
        assert_eq!(ops(diff_lines("", "a")), vec!["+a"]);
        assert_eq!(ops(diff_lines("a", "")), vec!["-a"]);
        assert!(diff_lines("same\n", "same\n").iter().all(|l| l.op == " "));
    }
}
