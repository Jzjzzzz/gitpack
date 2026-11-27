#[allow(dead_code)]
#[path = "../src/engine.rs"]
mod engine;
#[path = "../src/remote.rs"]
mod remote;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::{tempdir, TempDir};

const DAY: i64 = 1_700_000_000;

fn git(dir: &Path, args: &[&str], at: i64) -> String {
    let date = format!("@{at} +0000");
    let output = Command::new("git").current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", if cfg!(windows) { "NUL" } else { "/dev/null" })
        .env("GIT_AUTHOR_DATE", &date).env("GIT_COMMITTER_DATE", &date)
        .args(["-c", "user.name=Remote Test", "-c", "user.email=remote@example.invalid",
               "-c", "core.hooksPath=.git/no-hooks", "-c", "commit.gpgSign=false"])
        .args(args).output().unwrap();
    assert!(output.status.success(), "{args:?}: {}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

struct Fixture {
    tmp: TempDir,
    local: PathBuf,
    peer: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        git(root, &["init", "--bare", "-b", "release", "共享.git"], DAY);
        git(root, &["clone", "共享.git", "peer"], DAY);
        let peer = root.join("peer");
        commit(&peer, "base.txt", DAY);
        git(&peer, &["push", "-u", "origin", "HEAD:refs/heads/release"], DAY);
        git(root, &["clone", "-o", "team", "共享.git", "local"], DAY);
        let local = root.join("local");
        git(&local, &["branch", "-m", "delivery"], DAY);
        Self { tmp, local, peer }
    }

    fn preview(&self, cutoff: i64) -> engine::Repository {
        let mut result = engine::scan(&self.local, DAY - 1, cutoff).unwrap();
        assert!(result.errors.is_empty(), "{:?}", result.errors);
        result.repositories.pop().unwrap()
    }
}

fn commit(dir: &Path, name: &str, at: i64) {
    fs::write(dir.join(name), name).unwrap();
    git(dir, &["add", "--", name], at);
    git(dir, &["commit", "-m", name], at);
}

#[test]
fn reports_live_full_graph_counts_from_locked_local_tip_without_changing_the_checkout() {
    let f = Fixture::new();
    let locked = f.preview(DAY + 1);
    commit(&f.peer, "update.txt", DAY + 60);
    git(&f.peer, &["switch", "-c", "topic"], DAY + 70);
    commit(&f.peer, "topic.txt", DAY + 80);
    git(&f.peer, &["switch", "release"], DAY + 90);
    git(&f.peer, &["merge", "--no-ff", "topic", "-m", "merged"], DAY + 95);
    git(&f.peer, &["push", "origin", "release"], DAY + 96);
    commit(&f.local, "local.txt", DAY + 100);
    fs::write(f.local.join("dirty.txt"), b"working copy").unwrap();
    let refs_before = git(&f.local, &["show-ref"], DAY);
    let status_before = git(&f.local, &["status", "--porcelain"], DAY);
    let behind = remote::check(&locked);
    assert_eq!((behind.state.as_str(), behind.ahead, behind.behind), ("behind", 0, 3), "{:?}", behind.reason);
    assert_eq!(behind.upstream.as_deref(), Some("team/release"));
    let divergent = remote::check(&f.preview(DAY + 1));
    assert_eq!((divergent.state.as_str(), divergent.ahead, divergent.behind), ("diverged", 1, 3));
    assert_eq!(refs_before, git(&f.local, &["show-ref"], DAY));
    assert_eq!(status_before, git(&f.local, &["status", "--porcelain"], DAY));
    git(&f.local, &["fetch", "team"], DAY + 110);
    git(&f.local, &["merge", "--no-edit", "team/release"], DAY + 120);
    let ahead = remote::check(&f.preview(DAY + 1));
    assert_eq!((ahead.state.as_str(), ahead.ahead, ahead.behind), ("ahead", 2, 0));
    git(&f.local, &["push", "team", "HEAD:refs/heads/release"], DAY + 130);
    let synced = remote::check(&f.preview(DAY + 1));
    assert_eq!((synced.state.as_str(), synced.ahead, synced.behind), ("synced", 0, 0));
}

#[test]
fn reports_missing_branch_upstream_and_remote_separately_from_local_preview() {
    let f = Fixture::new();
    git(&f.local, &["branch", "--unset-upstream"], DAY);
    let no_tracking = remote::check(&f.preview(DAY + 1));
    assert_eq!(no_tracking.state, "unavailable");
    assert!(no_tracking.reason.unwrap().contains("上游"));
    git(&f.local, &["branch", "--set-upstream-to=team/release"], DAY);
    git(&f.local, &["config", "branch.delivery.merge", "refs/heads/missing"], DAY);
    let gone = remote::check(&f.preview(DAY + 1));
    assert_eq!(gone.state, "unavailable");
    assert!(gone.reason.is_some());
    let missing = f.tmp.path().join("offline.git");
    git(&f.local, &["remote", "set-url", "team", missing.to_str().unwrap()], DAY);
    let offline = remote::check(&f.preview(DAY + 1));
    assert_eq!(offline.state, "unavailable");
    assert!(offline.reason.is_some());
    git(&f.local, &["switch", "--detach"], DAY);
    let detached = remote::check(&f.preview(DAY + 1));
    assert_eq!(detached.state, "unavailable");
    assert!(detached.reason.unwrap().contains("HEAD"));
}

#[test]
fn stops_checking_an_unresponsive_remote_within_the_repo_deadline() {
    use std::net::TcpListener;
    use std::time::{Duration, Instant};
    let f = Fixture::new();
    // Hold the TCP port open without replying to Git's protocol handshake.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("git://{}/slow.git", listener.local_addr().unwrap());
    git(&f.local, &["remote", "set-url", "team", &url], DAY);
    let preview = f.preview(DAY + 1);
    let start = Instant::now();
    let status = remote::check(&preview);
    assert_eq!(status.state, "unavailable");
    assert!(status.reason.unwrap().contains("超时"));
    assert!(start.elapsed() < Duration::from_secs(35));
}
