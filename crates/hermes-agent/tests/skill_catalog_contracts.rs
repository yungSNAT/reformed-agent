use std::{fs, path::Path};

use hermes_agent::skill_orchestrator::SkillOrchestrator;

fn write_skill(root: &Path, directory: &str, content: &[u8]) {
    let directory = root.join(directory);
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join("SKILL.md"), content).unwrap();
}

#[test]
fn catalogue_recovers_metadata_and_skips_invalid_entries() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path();
    write_skill(
        path,
        "notes/reading-list",
        b"# Reading list\n\n  List the books in order.\n",
    );
    write_skill(path, "notes/heading-only", b"# Heading only\n");
    let duplicate = b"---\nname: duplicate\n---\nRead the sample.\n";
    write_skill(path, "notes/first", duplicate);
    write_skill(path, "notes/second", duplicate);
    write_skill(path, "notes/invalid", &[0xff]);
    write_skill(path, "notes/empty-slug", b"---\nname: '!!!'\n---\n");
    write_skill(path, "notes/.hidden", b"Read the sample.\n");

    let mut catalogue = SkillOrchestrator::new(path);
    catalogue.scan_skill_commands();
    let commands = catalogue.get_commands();
    assert_eq!(commands.len(), 3);

    let key = catalogue.resolve_skill_command_key("reading_list").unwrap();
    assert_eq!(key, "/reading-list");
    assert_eq!(commands[&key].name, "reading-list");
    assert_eq!(commands[&key].description, "List the books in order.");
    assert_eq!(
        catalogue.skill_category(&commands[&key]).as_deref(),
        Some("notes")
    );
    assert_eq!(
        commands["/heading-only"].description,
        "Invoke the heading-only skill"
    );
    assert_eq!(commands["/duplicate"].description, "Read the sample.");

    let other_root = tempfile::tempdir().unwrap();
    let other_catalogue = SkillOrchestrator::new(other_root.path());
    assert_eq!(other_catalogue.skill_category(&commands[&key]), None);
}

#[test]
fn invocation_and_preloading_report_files_removed_after_scanning() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path();
    write_skill(path, "available", b"Read the sample.\n");
    write_skill(path, "relocated", b"Summarise the sample.\n");

    let mut catalogue = SkillOrchestrator::new(path);
    catalogue.scan_skill_commands();
    let source = &catalogue.get_commands()["/relocated"].skill_md_path;
    fs::rename(source, source.with_file_name("archived.md")).unwrap();

    let invocation = catalogue
        .build_skill_invocation_message("/available", "Use a short sentence.")
        .unwrap();
    assert!(invocation.contains("Read the sample."));
    assert!(invocation.ends_with("Use a short sentence."));
    assert_eq!(
        catalogue
            .build_skill_invocation_message("/relocated", "")
            .as_deref(),
        Some("[Failed to load skill: relocated]")
    );
    assert_eq!(
        catalogue.build_skill_invocation_message("/unknown", ""),
        None
    );

    let identifiers = [" relocated ", "", "available", " available ", "unknown"].map(str::to_owned);
    let (prompt, loaded, missing) = catalogue.build_preloaded_skills_prompt(&identifiers);
    assert!(prompt.ends_with("Read the sample."));
    assert_eq!(loaded, ["available"]);
    assert_eq!(missing, ["relocated", "unknown"]);
}
