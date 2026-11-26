#[path = "../src/engine.rs"]
mod engine;
#[path = "../src/anchors.rs"]
mod anchors;

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::tempdir;

const START: i64 = 1_700_000_040;
const END: i64 = START + 60;

fn git(repo: &Path, when: i64, args: &[&str]) -> Vec<u8> {
    let date = format!("@{when} +0000");
    let output = Command::new("git")
        .current_dir(repo)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", if cfg!(windows) { "NUL" } else { "/dev/null" })
        .env("GIT_AUTHOR_DATE", &date)
        .env("GIT_COMMITTER_DATE", &date)
        .args(["-c", "user.name=Snapshot Test", "-c", "user.email=snapshots@example.invalid",
               "-c", "core.hooksPath=.git/empty-hooks", "-c", "commit.gpgSign=false"])
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success(), "{args:?}: {}", String::from_utf8_lossy(&output.stderr));
    output.stdout
}

fn init(path: &Path) {
    fs::create_dir_all(path).unwrap();
    git(path, START, &["init", "-b", "main"]);
    git(path, START, &["config", "core.autocrlf", "false"]);
    git(path, START, &["config", "diff.noprefix", "true"]);
}

fn put(repo: &Path, path: &str, bytes: &[u8]) {
    let full = repo.join(path);
    fs::create_dir_all(full.parent().unwrap()).unwrap();
    fs::write(full, bytes).unwrap();
}

fn head(repo: &Path) -> String {
    String::from_utf8(git(repo, START, &["rev-parse", "HEAD"])).unwrap().trim().to_string()
}

fn commit(repo: &Path, at: i64) -> String {
    git(repo, at, &["add", "-A"]);
    git(repo, at, &["commit", "-m", "Fixture"]);
    head(repo)
}

fn scan(root: &Path, from: i64, through: i64) -> engine::Preview {
    let preview = engine::scan(root, from, through).unwrap();
    assert!(preview.errors.is_empty(), "{:?}", preview.errors);
    preview
}

fn entries(path: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut zip = zip::ZipArchive::new(fs::File::open(path).unwrap()).unwrap();
    let mut contents = BTreeMap::new();
    for index in 0..zip.len() {
        let mut entry = zip.by_index(index).unwrap();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).unwrap();
        assert!(contents.insert(entry.name().to_string(), bytes).is_none());
    }
    contents
}

#[test]
fn preserves_scope_and_newest_commit_across_reopened_and_overlapping_exports() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("全部项目");
    let repo = root.join("服务端");
    let second = root.join("桌面");
    let data = temp.path().join("appdata");
    let output = temp.path().join("out.zip");
    init(&repo);
    init(&second);
    put(&second, "other", b"unchanged during date range");
    let second_tip = commit(&second, START - 1);
    put(&repo, "code", b"B");
    let b = commit(&repo, START);

    let scope = anchors::scope_key(&root).unwrap();
    let single = anchors::scope_key(&repo).unwrap();
    let seed = scan(&root, START, END);
    let first_scan = anchors::load(&data, &scope).unwrap();
    assert!(first_scan.commits.is_empty());
    let bad_dest = temp.path().join("missing/out.zip");
    assert!(anchors::export(&data, &scope, &first_scan, &seed, &bad_dest, false).is_err());
    assert!(anchors::load(&data, &scope).unwrap().commits.is_empty());
    let written = anchors::export(&data, &scope, &first_scan, &seed, &output, false).unwrap();
    assert_eq!(written.file_count, 1);
    assert!(written.anchor_error.is_none());
    let saved = anchors::load(&data, &scope).unwrap();
    assert_eq!(saved.commits["服务端"]["refs/heads/main"], b);
    assert_eq!(saved.commits["桌面"]["refs/heads/main"], second_tip);
    assert!(anchors::load(&data, &single).unwrap().commits.is_empty());

    put(&repo, "code", b"C");
    let c = commit(&repo, END);
    let preview_c = engine::scan_with_anchors(&root, None, None, &saved.commits).unwrap();
    put(&repo, "code", b"D");
    let d = commit(&repo, END + 5);
    let preview_d = engine::scan_with_anchors(&root, None, None, &saved.commits).unwrap();
    assert!(anchors::export(&data, &scope, &saved, &preview_d, &output, false).unwrap().anchor_error.is_none());
    // A second window finishes its earlier preview after the first one.
    assert!(anchors::export(&data, &scope, &saved, &preview_c, &output, false).unwrap().anchor_error.is_none());
    assert_eq!(entries(&output)["服务端/code"], b"C");
    let saved = anchors::load(&data, &scope).unwrap();
    assert_eq!(saved.commits["服务端"]["refs/heads/main"], d);
    let backfill = engine::scan_with_anchors(&root, Some(END), Some(END + 1), &saved.commits).unwrap();
    assert!(backfill.repositories.iter().any(|r| r.end.as_ref() == Some(&c)));
    assert!(anchors::export(&data, &scope, &saved, &backfill, &output, false).unwrap().anchor_error.is_none());
    assert_eq!(anchors::load(&data, &scope).unwrap(), saved);
    let empty = engine::scan_with_anchors(&root, None, None, &saved.commits).unwrap();
    assert!(anchors::export(&data, &scope, &saved, &empty, &output, false).is_err());
    assert_eq!(anchors::load(&data, &scope).unwrap(), saved);
}

#[test]
fn distinct_zip_and_anchor_failures_and_explicit_rebuild() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    let data = temp.path().join("data");
    let zip = temp.path().join("out.zip");
    init(&repo);
    put(&repo, "code", b"original");
    commit(&repo, START);
    let scope = anchors::scope_key(&repo).unwrap();
    let seed = scan(&repo, START, END);
    let initial = anchors::load(&data, &scope).unwrap();
    assert!(anchors::export(&data, &scope, &initial, &seed, &zip, false).unwrap().anchor_error.is_none());
    let saved = anchors::load(&data, &scope).unwrap();
    put(&repo, "code", b"rewritten");
    git(&repo, START + 1, &["add", "-A"]);
    git(&repo, START + 1, &["commit", "--amend", "-m", "new history"]);
    let new_head = head(&repo);
    let blocked = engine::scan_with_anchors(&repo, None, None, &saved.commits).unwrap();
    assert!(!blocked.errors.is_empty());
    let reset = engine::scan_with_anchors(&repo, Some(START), Some(END), &saved.commits).unwrap();
    assert!(reset.repositories[0].rebuild_anchor);
    assert!(anchors::export(&data, &scope, &saved, &reset, &zip, false).is_err());
    assert_eq!(entries(&zip)["code"], b"original");
    assert_eq!(anchors::load(&data, &scope).unwrap(), saved);
    assert!(anchors::export(&data, &scope, &saved, &reset, &zip, true).unwrap().anchor_error.is_none());
    assert_eq!(anchors::load(&data, &scope).unwrap().commits[""]["refs/heads/main"], new_head);

    // Fail the state write after preview, leaving the successfully generated ZIP usable.
    let bad_data = temp.path().join("unusable");
    fs::write(&bad_data, b"not a directory").unwrap();
    let result = anchors::export(&bad_data, &scope, &saved, &reset, &zip, true).unwrap();
    assert_eq!(result.file_count, 1);
    assert!(result.anchor_error.is_some());
    assert_eq!(entries(&zip)["code"], b"rewritten");
}

#[test]
fn stale_preview_cannot_restore_discarded_history_after_anchor_rebuild() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    let data = temp.path().join("data");
    let zip = temp.path().join("out.zip");
    init(&repo);
    put(&repo, "code", b"B");
    let b = commit(&repo, START);
    let scope = anchors::scope_key(&repo).unwrap();
    let seed = scan(&repo, START, END);
    let first = anchors::load(&data, &scope).unwrap();
    anchors::export(&data, &scope, &first, &seed, &zip, false).unwrap();
    let original = anchors::load(&data, &scope).unwrap();
    put(&repo, "code", b"C");
    commit(&repo, END + 1);
    let stale = engine::scan_with_anchors(&repo, None, None, &original.commits).unwrap();
    assert!(anchors::export(&data, &scope, &original, &stale, &zip, false).unwrap().anchor_error.is_none());

    git(&repo, START, &["switch", "--detach", &b]);
    git(&repo, START, &["branch", "-f", "main", &b]);
    git(&repo, START, &["switch", "main"]);
    let at_c = anchors::load(&data, &scope).unwrap();
    let reset = engine::scan_with_anchors(&repo, Some(START), Some(END), &at_c.commits).unwrap();
    assert!(reset.repositories[0].rebuild_anchor);
    assert!(anchors::export(&data, &scope, &at_c, &reset, &zip, true).unwrap().anchor_error.is_none());
    assert!(anchors::export(&data, &scope, &original, &stale, &zip, false).unwrap().anchor_error.is_some());
    assert_eq!(anchors::load(&data, &scope).unwrap().commits[""]["refs/heads/main"], b);
}

#[test]
fn exports_only_net_changes_from_locked_blobs_for_multiple_repositories() {
    assert!(engine::git_version().unwrap().contains("git version"));
    let temp = tempdir().unwrap();
    let root = temp.path().join("工作区");
    let server = root.join("组一/服务端");
    let client = root.join("客户端");
    init(&server);
    init(&client);
    put(&server, "config/app.bin", &[0, 1, 2]);
    put(&server, "src/旧 文件.txt", b"rename only");
    put(&server, "reverted.txt", b"original");
    put(&server, "delete.txt", b"deleted");
    let before = commit(&server, START - 1);

    put(&server, "config/app.bin", &[0, 255, 127, 13, 10, 0]);
    put(&server, "reverted.txt", b"temporary");
    fs::rename(server.join("src/旧 文件.txt"), server.join("src/新 文件.txt")).unwrap();
    fs::remove_file(server.join("delete.txt")).unwrap();
    commit(&server, START);
    put(&server, "reverted.txt", b"original");
    put(&server, "最后一秒.txt", b"end minute");
    let server_end = commit(&server, END - 1);
    put(&server, "too late.txt", b"outside range");
    commit(&server, END);

    put(&client, "config/app.bin", &[10, 0, 250, 88]);
    let client_end = commit(&client, START);
    let preview = scan(&root, START, END);
    assert_eq!(preview.repositories.len(), 2);
    assert_eq!(preview.file_count, 4);
    let server_result = preview.repositories.iter().find(|r| r.path == "组一/服务端").unwrap();
    assert_eq!(server_result.branch, "main");
    assert_eq!(server_result.start.as_deref(), Some(before.as_str()));
    assert_eq!(server_result.end.as_deref(), Some(server_end.as_str()));
    let statuses: Vec<_> = server_result.changes.iter().map(|c| {
        (c.kind.as_str(), c.path.as_str(), c.old_path.as_deref())
    }).collect();
    assert_eq!(statuses, [
        ("modified", "config/app.bin", None),
        ("deleted", "delete.txt", None),
        ("renamed", "src/新 文件.txt", Some("src/旧 文件.txt")),
        ("added", "最后一秒.txt", None),
    ]);

    git(&server, START, &["switch", "--detach", &before]);
    put(&server, "config/app.bin", b"dirty working tree");
    put(&client, "config/app.bin", b"staged version");
    git(&client, START, &["add", "config/app.bin"]);
    put(&client, "config/app.bin", b"unstaged version");
    let bundle = temp.path().join("bundle.zip");
    fs::write(&bundle, b"old archive").unwrap();
    assert_eq!(engine::export(&preview, &bundle).unwrap(), 4);
    let mut expected = BTreeMap::new();
    for (dir, prefix, rev, paths) in [
        (&server, "组一/服务端", &server_end, vec!["config/app.bin", "src/新 文件.txt", "最后一秒.txt"]),
        (&client, "客户端", &client_end, vec!["config/app.bin"]),
    ] {
        for name in paths {
            let blob = git(dir, START, &["show", &format!("{rev}:{name}")]);
            expected.insert(format!("{prefix}/{name}"), blob);
        }
    }
    assert_eq!(entries(&bundle), expected);
}

#[test]
fn counts_merge_changes_on_first_parent_and_exports_directly_selected_worktree() {
    let temp = tempdir().unwrap();
    let main = temp.path().join("main");
    init(&main);
    put(&main, "base", b"base");
    let base = commit(&main, START - 90);
    git(&main, START, &["switch", "-c", "topic"]);
    put(&main, "merged.txt", b"from side branch before the range");
    commit(&main, START - 60);
    git(&main, START, &["switch", "main"]);
    git(&main, START + 5, &["merge", "--no-ff", "topic", "-m", "Merge topic"]);
    let merge = head(&main);
    let worktree = temp.path().join("linked worktree");
    git(&main, START, &["worktree", "add", "--detach", worktree.to_str().unwrap(), &merge]);
    let preview = scan(&worktree, START, END);
    assert_eq!(preview.repositories.len(), 1);
    let repo = &preview.repositories[0];
    assert_eq!(repo.path, "");
    assert_eq!(repo.branch, "分离 HEAD");
    assert_eq!(repo.start.as_deref(), Some(base.as_str()));
    assert_eq!(repo.end.as_deref(), Some(merge.as_str()));
    let bundle = temp.path().join("merge.zip");
    engine::export(&preview, &bundle).unwrap();
    let expected = BTreeMap::from([("merged.txt".to_owned(), git(&main, START, &["show", "HEAD:merged.txt"]))]);
    assert_eq!(entries(&bundle), expected);
    assert_eq!(scan(&worktree, START - 65, START - 55).file_count, 0);
}

#[test]
fn handles_unborn_history_before_first_commit_and_delete_only_ranges() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("empty");
    init(&repo);
    let unborn = scan(&repo, START, END);
    assert_eq!(unborn.repositories.len(), 1);
    assert!(unborn.repositories[0].end.is_none());
    put(&repo, "removed", b"contents");
    let first = commit(&repo, START);
    let earlier = scan(&repo, START - 20, START);
    assert!(earlier.repositories[0].end.is_none());
    let initial = scan(&repo, START, END);
    assert!(initial.repositories[0].start.is_none());
    assert_eq!(initial.repositories[0].end.as_deref(), Some(first.as_str()));
    assert_eq!(initial.file_count, 1);
    fs::remove_file(repo.join("removed")).unwrap();
    commit(&repo, END);
    let deletion = scan(&repo, END, END + 60);
    assert_eq!(deletion.file_count, 0);
    assert_eq!(deletion.repositories[0].changes[0].kind, "deleted");
    let dest = temp.path().join("none.zip");
    assert!(engine::export(&deletion, &dest).unwrap_err().contains("没有可打包的文件"));
    assert!(!dest.exists());
}

#[test]
fn preserves_existing_destination_if_later_repository_disappears() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("projects");
    let repo = root.join("b");
    for dir in [root.join("a"), repo.clone()] {
        init(&dir);
        put(&dir, "file", b"data");
        commit(&dir, START);
    }
    let preview = scan(&root, START, END);
    fs::rename(&repo, temp.path().join("relocated")).unwrap();
    let output = temp.path().join("exports");
    fs::create_dir(&output).unwrap();
    let dest = output.join("bundle.zip");
    fs::write(&dest, b"previous archive").unwrap();
    let error = engine::export(&preview, &dest).unwrap_err();
    assert!(error.contains(&repo.display().to_string()));
    assert_eq!(fs::read(&dest).unwrap(), b"previous archive");
    let remaining: Vec<PathBuf> = fs::read_dir(output).unwrap().map(|e| e.unwrap().path()).collect();
    assert_eq!(remaining, [dest]);
}

#[test]
fn broken_nested_marker_is_reported_without_reusing_outer_repository() {
    let temp = tempdir().unwrap();
    init(temp.path());
    put(temp.path(), "outer.txt", b"outer");
    commit(temp.path(), START);
    let broken = temp.path().join("broken");
    fs::create_dir_all(broken.join(".git")).unwrap();
    let result = engine::scan(temp.path(), START, END).unwrap();
    assert_eq!(result.repositories.len(), 1);
    assert_eq!(result.repositories[0].path, "");
    assert_eq!(result.errors.len(), 1);
    assert!(result.errors[0].contains(&broken.display().to_string()));
}

fn detail(preview: &engine::Preview, path: &str) -> engine::FileDiff {
    let index = preview.repositories[0].changes.iter().position(|c| c.path == path).unwrap();
    engine::file_diff(preview, 0, index).unwrap()
}

#[test]
fn reads_both_locked_sides_for_text_rename_mode_and_missing_files() {
    let temp = tempdir().unwrap();
    let dir = temp.path();
    init(dir);
    put(dir, "代码.java", "class A {\r\n    int x = 1;\r\n}\r\n".as_bytes());
    put(dir, "旧.txt", b"renamed");
    put(dir, "delete.md", b"removed");
    put(dir, "build.sh", b"echo done");
    let first = commit(dir, START - 1);
    put(dir, "代码.java", "class A {\n    int x = 2;\n}".as_bytes());
    fs::rename(dir.join("旧.txt"), dir.join("新.txt")).unwrap();
    fs::remove_file(dir.join("delete.md")).unwrap();
    put(dir, "新增.txt", b"");
    git(dir, START, &["add", "-A"]);
    git(dir, START, &["update-index", "--chmod=+x", "build.sh"]);
    git(dir, START, &["commit", "-m", "Change"]);
    let preview = scan(dir, START, END);
    git(dir, START, &["switch", "--detach", &first]);
    put(dir, "代码.java", b"worktree");

    let code = detail(&preview, "代码.java");
    assert_eq!(code.state, "text");
    assert_eq!(code.before.as_deref(), Some("class A {\r\n    int x = 1;\r\n}\r\n"));
    assert_eq!(code.after.as_deref(), Some("class A {\n    int x = 2;\n}"));
    let added = detail(&preview, "新增.txt");
    assert_eq!((added.before_size, added.before), (None, None));
    assert_eq!((added.after_size, added.after.as_deref()), (Some(0), Some("")));
    let removed = detail(&preview, "delete.md");
    assert_eq!((removed.before.as_deref(), removed.after.as_deref()), (Some("removed"), None));
    let rename = detail(&preview, "新.txt");
    assert_eq!((rename.before.as_deref(), rename.after.as_deref()), (Some("renamed"), Some("renamed")));
    let mode = detail(&preview, "build.sh");
    assert_eq!((mode.before_mode.as_deref(), mode.after_mode.as_deref()), (Some("100644"), Some("100755")));
    assert_eq!(mode.before, mode.after);
    assert!(engine::file_diff(&preview, 1, 0).is_err());
    assert!(engine::file_diff(&preview, 0, usize::MAX).is_err());
}

#[test]
fn classifies_binary_large_and_encoded_files_before_rendering() {
    let temp = tempdir().unwrap();
    let dir = temp.path();
    init(dir);
    let utf16: Vec<u8> = [0xff, 0xfe].into_iter()
        .chain("标题\r\n".encode_utf16().flat_map(u16::to_le_bytes)).collect();
    put(dir, "encoded.txt", &utf16);
    commit(dir, START - 1);
    put(dir, "encoded.txt", "\u{feff}标题\r\n".as_bytes());
    put(dir, "data.bin", &[0, 17, 255, 0]);
    put(dir, "legacy.txt", &[0x81, 0x89, 0xbb]);
    put(dir, "large.txt", &vec![b'x'; 2 * 1024 * 1024 + 1]);
    commit(dir, START);
    let preview = scan(dir, START, END);
    let encoded = detail(&preview, "encoded.txt");
    assert_eq!(encoded.state, "text");
    assert_eq!(encoded.before_encoding, Some("UTF-16 LE"));
    assert_eq!(encoded.after_encoding, Some("UTF-8 BOM"));
    assert_eq!(encoded.before.as_deref(), Some("标题\r\n"));
    assert_eq!(encoded.after, encoded.before);
    for (path, state, size) in [("data.bin", "binary", 4), ("legacy.txt", "encoding", 3),
                                ("large.txt", "tooLarge", 2 * 1024 * 1024 + 1)] {
        let data = detail(&preview, path);
        assert_eq!(data.state, state);
        assert_eq!(data.after_size, Some(size));
        assert!(data.before.is_none() && data.after.is_none());
    }
}
