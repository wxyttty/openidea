//! Stage-by-hunk: `Repo::line_hunks` describes the changes of a file and
//! `Repo::stage_hunks` stages a chosen subset. These drive a real temporary Git
//! repository, so the plumbing the VCS panel relies on is verified end to end.
#![recursion_limit = "512"]

use ide_vcs::{HunkKind, Repo};
use std::process::Command;

fn git(dir: &std::path::Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .output()
        .expect("git runs");
    assert!(status.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&status.stderr));
}

fn repo(files: &[(&str, &str)]) -> (tempfile::TempDir, std::path::PathBuf, Repo) {
    let temp = tempfile::tempdir().expect("temp dir");
    let root = temp.path().to_path_buf();
    git(&root, &["init", "-q"]);
    for (name, text) in files {
        std::fs::write(root.join(name), text).expect("write");
    }
    git(&root, &["add", "."]);
    git(&root, &["commit", "-q", "-m", "base"]);
    let repo = Repo::discover(&root).expect("discover").expect("a repository");
    (temp, root, repo)
}

#[test]
fn line_hunks_describe_each_change_of_a_file() {
    let (_temp, root, repo) = repo(&[("a.txt", "one\ntwo\nthree\n")]);
    // Change line 2 and add a line at the end: two separate hunks.
    let current = "one\nTWO\nthree\nfour\n";
    std::fs::write(root.join("a.txt"), current).unwrap();
    let hunks = repo.line_hunks(&root.join("a.txt"), current, false, ide_vcs::TextEncoding::from(encoding_rs::UTF_8)).expect("hunks");
    assert_eq!(hunks.len(), 2, "one modification and one addition: {hunks:?}");
    assert_eq!(hunks[0].kind, HunkKind::Modified);
    assert!(hunks.iter().any(|hunk| hunk.kind == HunkKind::Added));
}

#[test]
fn staging_one_hunk_leaves_the_other_unstaged() {
    let (_temp, root, repo) = repo(&[("a.txt", "one\ntwo\nthree\n")]);
    let current = "one\nTWO\nthree\nfour\n";
    std::fs::write(root.join("a.txt"), current).unwrap();
    let hunks = repo
        .line_hunks(&root.join("a.txt"), current, false, ide_vcs::TextEncoding::from(encoding_rs::UTF_8))
        .expect("hunks");
    // Stage only the first hunk (the modification on line 2).
    repo.stage_hunks(&root.join("a.txt"), current, &[0], ide_vcs::TextEncoding::from(encoding_rs::UTF_8))
        .expect("stage hunk");

    // The index holds exactly the staged hunk; the working tree keeps both.
    let index = Command::new("git")
        .args(["show", ":0:a.txt"])
        .current_dir(&root)
        .output()
        .expect("git show");
    let index_text = String::from_utf8_lossy(&index.stdout);
    assert!(index_text.contains("TWO"), "the modification reached the index: {index_text}");
    assert!(!index_text.contains("four"), "the addition stays out of the index: {index_text}");
    let working = std::fs::read_to_string(root.join("a.txt")).expect("working tree");
    assert!(working.contains("TWO") && working.contains("four"), "the file itself is untouched");
    let _ = hunks;
}

#[test]
fn an_untracked_file_is_one_added_hunk() {
    let (_temp, root, repo) = repo(&[("base.txt", "x\n")]);
    std::fs::write(root.join("new.txt"), "a\nb\n").unwrap();
    let hunks = repo
        .line_hunks(&root.join("new.txt"), "a\nb\n", false, ide_vcs::TextEncoding::from(encoding_rs::UTF_8))
        .expect("hunks");
    assert_eq!(hunks.len(), 1);
    assert_eq!(hunks[0].kind, HunkKind::Added);
    assert_eq!(hunks[0].lines, 2);
}
