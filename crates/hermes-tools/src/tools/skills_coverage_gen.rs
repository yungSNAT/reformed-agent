use super::*;
use hermes_core::{AgentError, Skill, SkillMeta};
use std::collections::HashMap;
use std::fs;
use std::sync::Mutex;
use tempfile::tempdir;

type Store = HashMap<String, (String, Option<String>)>;

struct MemProvider {
    store: Mutex<Store>,
    metas: Vec<(String, Option<String>)>,
}

impl MemProvider {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            store: Mutex::new(HashMap::new()),
            metas: Vec::new(),
        })
    }

    fn with_metas(metas: Vec<(&str, Option<&str>)>) -> Arc<Self> {
        Arc::new(Self {
            store: Mutex::new(HashMap::new()),
            metas: metas
                .into_iter()
                .map(|(n, c)| (n.to_string(), c.map(String::from)))
                .collect(),
        })
    }

    fn content(&self, name: &str) -> Option<String> {
        self.store.lock().unwrap().get(name).map(|(c, _)| c.clone())
    }

    fn category(&self, name: &str) -> Option<String> {
        self.store
            .lock()
            .unwrap()
            .get(name)
            .and_then(|(_, cat)| cat.clone())
    }

    fn insert(&self, name: &str, content: &str) {
        self.store
            .lock()
            .unwrap()
            .insert(name.to_string(), (content.to_string(), None));
    }
}

#[async_trait]
impl SkillProvider for MemProvider {
    async fn create_skill(
        &self,
        name: &str,
        content: &str,
        category: Option<&str>,
    ) -> Result<Skill, AgentError> {
        self.store.lock().unwrap().insert(
            name.to_string(),
            (content.to_string(), category.map(String::from)),
        );
        Ok(Skill {
            name: name.into(),
            content: content.into(),
            category: category.map(String::from),
            description: None,
        })
    }

    async fn get_skill(&self, name: &str) -> Result<Option<Skill>, AgentError> {
        let found = self.store.lock().unwrap().get(name).cloned();
        Ok(found.map(|(content, category)| Skill {
            name: name.into(),
            content,
            category,
            description: None,
        }))
    }

    async fn list_skills(&self) -> Result<Vec<SkillMeta>, AgentError> {
        Ok(self
            .metas
            .iter()
            .map(|(n, c)| SkillMeta {
                name: n.clone(),
                category: c.clone(),
                description: None,
            })
            .collect())
    }

    async fn update_skill(&self, name: &str, content: &str) -> Result<Skill, AgentError> {
        let mut store = self.store.lock().unwrap();
        let category = store.get(name).and_then(|(_, c)| c.clone());
        store.insert(name.to_string(), (content.to_string(), category.clone()));
        Ok(Skill {
            name: name.into(),
            content: content.into(),
            category,
            description: None,
        })
    }

    async fn delete_skill(&self, name: &str) -> Result<(), AgentError> {
        self.store.lock().unwrap().remove(name);
        Ok(())
    }
}

#[tokio::test]
async fn list_sorts_by_name_then_category() {
    let provider = MemProvider::with_metas(vec![
        ("beta", None),
        ("alpha", Some("zeta")),
        ("alpha", Some("core")),
    ]);
    let handler = SkillsListHandler::new(provider);
    let out = handler.execute(json!({})).await.unwrap();
    let payload: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(payload["count"], json!(3));
    assert_eq!(payload["names"], json!(["alpha", "alpha", "beta"]));
    assert_eq!(payload["skills"][0]["category"], json!("core"));
    assert_eq!(payload["skills"][1]["category"], json!("zeta"));
    assert_eq!(payload["skills"][2]["name"], json!("beta"));
}

#[tokio::test]
async fn view_missing_name_is_invalid_params() {
    let handler = SkillViewHandler::with_skill_roots(MemProvider::new(), vec![]);
    let err = handler.execute(json!({})).await.unwrap_err();
    assert!(matches!(err, ToolError::InvalidParams(ref m) if m.contains("name")));
}

#[tokio::test]
async fn view_missing_file_lists_available_files() {
    let tmp = tempdir().unwrap();
    let root = tmp.path().join("skills");
    let skill = root.join("my-skill");
    fs::create_dir_all(skill.join("references")).unwrap();
    fs::write(skill.join("SKILL.md"), "# s").unwrap();
    fs::write(skill.join("references/api.md"), "api").unwrap();

    let handler = SkillViewHandler::with_skill_roots(MemProvider::new(), vec![root]);
    let out = handler
        .execute(json!({"name": "my-skill", "file_path": "references/missing.md"}))
        .await
        .unwrap();
    let payload: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(payload["success"], json!(false));
    assert_eq!(
        payload["available_files"],
        json!({"references": ["references/api.md"]})
    );
}

#[test]
fn has_traversal_component_variants() {
    let abs = std::env::temp_dir();
    assert!(has_traversal_component(&abs.to_string_lossy()));
    assert!(has_traversal_component("a/../b"));
    assert!(!has_traversal_component("a/b.md"));
}

#[test]
fn resolve_skill_dir_branches() {
    let tmp = tempdir().unwrap();
    let root = tmp.path().join("skills");
    let cat = root.join("cat");
    fs::create_dir_all(cat.join("myskill")).unwrap();
    fs::create_dir_all(cat.join("other")).unwrap();
    fs::write(cat.join("myskill").join("SKILL.md"), "# m").unwrap();
    fs::write(cat.join("other").join("SKILL.md"), "# o").unwrap();
    fs::write(cat.join("notes.txt"), "n").unwrap();
    let missing_root = tmp.path().join("does-not-exist");
    let roots = vec![missing_root, root.clone()];

    assert_eq!(resolve_skill_dir("../myskill", &roots), None);
    assert_eq!(
        resolve_skill_dir("myskill", &roots),
        Some(cat.join("myskill"))
    );
    assert_eq!(
        resolve_skill_dir("cat/myskill", &roots),
        Some(root.join("cat/myskill"))
    );
    assert_eq!(resolve_skill_dir("cat/missing", &roots), None);
    assert_eq!(resolve_skill_dir("nope", &roots), None);
    assert_eq!(find_skill_dir_by_name(&root, "nope"), None);
}

#[test]
fn validate_within_skill_dir_relative_missing_path_is_invalid() {
    let tmp = tempdir().unwrap();
    let rel = PathBuf::from(format!(
        "__hermes_cov_missing_dir_{}__/f.md",
        std::process::id()
    ));
    assert_eq!(
        validate_within_skill_dir(&rel, tmp.path()),
        Err("Invalid file path".to_string())
    );
}

#[test]
fn validate_within_skill_dir_nonexistent_leaf_inside_is_ok() {
    let tmp = tempdir().unwrap();
    let target = tmp.path().join("sub").join("deeper").join("x.md");
    assert_eq!(validate_within_skill_dir(&target, tmp.path()), Ok(()));
}

#[test]
fn collect_available_skill_files_categorizes() {
    let tmp = tempdir().unwrap();
    let d = tmp.path();
    for sub in ["references", "templates", "assets", "scripts", "nested"] {
        fs::create_dir_all(d.join(sub)).unwrap();
    }
    fs::write(d.join("SKILL.md"), "# s").unwrap();
    fs::write(d.join("references/b.md"), "b").unwrap();
    fs::write(d.join("references/a.md"), "a").unwrap();
    fs::write(d.join("templates/t.txt"), "t").unwrap();
    fs::write(d.join("assets/x.png"), "x").unwrap();
    fs::write(d.join("scripts/run.sh"), "r").unwrap();
    fs::write(d.join("nested/deep.yaml"), "y").unwrap();
    fs::write(d.join("nested/SKILL.md"), "# n").unwrap();
    fs::write(d.join("notes.md"), "n").unwrap();
    fs::write(d.join("README.MD"), "r").unwrap();
    fs::write(d.join("image.png"), "i").unwrap();

    let out = collect_available_skill_files(d);
    assert_eq!(
        out,
        json!({
            "assets": ["assets/x.png"],
            "other": ["README.MD", "nested/deep.yaml", "notes.md"],
            "references": ["references/a.md", "references/b.md"],
            "scripts": ["scripts/run.sh"],
            "templates": ["templates/t.txt"]
        })
    );
}

#[test]
fn collect_available_skill_files_empty_dir() {
    let tmp = tempdir().unwrap();
    assert_eq!(collect_available_skill_files(tmp.path()), json!({}));
}

#[test]
fn normalize_relative_path_drops_non_normal() {
    assert_eq!(normalize_relative_path(Path::new("./a/b/c.md")), "a/b/c.md");
    assert_eq!(normalize_relative_path(Path::new(".")), "");
}

#[tokio::test]
async fn manage_missing_action() {
    let handler = SkillManageHandler::new(MemProvider::new());
    let err = handler.execute(json!({"name": "x"})).await.unwrap_err();
    assert!(matches!(err, ToolError::InvalidParams(ref m) if m.contains("action")));
}

#[tokio::test]
async fn manage_create_validation_and_category() {
    let provider = MemProvider::new();
    let handler = SkillManageHandler::new(provider.clone());
    let err = handler
        .execute(json!({"action": "create", "content": "c"}))
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::InvalidParams(ref m) if m.contains("name")));
    let err = handler
        .execute(json!({"action": "create", "name": "n"}))
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::InvalidParams(ref m) if m.contains("content")));

    let out = handler
        .execute(json!({"action": "create", "name": "n", "content": "c", "category": "cat"}))
        .await
        .unwrap();
    assert_eq!(out, "Skill 'n' created successfully");
    assert_eq!(provider.content("n").as_deref(), Some("c"));
    assert_eq!(provider.category("n").as_deref(), Some("cat"));
}

#[tokio::test]
async fn manage_update_validation_and_success() {
    let provider = MemProvider::new();
    provider.insert("n", "old");
    let handler = SkillManageHandler::new(provider.clone());
    let err = handler
        .execute(json!({"action": "update", "content": "c"}))
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::InvalidParams(ref m) if m.contains("name")));
    let err = handler
        .execute(json!({"action": "update", "name": "n"}))
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::InvalidParams(ref m) if m.contains("content")));

    let out = handler
        .execute(json!({"action": "update", "name": "n", "content": "new"}))
        .await
        .unwrap();
    assert_eq!(out, "Skill 'n' updated successfully");
    assert_eq!(provider.content("n").as_deref(), Some("new"));
}

#[tokio::test]
async fn manage_delete_validation_and_success() {
    let provider = MemProvider::new();
    provider.insert("n", "c");
    let handler = SkillManageHandler::new(provider.clone());
    let err = handler
        .execute(json!({"action": "delete"}))
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::InvalidParams(ref m) if m.contains("name")));
    let out = handler
        .execute(json!({"action": "delete", "name": "n"}))
        .await
        .unwrap();
    assert_eq!(out, "Skill 'n' deleted successfully");
    assert_eq!(provider.content("n"), None);
}

#[tokio::test]
async fn manage_auto_create() {
    let provider = MemProvider::new();
    let handler = SkillManageHandler::new(provider.clone());
    let err = handler
        .execute(json!({"action": "auto_create"}))
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::InvalidParams(ref m) if m.contains("name")));

    let out = handler
        .execute(json!({
            "action": "auto_create",
            "name": "deploy",
            "summary": "Deploy app",
            "steps": ["build", 42, "ship"]
        }))
        .await
        .unwrap();
    assert_eq!(out, "Skill 'deploy' auto-created");
    assert_eq!(
        provider.content("deploy").as_deref(),
        Some("# deploy\n\n## Summary\nDeploy app\n\n## Steps\n1. build\n2. ship\n")
    );
    assert_eq!(
        provider.category("deploy").as_deref(),
        Some("auto-generated")
    );

    handler
        .execute(json!({"action": "auto_create", "name": "plain"}))
        .await
        .unwrap();
    assert_eq!(
        provider.content("plain").as_deref(),
        Some("# plain\n\n## Summary\nAuto-generated from completed task.\n\n## Steps\n")
    );
}

#[tokio::test]
async fn manage_self_improve() {
    let provider = MemProvider::new();
    provider.insert("s", "base");
    let handler = SkillManageHandler::new(provider.clone());
    let err = handler
        .execute(json!({"action": "self_improve"}))
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::InvalidParams(ref m) if m.contains("name")));

    let err = handler
        .execute(json!({"action": "self_improve", "name": "ghost"}))
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::NotFound(ref m) if m.contains("ghost")));

    let out = handler
        .execute(json!({"action": "self_improve", "name": "s"}))
        .await
        .unwrap();
    assert_eq!(out, "Skill 's' improved");
    assert_eq!(
        provider.content("s").as_deref(),
        Some("base\n\n## Improvement Feedback\nNo feedback provided.\n")
    );

    handler
        .execute(json!({"action": "self_improve", "name": "s", "feedback": "be faster"}))
        .await
        .unwrap();
    assert!(provider
        .content("s")
        .unwrap()
        .ends_with("## Improvement Feedback\nbe faster\n"));
}

#[tokio::test]
async fn manage_install_builtins_skips_existing() {
    let provider = MemProvider::new();
    provider.insert("planning", "custom");
    let handler = SkillManageHandler::new(provider.clone());
    let out = handler
        .execute(json!({"action": "install_builtins"}))
        .await
        .unwrap();
    assert_eq!(out, "Installed 25 built-in skills");
    assert_eq!(provider.content("planning").as_deref(), Some("custom"));
    assert_eq!(provider.category("git").as_deref(), Some("builtin"));
}
