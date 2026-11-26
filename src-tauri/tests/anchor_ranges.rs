#[allow(dead_code)]
#[path = "../src/engine.rs"]
mod engine;

use engine::{scan_with_anchors, AnchorMap};
use std::fs;
use std::path::Path;
use std::process::Command;
use tempfile::tempdir;

const BASE: i64 = 1_700_000_000;

fn git(path: &Path, offset: i64, args: &[&str]) -> String {
    let when = format!("@{} +0000", BASE + offset);
    let result = Command::new("git").current_dir(path)
        .env("GIT_AUTHOR_DATE", &when).env("GIT_COMMITTER_DATE", &when)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", if cfg!(windows) { "NUL" } else { "/dev/null" })
        .args(["-c", "user.name=Anchor Test", "-c", "user.email=test@example.invalid",
               "-c", "commit.gpgSign=false", "-c", "core.hooksPath=.git/disabled-hooks"])
        .args(args).output().unwrap();
    assert!(result.status.success(), "{args:?}: {}", String::from_utf8_lossy(&result.stderr));
    String::from_utf8(result.stdout).unwrap().trim().to_string()
}

fn init(path: &Path) {
    fs::create_dir_all(path).unwrap();
    git(path, 0, &["init", "-b", "main"]);
}

fn add_file(repo: &Path, name: &str, offset: i64) -> String {
    fs::write(repo.join(name), name.as_bytes()).unwrap();
    git(repo, offset, &["add", name]);
    git(repo, offset, &["commit", "-m", name]);
    git(repo, offset, &["rev-parse", "HEAD"])
}

#[test]
fn anchors_use_exact_repo_branch_and_optional_date_cutoff() {
    let root = tempdir().unwrap();
    let a = root.path().join("服务端");
    let b = root.path().join("组/客户端");
    let mut anchors = AnchorMap::new();
    for (repo, relative) in [(&a, "服务端"), (&b, "组/客户端")] {
        init(repo);
        add_file(repo, "A", 0);
        let anchor = add_file(repo, "B", 10);
        add_file(repo, "C", 20);
        add_file(repo, "D", 30);
        anchors.entry(relative.into()).or_default().insert("refs/heads/main".into(), anchor);
    }
    for (end, count) in [(None, 4), (Some(BASE + 30), 2), (Some(BASE + 20), 0)] {
        let preview = scan_with_anchors(root.path(), None, end, &anchors).unwrap();
        assert!(preview.errors.is_empty(), "{:?}", preview.errors);
        assert_eq!(preview.file_count, count);
        for repo in &preview.repositories {
            assert_eq!(repo.branch_key, "refs/heads/main");
            assert_eq!(repo.start, repo.scanned_anchor);
            assert!(!repo.rebuild_anchor);
            assert!(repo.changes.iter().all(|file| file.path == "C" || file.path == "D"));
        }
    }
    let too_early = scan_with_anchors(root.path(), None, Some(BASE + 10), &anchors).unwrap();
    assert_eq!(too_early.errors.len(), 2);
    assert!(too_early.errors.iter().all(|reason| reason.contains("截止时间早于")));
    let historical = scan_with_anchors(root.path(), Some(BASE - 1), Some(BASE + 10), &anchors).unwrap();
    assert_eq!(historical.file_count, 2);
    assert!(historical.repositories.iter().all(|repo| !repo.rebuild_anchor));

    git(&b, 40, &["switch", "-c", "feature"]);
    let changed = scan_with_anchors(root.path(), None, None, &anchors).unwrap();
    assert_eq!(changed.errors.len(), 1);
    assert!(changed.errors[0].contains("feature") && changed.errors[0].contains("尚无锚点"));
    assert_eq!(changed.repositories.len(), 1);
    assert!(engine::export(&changed, &root.path().join("partial.zip")).is_err());
    git(&b, 40, &["switch", "--detach", "main"]);
    let detached = scan_with_anchors(&b, Some(BASE), Some(BASE + 60), &AnchorMap::new()).unwrap();
    assert_eq!(detached.repositories[0].branch_key, "HEAD");
}

#[test]
fn distinguishes_rewritten_history_from_a_historical_date_export() {
    let root = tempdir().unwrap();
    let repo = root.path();
    init(repo);
    let a = add_file(repo, "old", 0);
    git(repo, 10, &["switch", "-c", "topic"]);
    let side = add_file(repo, "side", 20);
    git(repo, 25, &["switch", "main"]);
    add_file(repo, "main", 30);
    git(repo, 40, &["merge", "--no-ff", "topic", "-m", "merge"]);
    let tip = git(repo, 40, &["rev-parse", "HEAD"]);
    assert!(!engine::on_first_parent_chain(repo, &side, &tip).unwrap());
    assert!(engine::on_first_parent_chain(repo, &a, &tip).unwrap());
    let mut anchors = AnchorMap::new();
    anchors.entry(String::new()).or_default().insert("refs/heads/main".into(), side);
    let blocked = scan_with_anchors(repo, None, None, &anchors).unwrap();
    assert!(blocked.errors[0].contains("重建锚点"));
    // Even a historical end shared with the old branch must ask for a confirmed rebuild.
    let date = scan_with_anchors(repo, Some(BASE-1), Some(BASE+10), &anchors).unwrap();
    assert!(date.errors.is_empty());
    assert!(date.repositories[0].rebuild_anchor);
    assert_eq!(date.repositories[0].end.as_deref(), Some(a.as_str()));
    assert_eq!(date.repositories[0].scanned_tip.as_deref(), Some(tip.as_str()));
}

#[test]
fn incomplete_history_is_not_a_rewrite() {
    let root = tempdir().unwrap();
    let origin = root.path().join("origin");
    let clone = root.path().join("clone");
    init(&origin);
    let missing = add_file(&origin, "missing", 0);
    add_file(&origin, "local", 10);
    git(root.path(), 10, &["clone", "--depth", "1", "--no-local", origin.to_str().unwrap(), clone.to_str().unwrap()]);
    let mut anchors = AnchorMap::new();
    anchors.entry(String::new()).or_default().insert("refs/heads/main".into(), missing);
    for start in [Some(BASE), None] {
        let preview = scan_with_anchors(&clone, start, Some(BASE+20), &anchors).unwrap();
        assert!(preview.repositories.is_empty());
        assert!(preview.errors[0].contains("补全历史"));
    }
}
