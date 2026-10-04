//! The agent skills that ship with ox, embedded so a binary install can set them up:
//! `ox skill install` copies them into `~/.claude/skills`.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

pub const SKILLS: &[(&str, &str, &str)] = &[
    ("ox", "how to run ox and read its output (start here)", include_str!("../skills/ox/SKILL.md")),
    (
        "ox-ibkr",
        "sync, verify and stage trades with the Interactive Brokers connector",
        include_str!("../skills/ox-ibkr/SKILL.md"),
    ),
];

pub fn get(name: &str) -> Result<&'static str> {
    SKILLS.iter().find(|(n, _, _)| *n == name).map(|(_, _, body)| *body).with_context(|| {
        format!(
            "no skill named {name}; available: {}",
            SKILLS.iter().map(|(n, _, _)| *n).collect::<Vec<_>>().join(", ")
        )
    })
}

pub fn default_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")).join(".claude").join("skills")
}

/// Write `<dir>/<name>/SKILL.md` for each name. An existing, different file is only replaced
/// with `force`; returns the paths written.
pub fn install(names: &[String], dir: &Path, force: bool) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for name in names {
        let body = get(name)?;
        let path = dir.join(name).join("SKILL.md");
        if let Ok(existing) = std::fs::read_to_string(&path) {
            if existing == body {
                continue;
            }
            if !force {
                bail!("{} exists and differs from this version; pass --force to replace it", path.display());
            }
        }
        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::write(&path, body).with_context(|| format!("writing {}", path.display()))?;
        out.push(path);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skills_have_matching_frontmatter_names() {
        for (name, _, body) in SKILLS {
            assert!(body.starts_with("---\n"), "{name}");
            assert!(body.contains(&format!("\nname: {name}\n")), "{name}");
        }
    }

    #[test]
    fn install_is_idempotent_and_guards_edits() {
        let dir = std::env::temp_dir().join(format!("ox-skill-test-{}", std::process::id()));
        let names = vec!["ox".to_string()];
        assert_eq!(install(&names, &dir, false).unwrap().len(), 1);
        assert!(install(&names, &dir, false).unwrap().is_empty());
        std::fs::write(dir.join("ox/SKILL.md"), "edited").unwrap();
        assert!(install(&names, &dir, false).is_err());
        assert_eq!(install(&names, &dir, true).unwrap().len(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
