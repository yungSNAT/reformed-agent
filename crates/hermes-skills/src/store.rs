//! Local skill storage: the `SkillStore` trait and `FileSkillStore` implementation.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::fs;
use tracing::{debug, instrument};

use hermes_core::types::{Skill, SkillMeta};

use crate::guard::SkillGuard;
use crate::skill::SkillError;

pub const MAX_SKILL_NAME_LENGTH: usize = 128;

// ---------------------------------------------------------------------------
// SkillStore trait
// ---------------------------------------------------------------------------

/// Abstraction over skill persistence backends.
#[async_trait]
pub trait SkillStore: Send + Sync {
    /// Persist a skill. Creates or overwrites.
    async fn save(&self, skill: &Skill) -> Result<(), SkillError>;

    /// Load a skill by name. Returns `None` if not found.
    async fn load(&self, name: &str) -> Result<Option<Skill>, SkillError>;

    /// List metadata for all stored skills.
    async fn list(&self) -> Result<Vec<SkillMeta>, SkillError>;

    /// Delete a skill by name. Succeeds even if the skill didn't exist.
    async fn delete(&self, name: &str) -> Result<(), SkillError>;
}

// ---------------------------------------------------------------------------
// YAML frontmatter
// ---------------------------------------------------------------------------

/// The frontmatter we write into / parse from `SKILL.md`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct SkillFrontmatter {
    name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    category: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    version: Option<String>,
}

// ---------------------------------------------------------------------------
// FileSkillStore
// ---------------------------------------------------------------------------

/// File-system backed skill store.
///
/// Skills are stored as `<skills_dir>/<category>/<name>/SKILL.md`.
/// Each file contains a YAML frontmatter block followed by the skill
/// content in Markdown.
pub struct FileSkillStore {
    skills_dir: PathBuf,
}

impl FileSkillStore {
    /// Create a new store rooted at `skills_dir`.
    pub fn new(skills_dir: PathBuf) -> Self {
        Self { skills_dir }
    }

    /// Return the default skills directory: `~/.hermes/skills/`.
    pub fn default_dir() -> PathBuf {
        directories::ProjectDirs::from("com", "hermes", "hermes")
            .map(|dirs| dirs.data_dir().join("skills"))
            .unwrap_or_else(|| PathBuf::from(".hermes/skills"))
    }

    /// Validate a skill path segment (`name` / `category`) to prevent path traversal.
    fn validate_segment(value: &str, field: &str) -> Result<String, SkillError> {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err(SkillError::GuardViolation(format!(
                "Invalid skill {field}: value must be non-empty"
            )));
        }
        if trimmed.len() > MAX_SKILL_NAME_LENGTH {
            return Err(SkillError::GuardViolation(format!(
                "Invalid skill {field}: value exceeds {MAX_SKILL_NAME_LENGTH} chars"
            )));
        }

        let path = Path::new(trimmed);
        let mut comps = path.components();
        let segment = match (comps.next(), comps.next()) {
            (Some(Component::Normal(seg)), None) => seg.to_string_lossy().to_string(),
            _ => {
                return Err(SkillError::GuardViolation(format!(
                    "Invalid skill {field}: path traversal or separators are not allowed"
                )))
            }
        };

        if segment.starts_with('.') {
            return Err(SkillError::GuardViolation(format!(
                "Invalid skill {field}: hidden path segments are not allowed"
            )));
        }
        if segment.starts_with('-') {
            return Err(SkillError::GuardViolation(format!(
                "Invalid skill {field}: leading '-' is not allowed"
            )));
        }
        if !segment
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.'))
        {
            return Err(SkillError::GuardViolation(format!(
                "Invalid skill {field}: use lowercase letters, digits, '-', '_' or '.'"
            )));
        }
        if segment.chars().any(|c| c.is_control()) {
            return Err(SkillError::GuardViolation(format!(
                "Invalid skill {field}: control characters are not allowed"
            )));
        }
        Ok(segment)
    }

    fn is_hidden_name(name: &str) -> bool {
        name.starts_with('.')
    }

    /// Compute the directory path for a given skill name and optional category.
    fn skill_dir(&self, name: &str, category: Option<&str>) -> Result<PathBuf, SkillError> {
        let safe_name = Self::validate_segment(name, "name")?;
        let safe_category = category
            .map(|c| Self::validate_segment(c, "category"))
            .transpose()?;
        Ok(match safe_category {
            Some(cat) => self.skills_dir.join(cat).join(safe_name),
            None => self.skills_dir.join(safe_name),
        })
    }

    async fn canonical_root(&self) -> Result<PathBuf, SkillError> {
        fs::create_dir_all(&self.skills_dir)
            .await
            .map_err(|e| SkillError::Io(e.to_string()))?;
        fs::canonicalize(&self.skills_dir)
            .await
            .map_err(|e| SkillError::Io(e.to_string()))
    }

    async fn ensure_path_within_root(&self, path: &Path) -> Result<(), SkillError> {
        let root = self.canonical_root().await?;

        // For non-existing targets, resolve the parent and append the filename.
        let resolved = match fs::canonicalize(path).await {
            Ok(p) => p,
            Err(_) => {
                let parent = path.parent().ok_or_else(|| {
                    SkillError::GuardViolation("Invalid skill path: missing parent".to_string())
                })?;
                if !parent.exists() {
                    // Parent doesn't exist yet; check the nearest guaranteed safe root.
                    let rel = path.strip_prefix(&self.skills_dir).map_err(|_| {
                        SkillError::GuardViolation(
                            "Invalid skill path: outside skills root".to_string(),
                        )
                    })?;
                    root.join(rel)
                } else {
                    let parent_real = fs::canonicalize(parent)
                        .await
                        .map_err(|e| SkillError::Io(e.to_string()))?;
                    if let Some(name) = path.file_name() {
                        parent_real.join(name)
                    } else {
                        parent_real
                    }
                }
            }
        };

        if !resolved.starts_with(&root) {
            return Err(SkillError::GuardViolation(format!(
                "Skill path escapes root boundary: {}",
                path.display()
            )));
        }
        Ok(())
    }

    /// Write a `SKILL.md` file with frontmatter + content.
    fn render_skill_file(fm: &SkillFrontmatter, content: &str) -> String {
        let yaml = serde_yaml::to_string(fm).unwrap_or_default();
        // serde_yaml adds a leading "---\n" we need to strip the first line
        // and add our own delimiters.
        let yaml_body = yaml.trim_start_matches("---\n").trim_end();
        format!("---\n{}\n---\n{}", yaml_body, content)
    }

    /// Parse a `SKILL.md` file, extracting frontmatter and body.
    fn parse_skill_file(raw: &str) -> Result<(SkillFrontmatter, String), SkillError> {
        // We expect the file to start with "---\n" and end the frontmatter
        // with another "---\n".
        if !raw.starts_with("---") {
            return Err(SkillError::Parse(
                "Skill file must start with YAML frontmatter".to_string(),
            ));
        }

        let rest = &raw[3..]; // skip first "---"
                              // Find the closing "---"
        let end = rest
            .find("\n---")
            .ok_or_else(|| SkillError::Parse("Missing closing --- in frontmatter".to_string()))?;

        let yaml_str = &rest[..end];
        let body_start = end + 4; // skip "\n---"
        let body = rest[body_start..].trim_start_matches('\n').to_string();

        let fm: SkillFrontmatter =
            serde_yaml::from_str(yaml_str).map_err(|e| SkillError::Parse(e.to_string()))?;

        Ok((fm, body))
    }
}

#[async_trait]
impl SkillStore for FileSkillStore {
    #[instrument(skip(self, skill), fields(name = %skill.name))]
    async fn save(&self, skill: &Skill) -> Result<(), SkillError> {
        let dir = self.skill_dir(&skill.name, skill.category.as_deref())?;
        self.ensure_path_within_root(&dir).await?;
        fs::create_dir_all(&dir)
            .await
            .map_err(|e| SkillError::Io(e.to_string()))?;
        self.ensure_path_within_root(&dir).await?;

        let fm = SkillFrontmatter {
            name: skill.name.clone(),
            category: skill.category.clone(),
            description: skill.description.clone(),
            version: Some(crate::version::compute_version(&skill.content)),
        };

        let content = Self::render_skill_file(&fm, &skill.content);
        let path = dir.join("SKILL.md");

        debug!("Writing skill file: {}", path.display());
        fs::write(&path, content)
            .await
            .map_err(|e| SkillError::Io(e.to_string()))?;

        Ok(())
    }

    #[instrument(skip(self), fields(name = %name))]
    async fn load(&self, name: &str) -> Result<Option<Skill>, SkillError> {
        // Search in all category subdirectories and the root.
        let candidates = self.candidate_dirs(name).await?;
        let root = self.canonical_root().await?;

        for dir in candidates {
            let path = dir.join("SKILL.md");
            if fs::try_exists(&path).await.unwrap_or(false) {
                let Some(real) = Self::contained_skill_file(&dir, &root).await? else {
                    continue;
                };
                let raw = fs::read_to_string(&real)
                    .await
                    .map_err(|e| SkillError::Io(e.to_string()))?;

                let (fm, content) = Self::parse_skill_file(&raw)?;
                let skill = Skill {
                    name: fm.name,
                    content,
                    category: fm.category,
                    description: fm.description,
                };
                // Mandatory pre-use security gate.
                SkillGuard::for_loaded_skills().scan_security_only(&skill)?;

                return Ok(Some(skill));
            }
        }

        Ok(None)
    }

    #[instrument(skip(self))]
    async fn list(&self) -> Result<Vec<SkillMeta>, SkillError> {
        let mut metas = Vec::new();

        // Ensure root exists.
        if !self.skills_dir.exists() {
            return Ok(metas);
        }

        self.collect_metas(&self.skills_dir, &mut metas).await?;
        Ok(metas)
    }

    #[instrument(skip(self), fields(name = %name))]
    async fn delete(&self, name: &str) -> Result<(), SkillError> {
        let candidates = self.candidate_dirs(name).await?;
        let root = self.canonical_root().await?;

        for dir in candidates {
            let path = dir.join("SKILL.md");
            if fs::try_exists(&path).await.unwrap_or(false) {
                let real = fs::canonicalize(&dir)
                    .await
                    .map_err(|e| SkillError::Io(e.to_string()))?;
                if !real.starts_with(&root) {
                    return Err(SkillError::GuardViolation(format!(
                        "Refusing to delete linked skill '{name}' outside the skills root: {}",
                        dir.display()
                    )));
                }
                // Remove the whole skill directory.
                fs::remove_dir_all(&dir)
                    .await
                    .map_err(|e| SkillError::Io(e.to_string()))?;
                return Ok(());
            }
        }

        // Idempotent: deleting a non-existent skill is fine.
        Ok(())
    }
}

impl FileSkillStore {
    /// The real path of `dir/SKILL.md` when it resolves inside its boundary:
    /// the link target when the skill folder `dir` is itself a symlink or
    /// junction (std reports a junction as a symlink), otherwise the
    /// canonical skills root. `None` when it escapes.
    async fn contained_skill_file(dir: &Path, root: &Path) -> Result<Option<PathBuf>, SkillError> {
        let is_link = fs::symlink_metadata(dir)
            .await
            .map_err(|e| SkillError::Io(e.to_string()))?
            .file_type()
            .is_symlink();
        let boundary = if is_link {
            fs::canonicalize(dir)
                .await
                .map_err(|e| SkillError::Io(e.to_string()))?
        } else {
            root.to_path_buf()
        };
        let real = fs::canonicalize(dir.join("SKILL.md"))
            .await
            .map_err(|e| SkillError::Io(e.to_string()))?;
        if !real.starts_with(&boundary) {
            tracing::warn!(
                "Skipping skill file outside its boundary: {}",
                dir.display()
            );
            return Ok(None);
        }
        Ok(Some(real))
    }

    /// Build a list of candidate directories where a skill named `name`
    /// might live (root + any category subdirectory).
    async fn candidate_dirs(&self, name: &str) -> Result<Vec<PathBuf>, SkillError> {
        let safe_name = Self::validate_segment(name, "name")?;
        let root = self.canonical_root().await?;
        let mut dirs = vec![self.skills_dir.join(&safe_name)];

        if self.skills_dir.exists() {
            let mut entries = fs::read_dir(&self.skills_dir)
                .await
                .map_err(|e| SkillError::Io(e.to_string()))?;

            while let Some(entry) = entries
                .next_entry()
                .await
                .map_err(|e| SkillError::Io(e.to_string()))?
            {
                let file_name = entry.file_name();
                let name = file_name.to_string_lossy();
                if Self::is_hidden_name(&name) {
                    continue;
                }
                let path = entry.path();
                let file_type = entry
                    .file_type()
                    .await
                    .map_err(|e| SkillError::Io(e.to_string()))?;
                if !file_type.is_dir() && !file_type.is_symlink() {
                    continue;
                }
                let canonical = fs::canonicalize(&path)
                    .await
                    .map_err(|e| SkillError::Io(e.to_string()))?;
                if !fs::metadata(&canonical)
                    .await
                    .map_err(|e| SkillError::Io(e.to_string()))?
                    .is_dir()
                {
                    continue;
                }
                if !canonical.starts_with(&root) {
                    tracing::warn!("Skipping category outside root: {}", path.display());
                    continue;
                }
                let candidate = path.join(&safe_name);
                dirs.push(candidate);
            }
        }

        Ok(dirs)
    }

    /// Recursively collect [`SkillMeta`] from all `SKILL.md` files under
    /// `dir`. The `relative` prefix is used to reconstruct categories.
    async fn collect_metas(
        &self,
        dir: &Path,
        metas: &mut Vec<SkillMeta>,
    ) -> Result<(), SkillError> {
        if !dir.exists() {
            return Ok(());
        }

        let root = self.canonical_root().await?;
        let mut stack = vec![dir.to_path_buf()];
        let mut visited = HashSet::new();

        while let Some(current_dir) = stack.pop() {
            let canonical_current = fs::canonicalize(&current_dir)
                .await
                .map_err(|e| SkillError::Io(e.to_string()))?;
            if !canonical_current.starts_with(&root) {
                tracing::warn!("Skipping directory outside root: {}", current_dir.display());
                continue;
            }
            if !visited.insert(canonical_current) {
                continue;
            }

            let mut entries = fs::read_dir(&current_dir)
                .await
                .map_err(|e| SkillError::Io(e.to_string()))?;

            while let Some(entry) = entries
                .next_entry()
                .await
                .map_err(|e| SkillError::Io(e.to_string()))?
            {
                let file_name = entry.file_name();
                let file_name = file_name.to_string_lossy();
                if Self::is_hidden_name(&file_name) {
                    continue;
                }

                let file_type = entry
                    .file_type()
                    .await
                    .map_err(|e| SkillError::Io(e.to_string()))?;
                if !file_type.is_dir() && !file_type.is_symlink() {
                    continue;
                }

                let path = entry.path();
                let canonical = fs::canonicalize(&path)
                    .await
                    .map_err(|e| SkillError::Io(e.to_string()))?;
                if !fs::metadata(&canonical)
                    .await
                    .map_err(|e| SkillError::Io(e.to_string()))?
                    .is_dir()
                {
                    continue;
                }
                let in_root = canonical.starts_with(&root);

                let skill_file = path.join("SKILL.md");
                if fs::try_exists(&skill_file).await.unwrap_or(false) {
                    let Some(real) = Self::contained_skill_file(&path, &root).await? else {
                        continue;
                    };
                    let raw = fs::read_to_string(&real)
                        .await
                        .map_err(|e| SkillError::Io(e.to_string()))?;

                    match Self::parse_skill_file(&raw) {
                        Ok((fm, _)) => {
                            // Derive category from the parent path relative to skills
                            // root; a linked skill outside the root takes it from the
                            // link's location.
                            let location = if in_root {
                                canonical.parent().and_then(|p| p.strip_prefix(&root).ok())
                            } else {
                                path.parent()
                                    .and_then(|p| p.strip_prefix(&self.skills_dir).ok())
                            };
                            let category = fm.category.or_else(|| {
                                location.and_then(|rel| {
                                    let s = rel.to_string_lossy().to_string();
                                    if s.is_empty() {
                                        None
                                    } else {
                                        Some(s)
                                    }
                                })
                            });

                            metas.push(SkillMeta {
                                name: fm.name,
                                category,
                                description: fm.description,
                            });
                        }
                        Err(e) => {
                            tracing::warn!("Failed to parse {}: {}", skill_file.display(), e);
                        }
                    }
                } else if in_root {
                    stack.push(path);
                } else {
                    tracing::warn!("Skipping directory outside root: {}", path.display());
                }
            }
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_render_and_parse_frontmatter() {
        let fm = SkillFrontmatter {
            name: "my-skill".to_string(),
            category: Some("general".to_string()),
            description: Some("A test skill".to_string()),
            version: Some("0.1.0".to_string()),
        };
        let content = "# My Skill\nDo the thing.";
        let rendered = FileSkillStore::render_skill_file(&fm, content);

        let (parsed_fm, parsed_body) = FileSkillStore::parse_skill_file(&rendered).unwrap();
        assert_eq!(parsed_fm.name, "my-skill");
        assert_eq!(parsed_fm.category, Some("general".to_string()));
        assert_eq!(parsed_fm.description, Some("A test skill".to_string()));
        assert!(parsed_body.contains("Do the thing"));
    }

    #[tokio::test]
    async fn test_save_and_load_skill() {
        let dir = tempdir().unwrap();
        let store = FileSkillStore::new(dir.path().to_path_buf());

        let skill = Skill {
            name: "greet".to_string(),
            content: "# Greet\nSay hello.".to_string(),
            category: Some("social".to_string()),
            description: Some("Greets people".to_string()),
        };

        store.save(&skill).await.unwrap();
        let loaded = store.load("greet").await.unwrap().unwrap();

        assert_eq!(loaded.name, "greet");
        assert_eq!(loaded.category, Some("social".to_string()));
        assert!(loaded.content.contains("Say hello."));
    }

    #[tokio::test]
    async fn test_list_skills() {
        let dir = tempdir().unwrap();
        let store = FileSkillStore::new(dir.path().to_path_buf());

        let skill_a = Skill {
            name: "skill-a".to_string(),
            content: "Content A".to_string(),
            category: Some("cat1".to_string()),
            description: None,
        };
        let skill_b = Skill {
            name: "skill-b".to_string(),
            content: "Content B".to_string(),
            category: None,
            description: None,
        };

        store.save(&skill_a).await.unwrap();
        store.save(&skill_b).await.unwrap();

        let metas = store.list().await.unwrap();
        assert_eq!(metas.len(), 2);
    }

    #[tokio::test]
    async fn test_delete_skill() {
        let dir = tempdir().unwrap();
        let store = FileSkillStore::new(dir.path().to_path_buf());

        let skill = Skill {
            name: "temp".to_string(),
            content: "Temporary".to_string(),
            category: None,
            description: None,
        };

        store.save(&skill).await.unwrap();
        store.delete("temp").await.unwrap();

        let result = store.load("temp").await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_load_nonexistent() {
        let dir = tempdir().unwrap();
        let store = FileSkillStore::new(dir.path().to_path_buf());
        let result = store.load("nope").await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_reject_path_traversal_name_on_save_and_load() {
        let dir = tempdir().unwrap();
        let store = FileSkillStore::new(dir.path().to_path_buf());

        let skill = Skill {
            name: "../escape".to_string(),
            content: "Bad".to_string(),
            category: None,
            description: None,
        };
        assert!(store.save(&skill).await.is_err());
        assert!(store.load("../escape").await.is_err());
    }

    #[tokio::test]
    async fn test_reject_non_canonical_skill_names() {
        let dir = tempdir().unwrap();
        let store = FileSkillStore::new(dir.path().to_path_buf());

        for name in ["MySkill", "-invalid", "skill name", "skill@name"] {
            let skill = Skill {
                name: name.to_string(),
                content: "Body".to_string(),
                category: None,
                description: None,
            };
            assert!(store.save(&skill).await.is_err(), "{name} should fail");
        }

        for name in ["my-skill", "skill123", "my_skill.v2", "a"] {
            let skill = Skill {
                name: name.to_string(),
                content: "Body".to_string(),
                category: None,
                description: None,
            };
            assert!(store.save(&skill).await.is_ok(), "{name} should pass");
        }
    }

    #[tokio::test]
    async fn test_reject_path_traversal_category_on_save() {
        let dir = tempdir().unwrap();
        let store = FileSkillStore::new(dir.path().to_path_buf());

        let skill = Skill {
            name: "safe-skill".to_string(),
            content: "Body".to_string(),
            category: Some("../badcat".to_string()),
            description: None,
        };
        assert!(store.save(&skill).await.is_err());
    }

    #[tokio::test]
    async fn test_list_ignores_hidden_directories() {
        let dir = tempdir().unwrap();
        let store = FileSkillStore::new(dir.path().to_path_buf());

        let visible = Skill {
            name: "visible".to_string(),
            content: "# Visible".to_string(),
            category: None,
            description: None,
        };
        store.save(&visible).await.unwrap();

        let hidden_skill_dir = dir.path().join(".hidden").join("secret-skill");
        fs::create_dir_all(&hidden_skill_dir).await.unwrap();
        let fm = SkillFrontmatter {
            name: "secret-skill".to_string(),
            category: None,
            description: None,
            version: Some("0.1.0".to_string()),
        };
        let hidden_content = FileSkillStore::render_skill_file(&fm, "# Secret");
        fs::write(hidden_skill_dir.join("SKILL.md"), hidden_content)
            .await
            .unwrap();

        let metas = store.list().await.unwrap();
        assert_eq!(metas.len(), 1);
        assert_eq!(metas[0].name, "visible");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_list_follows_symlinked_skill_directories_once() {
        use std::os::unix::fs as unix_fs;

        let dir = tempdir().unwrap();
        let store = FileSkillStore::new(dir.path().to_path_buf());

        let real_skill_dir = dir.path().join("real-cat").join("linked-skill");
        fs::create_dir_all(&real_skill_dir).await.unwrap();
        let fm = SkillFrontmatter {
            name: "linked-skill".to_string(),
            category: None,
            description: Some("reachable through symlink".to_string()),
            version: Some("0.1.0".to_string()),
        };
        fs::write(
            real_skill_dir.join("SKILL.md"),
            FileSkillStore::render_skill_file(&fm, "# Linked Skill\nBody"),
        )
        .await
        .unwrap();

        let alias_dir = dir.path().join("alias-cat");
        unix_fs::symlink(dir.path().join("real-cat"), &alias_dir).unwrap();

        let metas = store.list().await.unwrap();
        let hits = metas.iter().filter(|m| m.name == "linked-skill").count();
        assert_eq!(hits, 1);
    }

    /// Link `link` to the directory `target`: a symlink on Unix, a junction
    /// (which needs no privilege) on Windows.
    fn link_dir(target: &Path, link: &Path) {
        #[cfg(unix)]
        std::os::unix::fs::symlink(target, link).unwrap();
        #[cfg(windows)]
        {
            let status = std::process::Command::new("cmd")
                .arg("/C")
                .arg("mklink")
                .arg("/J")
                .arg(link)
                .arg(target)
                .stdout(std::process::Stdio::null())
                .status()
                .unwrap();
            assert!(status.success(), "mklink /J failed");
        }
    }

    async fn write_skill(dir: &Path, name: &str) {
        fs::create_dir_all(dir).await.unwrap();
        let fm = SkillFrontmatter {
            name: name.to_string(),
            category: None,
            description: None,
            version: None,
        };
        fs::write(
            dir.join("SKILL.md"),
            FileSkillStore::render_skill_file(&fm, "# Linked\nBody"),
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn test_linked_skill_folders_outside_root_load_and_list() {
        let root = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let store = FileSkillStore::new(root.path().to_path_buf());

        write_skill(&outside.path().join("trail"), "trail").await;
        write_skill(&outside.path().join("top"), "top").await;
        let category = root.path().join("software-development");
        fs::create_dir_all(&category).await.unwrap();
        link_dir(&outside.path().join("trail"), &category.join("trail"));
        link_dir(&outside.path().join("top"), &root.path().join("top"));

        assert_symlink(&category.join("trail"));
        assert!(store.load("trail").await.unwrap().is_some());
        assert!(store.load("top").await.unwrap().is_some());

        let metas = store.list().await.unwrap();
        let trail = metas.iter().find(|m| m.name == "trail").expect("listed");
        assert_eq!(trail.category.as_deref(), Some("software-development"));
        assert!(metas.iter().any(|m| m.name == "top"));
    }

    #[tokio::test]
    async fn test_category_link_outside_root_is_not_followed() {
        let root = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let store = FileSkillStore::new(root.path().to_path_buf());

        write_skill(&outside.path().join("escaped"), "escaped").await;
        let category = root.path().join("software-development");
        link_dir(outside.path(), &category);

        assert_symlink(&category);
        assert!(store.load("escaped").await.unwrap().is_none());
        let metas = store.list().await.unwrap();
        assert!(!metas.iter().any(|m| m.name == "escaped"));
    }

    /// The fix relies on std reporting a junction as a symlink.
    fn assert_symlink(path: &Path) {
        let meta = std::fs::symlink_metadata(path).unwrap();
        assert!(meta.file_type().is_symlink(), "{}", path.display());
    }

    #[tokio::test]
    async fn test_delete_linked_skill_outside_root_fails_and_keeps_target() {
        let root = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let store = FileSkillStore::new(root.path().to_path_buf());

        write_skill(&outside.path().join("trail"), "trail").await;
        link_dir(&outside.path().join("trail"), &root.path().join("trail"));

        let err = store.delete("trail").await.unwrap_err().to_string();
        assert!(err.contains("linked skill 'trail'"), "{err}");
        assert!(outside.path().join("trail").join("SKILL.md").exists());
        assert!(store.load("trail").await.unwrap().is_some());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_real_skill_folder_with_escaping_skill_file_is_refused() {
        let root = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let store = FileSkillStore::new(root.path().to_path_buf());

        write_skill(&outside.path().join("elsewhere"), "sneaky").await;
        let skill = root.path().join("sneaky");
        fs::create_dir_all(&skill).await.unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("elsewhere").join("SKILL.md"),
            skill.join("SKILL.md"),
        )
        .unwrap();

        assert!(store.load("sneaky").await.unwrap().is_none());
        let metas = store.list().await.unwrap();
        assert!(!metas.iter().any(|m| m.name == "sneaky"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_linked_skill_file_escaping_link_target_is_refused() {
        let root = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let store = FileSkillStore::new(root.path().to_path_buf());

        write_skill(&outside.path().join("elsewhere"), "sneaky").await;
        let skill = outside.path().join("sneaky");
        fs::create_dir_all(&skill).await.unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("elsewhere").join("SKILL.md"),
            skill.join("SKILL.md"),
        )
        .unwrap();
        link_dir(&skill, &root.path().join("sneaky"));

        assert!(store.load("sneaky").await.unwrap().is_none());
        let metas = store.list().await.unwrap();
        assert!(!metas.iter().any(|m| m.name == "sneaky"));
    }
}
