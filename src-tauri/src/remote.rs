use crate::engine::{self, Repository};
use serde::Serialize;
use std::fs;
use std::io::{Read, Seek};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Debug, Serialize)]
pub struct RemoteStatus {
    pub state: String,
    pub upstream: Option<String>,
    pub ahead: u64,
    pub behind: u64,
    pub reason: Option<String>,
}

pub fn check(repo: &Repository) -> RemoteStatus {
    let mut upstream = None;
    match compare(repo, &mut upstream, Instant::now() + Duration::from_secs(20)) {
        Ok((ahead, behind)) => {
            let state = match (ahead, behind) {
                (0, 0) => "synced",
                (_, 0) => "ahead",
                (0, _) => "behind",
                _ => "diverged",
            };
            RemoteStatus { state: state.into(), upstream, ahead, behind, reason: None }
        }
        Err(reason) => RemoteStatus {
            state: "unavailable".into(), upstream, ahead: 0, behind: 0, reason: Some(reason),
        },
    }
}

fn compare(repo: &Repository, upstream: &mut Option<String>, deadline: Instant) -> Result<(u64, u64), String> {
    let branch = repo.branch_key.strip_prefix("refs/heads/").ok_or("当前为分离 HEAD，未关联上游分支")?;
    let local = repo.scanned_tip.as_deref().ok_or("本地分支尚无提交")?;
    let origin = || engine::git(Some(&repo.directory));
    let config = |field: &str| -> Result<String, String> {
        let key = format!("branch.{branch}.{field}");
        let out = run(origin().args(["config", "--get", &key]), deadline)?;
        if out.status.code() == Some(1) { return Err("当前分支未配置上游".into()); }
        out.text()
    };
    let remote = config("remote")?;
    let merge = config("merge")?;
    if remote == "." { return Err("上游指向本地分支".into()); }
    let branch_name = merge.strip_prefix("refs/heads/").ok_or("上游不是远程分支")?;
    if !run(origin().args(["check-ref-format", &merge]), deadline)?.status.success() {
        return Err("上游分支配置无效".into());
    }
    let remotes = run(origin().arg("remote"), deadline)?.text()?;
    if !remotes.lines().any(|name| name == remote) { return Err("找不到分支配置的远程仓库".into()); }
    *upstream = Some(format!("{remote}/{branch_name}"));
    let url = run(origin().args(["remote", "get-url", "--", &remote]), deadline)?.text()?;
    let url = resolve_url(&url, &repo.directory);

    // An isolated bare repo inherits user credentials, never repository hooks/helpers.
    // Borrow existing objects read-only and keep all downloaded objects and refs temporary.
    let scratch = tempfile::tempdir().map_err(|_| "无法创建远程检查临时目录")?;
    let path = scratch.path();
    let temporary = || {
        let mut cmd = engine::git(None);
        cmd.current_dir(path).arg("--git-dir").arg(path)
            .env("GIT_ALLOW_PROTOCOL", "https:http:ssh:git:file")
            .env("GIT_ASKPASS", "").env("SSH_ASKPASS_REQUIRE", "never")
            .env("GCM_INTERACTIVE", "never").env("GIT_NO_LAZY_FETCH", "1")
            .env("GIT_SSH_COMMAND", "ssh -o BatchMode=yes -o StrictHostKeyChecking=yes")
            .args(["-c", "credential.interactive=false", "-c", "core.askPass=", "-c", "core.hooksPath="]);
        cmd
    };
    let format = if local.len() == 64 { "--object-format=sha256" } else { "--object-format=sha1" };
    run(temporary().args(["init", "--quiet", "--bare", "--template=", format]), deadline)?.text()?;
    let advertised = run(temporary().args(["ls-remote", "--exit-code", "--refs", "--", &url, &merge]), deadline)?;
    if advertised.status.code() == Some(2) { return Err("远程上游分支不存在".into()); }
    let refs = advertised.text()?;
    let tip = refs.lines().filter_map(|s| s.split_once('\t'))
        .find_map(|(oid, name)| (name == merge && oid.len() == local.len() && oid.bytes().all(|b| b.is_ascii_hexdigit())).then_some(oid))
        .ok_or("未能确定远程上游分支的提交号")?;
    if tip == local { return Ok((0, 0)); }
    if run(origin().args(["rev-parse", "--is-shallow-repository"]), deadline)?.text()? == "true" {
        return Err("浅克隆的提交历史不完整，无法核实提交数".into());
    }
    let objects = run(origin().args(["rev-parse", "--path-format=absolute", "--git-path", "objects"]), deadline)?.text()?;
    if objects.contains(['\n','\r']) { return Err("仓库对象目录路径无法用于远程检查".into()); }
    fs::write(path.join("objects/info/alternates"), format!("{objects}\n")).map_err(|_| "无法引用本地 Git 对象目录")?;
    let exists = run(temporary().args(["cat-file", "-e", &format!("{tip}^{{commit}}")]), deadline)?;
    let remote_tip = if exists.status.success() {
        tip.to_owned()
    } else {
        run(temporary().args(["update-ref", "refs/heads/gitpack-local", local]), deadline)?.text()?;
        let refspec = format!("{merge}:refs/gitpack/upstream");
        run(temporary().args(["fetch", "--quiet", "--no-tags", "--no-prune", "--no-auto-gc",
            "--no-recurse-submodules", "--no-write-fetch-head", "--refmap=", "--", &url, &refspec]), deadline)?.text()?;
        run(temporary().args(["rev-parse", "--verify", "refs/gitpack/upstream^{commit}"]), deadline)?.text()?
    };
    let range = format!("{local}...{remote_tip}");
    let counts = run(temporary().args(["rev-list", "--left-right", "--count", &range, "--"]), deadline)?.text()?;
    let (left, right) = counts.split_once('\t').ok_or("Git 返回了无法识别的提交数")?;
    Ok((left.parse().map_err(|_| "本地提交数无效")?, right.parse().map_err(|_| "远程提交数无效")?))
}

fn resolve_url(url: &str, repo: &Path) -> String {
    // Keep SSH's scp-like syntax; resolve ordinary relative filesystem remotes
    // from the selected worktree instead of the temporary bare repository.
    let transport = url.split_once(':').is_some_and(|(prefix, _)| !prefix.contains(['/', '\\']));
    if transport || Path::new(url).is_absolute() { url.to_owned() }
    else { repo.join(url).to_string_lossy().into_owned() }
}

struct Reply { status: ExitStatus, stdout: String, stderr: String }

impl Reply {
    fn text(self) -> Result<String, String> {
        if self.status.success() { return Ok(self.stdout.trim_end_matches(['\n', '\r']).to_owned()); }
        // Git diagnostics can include credential-bearing URLs or HTTP headers.
        let detail = self.stderr.to_ascii_lowercase();
        let reason = if detail.contains("permission denied") || detail.contains("authentication")
            || detail.contains("could not read username") || detail.contains("401") || detail.contains("403")
            || detail.contains("terminal prompts disabled") {
            "远程认证失败或访问被拒绝"
        } else if detail.contains("host key verification") { "SSH 主机密钥未通过验证" }
        else if detail.contains("couldn't find remote ref") { "远程上游分支不存在" }
        else if detail.contains("transport") && detail.contains("not allowed") { "远程地址使用了不支持的传输协议" }
        else if detail.contains("timed out") { "远程连接超时" }
        else if detail.contains("resolve host") || detail.contains("resolve hostname") { "无法解析远程主机名" }
        else if detail.contains("ssl") || detail.contains("certificate") { "远程 TLS 连接或证书验证失败" }
        else if detail.contains("connect") || detail.contains("repository") || detail.contains("remote") { "无法访问远程仓库，请检查地址、权限或网络连接" }
        else { "Git 未能核实远程提交状态" };
        Err(reason.into())
    }
}

fn run(cmd: &mut Command, deadline: Instant) -> Result<Reply, String> {
    let timeout = "远程检查超时（20 秒）";
    if Instant::now() >= deadline { return Err(timeout.into()); }
    // File-backed pipes avoid blocking on large Git output or credential-helper children.
    let io = |e: std::io::Error| format!("远程检查的本地 I/O 失败：{e}");
    let mut stdout = tempfile::tempfile().map_err(io)?;
    let mut stderr = tempfile::tempfile().map_err(io)?;
    cmd.stdin(Stdio::null()).stdout(stdout.try_clone().map_err(io)?).stderr(stderr.try_clone().map_err(io)?);
    let mut child = cmd.spawn().map_err(io)?;
    let status = loop {
        if let Some(status) = child.try_wait().map_err(io)? { break status; }
        if Instant::now() >= deadline {
            terminate(&mut child);
            return Err(timeout.into());
        }
        thread::sleep(Duration::from_millis(15));
    };
    let read = |file: &mut fs::File| -> Result<String, String> {
        file.rewind().map_err(io)?;
        let mut bytes = Vec::new();
        file.take(65536).read_to_end(&mut bytes).map_err(io)?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    };
    Ok(Reply { status, stdout: read(&mut stdout)?, stderr: read(&mut stderr)? })
}

fn terminate(child: &mut Child) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        if let Some(system) = std::env::var_os("SystemRoot") {
            let _ = Command::new(Path::new(&system).join("System32/taskkill.exe"))
                .creation_flags(0x08000000).args(["/F", "/T", "/PID", &child.id().to_string()])
                .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status();
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}
