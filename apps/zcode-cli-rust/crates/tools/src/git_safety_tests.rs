//! Scenarios verified against the Node implementation.
use super::context_unsafe;
use std::{fs, path::Path};

fn repo(root: &Path) {
    fs::create_dir_all(root.join(".git/objects")).unwrap();
    fs::create_dir_all(root.join(".git/refs")).unwrap();
    fs::write(root.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
}

#[test]
fn plain_directories_and_repository_roots_are_safe() {
    let dir = tempfile::tempdir().unwrap();
    let plain = dir.path().join("plain");
    fs::create_dir(&plain).unwrap();
    assert!(!context_unsafe(&plain));
    let root = dir.path().join("repo");
    repo(&root);
    assert!(!context_unsafe(&root));
    let nested = root.join("src/deep");
    fs::create_dir_all(&nested).unwrap();
    assert!(!context_unsafe(&nested));
}

#[test]
fn bare_repository_indicators_are_unsafe_even_inside_a_repository() {
    let dir = tempfile::tempdir().unwrap();
    let refs = dir.path().join("withrefs");
    fs::create_dir_all(refs.join("refs")).unwrap();
    assert!(context_unsafe(&refs));
    let root = dir.path().join("repo");
    repo(&root);
    let sub = root.join("sub");
    fs::create_dir_all(sub.join("refs")).unwrap();
    assert!(context_unsafe(&sub));
    let head = dir.path().join("head");
    fs::create_dir(&head).unwrap();
    fs::write(head.join("HEAD"), "x").unwrap();
    assert!(context_unsafe(&head));
}

#[test]
fn gitdir_files_are_trusted_only_outside_the_working_directory() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("repo");
    repo(&root);
    let worktree = dir.path().join("wt");
    fs::create_dir(&worktree).unwrap();
    fs::write(worktree.join(".git"), "gitdir: ../repo/.git\r\n").unwrap();
    assert!(!context_unsafe(&worktree));

    let inside = dir.path().join("inside");
    repo(&inside.join("nested"));
    fs::write(inside.join(".git"), "gitdir: nested/.git\n").unwrap();
    assert!(context_unsafe(&inside));

    let no_segment = dir.path().join("nosegment");
    fs::create_dir(&no_segment).unwrap();
    fs::write(no_segment.join(".git"), "gitdir: ../repo\n").unwrap();
    assert!(context_unsafe(&no_segment));

    let missing = dir.path().join("missing");
    fs::create_dir(&missing).unwrap();
    fs::write(missing.join(".git"), "gitdir: ../nowhere/.git\n").unwrap();
    assert!(context_unsafe(&missing));

    let nul = dir.path().join("nul");
    fs::create_dir(&nul).unwrap();
    fs::write(nul.join(".git"), "gitdir: \0").unwrap();
    assert!(context_unsafe(&nul));
}

#[test]
fn head_must_name_a_ref_or_an_object_id() {
    let dir = tempfile::tempdir().unwrap();
    for (head, trusted) in [
        ("ref:\trefs/heads/x", true),
        (&*format!("{}\n", "a".repeat(40)), true),
        (&*"b".repeat(64), true),
        (&*"c".repeat(50), false),
        ("ref: heads/x", false),
        (&*"A".repeat(40), false),
    ] {
        let root = dir.path().join(format!("r{}", head.len()));
        let _ = fs::remove_dir_all(&root);
        repo(&root);
        fs::write(root.join(".git/HEAD"), head).unwrap();
        assert_eq!(super::valid_head(&root.join(".git")), trusted, "{head:?}");
    }
}
