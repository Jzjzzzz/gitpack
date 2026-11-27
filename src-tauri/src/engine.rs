use serde::Serialize;
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use tempfile::NamedTempFile;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Preview {
    pub repositories: Vec<Repository>,
    pub errors: Vec<String>,
    pub file_count: usize,
}

/// Repository-relative path -> full branch ref (or detached HEAD) -> commit OID.
pub type AnchorMap = BTreeMap<String, BTreeMap<String, String>>;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Repository {
    pub path: String,
    pub branch: String,
    pub start: Option<String>,
    pub end: Option<String>,
    pub changes: Vec<Change>,
    pub rebuild_anchor: bool,
    #[serde(skip)]
    pub branch_key: String,
    #[serde(skip)]
    pub scanned_anchor: Option<String>,
    #[serde(skip)]
    pub scanned_tip: Option<String>,
    #[serde(skip)]
    pub directory: PathBuf,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Change {
    pub kind: String,
    pub path: String,
    pub old_path: Option<String>,
    #[serde(skip)]
    old_blob: Option<String>,
    #[serde(skip)]
    blob: Option<String>,
    #[serde(skip)]
    old_mode: Option<String>,
    #[serde(skip)]
    new_mode: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileDiff {
    pub state: &'static str,
    pub before: Option<String>,
    pub after: Option<String>,
    pub before_size: Option<u64>,
    pub after_size: Option<u64>,
    pub before_encoding: Option<&'static str>,
    pub after_encoding: Option<&'static str>,
    pub before_mode: Option<String>,
    pub after_mode: Option<String>,
}

pub(crate) fn git(repo: Option<&Path>) -> Command {
    let mut cmd = Command::new("git");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    for key in ["GIT_DIR", "GIT_WORK_TREE", "GIT_COMMON_DIR", "GIT_INDEX_FILE",
                "GIT_OBJECT_DIRECTORY", "GIT_ALTERNATE_OBJECT_DIRECTORIES", "GIT_NAMESPACE"] {
        cmd.env_remove(key);
    }
    cmd.env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("LC_ALL", "C")
        .stdin(Stdio::null());
    if let Some(path) = repo {
        // Bind Git to this exact marker, including .git files in linked worktrees.
        // A broken marker must never cause Git to discover a parent repository.
        cmd.arg("-C").arg(path).arg("--git-dir=.git");
    }
    cmd
}

fn checked(output: Output) -> Result<Vec<u8>, String> {
    if output.status.success() {
        Ok(output.stdout)
    } else {
        let detail = String::from_utf8_lossy(&output.stderr);
        Err(format!("Git 执行失败（{}） {}", output.status, detail.trim()))
    }
}

fn run(repo: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    checked(git(Some(repo)).args(args).output().map_err(|e| e.to_string())?)
}

fn utf8(bytes: &[u8]) -> Result<&str, String> {
    std::str::from_utf8(bytes).map_err(|_| "Git 路径或输出包含无法表示为 UTF-8 的数据".into())
}

pub fn git_version() -> Result<String, String> {
    let output = git(None).arg("--version").output()
        .map_err(|e| format!("无法启动 Git，请安装 Git for Windows 并确保 git 在 PATH 中。{e}"))?;
    Ok(utf8(&checked(output)?)?.trim().to_owned())
}

pub fn scan(root: &Path, start: i64, end_exclusive: i64) -> Result<Preview, String> {
    scan_with_anchors(root, Some(start), Some(end_exclusive), &AnchorMap::new())
}

pub fn scan_with_anchors(
    root: &Path, start: Option<i64>, end_exclusive: Option<i64>, anchors: &AnchorMap,
) -> Result<Preview, String> {
    if let Some(start) = start {
        let end = end_exclusive.ok_or("按日期打包时请选择结束时间")?;
        if start >= end {
            return Err("开始时间必须早于范围结束时间".into());
        }
    }
    let root = std::path::absolute(root).map_err(|e| e.to_string())?;
    if !root.is_dir() {
        return Err(format!("目录不存在或无法读取：{}", root.display()));
    }
    let mut preview = Preview { repositories: Vec::new(), errors: Vec::new(), file_count: 0 };
    let mut pending = vec![root.clone()];
    while let Some(dir) = pending.pop() {
        let mut is_repo = false;
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) => { preview.errors.push(format!("{}：{e}", dir.display())); continue; }
        };
        for item in entries {
            let entry = match item {
                Ok(entry) => entry,
                Err(e) => { preview.errors.push(format!("{}：{e}", dir.display())); continue; }
            };
            if entry.file_name() == ".git" {
                is_repo = true;
                continue;
            }
            // Junctions can introduce cycles on Windows, just like symlinked directories.
            match entry.metadata() {
                Ok(meta) if descend(&meta) => pending.push(entry.path()),
                Ok(_) => {}
                Err(e) => preview.errors.push(format!("{}：{e}", entry.path().display())),
            }
        }
        if is_repo {
            match scan_repo(&dir, &root, start, end_exclusive, anchors) {
                Ok(repo) => {
                    preview.file_count += repo.changes.iter().filter(|c| c.blob.is_some()).count();
                    preview.repositories.push(repo);
                }
                Err(e) => preview.errors.push(format!("{}：{e}", dir.display())),
            }
        }
    }
    preview.repositories.sort_by(|a, b| a.path.cmp(&b.path));
    preview.errors.sort();
    Ok(preview)
}

fn descend(meta: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if meta.file_attributes() & 0x400 != 0 {
            return false;
        }
    }
    meta.is_dir() && !meta.file_type().is_symlink()
}

fn scan_repo(dir: &Path, root: &Path, start: Option<i64>, end: Option<i64>, anchors: &AnchorMap) -> Result<Repository, String> {
    let symbolic = git(Some(dir)).args(["symbolic-ref", "--quiet", "HEAD"]).output()
        .map_err(|e| e.to_string())?;
    let branch_ref = if symbolic.status.code() == Some(1) {
        None
    } else {
        Some(utf8(&checked(symbolic)?)?.trim_end().to_string())
    };
    let reference = branch_ref.as_deref().unwrap_or("HEAD");
    let head_result = git(Some(dir)).args(["rev-parse", "--verify", "--quiet", reference])
        .output().map_err(|e| e.to_string())?;
    let mut result = Repository {
        path: dir.strip_prefix(root).map_err(|e| e.to_string())?.components()
            .map(|c| c.as_os_str().to_str().ok_or("目录名称无法转换为 UTF-8"))
            .collect::<Result<Vec<_>, _>>()?.join("/"),
        branch: branch_ref.as_deref().map(|r| r.strip_prefix("refs/heads/").unwrap_or(r))
            .unwrap_or("分离 HEAD").into(),
        start: None, end: None, changes: Vec::new(), directory: dir.into(),
        branch_key: reference.into(), scanned_anchor: None, scanned_tip: None, rebuild_anchor: false,
    };
    result.scanned_anchor = anchors.get(&result.path).and_then(|branches| branches.get(reference)).cloned();
    // An unborn branch is a valid, empty repository.
    if head_result.status.code() == Some(1) && branch_ref.is_some() && head_result.stderr.is_empty() {
        return Ok(result);
    }
    let head_output = checked(head_result)?;
    let tip = utf8(&head_output)?.trim();
    result.scanned_tip = Some(tip.into());
    result.end = match end {
        Some(limit) => snapshot_before(dir, tip, limit)?,
        None => Some(tip.into()),
    };
    if let Some(anchor) = result.scanned_anchor.as_deref() {
        if !on_first_parent_chain(dir, anchor, tip)? {
            if is_shallow(dir)? {
                return Err(format!("分支 {} 的浅克隆历史不足以核实锚点 {anchor}，请补全历史后重新预览", result.branch));
            }
            if start.is_none() {
                return Err(format!("分支 {} 的锚点 {anchor} 已脱离当前第一父链，请按日期预览并确认重建锚点", result.branch));
            }
            result.rebuild_anchor = result.end.is_some();
        }
    }
    if let Some(start_time) = start {
        if let Some(oid) = &result.end {
            result.start = snapshot_before(dir, oid, start_time)?;
        }
        if result.start.is_none() && is_shallow(dir)? {
            return Err("浅克隆的历史不足以确定开始快照，请补全历史后重新预览".into());
        }
    } else {
        let anchor = result.scanned_anchor.as_ref()
            .ok_or_else(|| format!("分支 {} 尚无锚点，请先按日期成功导出一次 ZIP", result.branch))?;
        let end_contains_anchor = match &result.end {
            Some(oid) if oid == tip => true, // Already verified against this exact tip above.
            Some(oid) => on_first_parent_chain(dir, anchor, oid)?,
            None => false,
        };
        if !end_contains_anchor {
            return Err(format!("分支 {} 的截止时间早于上次打包的锚点 {anchor}，请选择更晚的截止时间", result.branch));
        }
        result.start = Some(anchor.clone());
    }
    if let Some(end_oid) = result.end.as_ref() {
        if result.start.as_ref() != Some(end_oid) {
            // Git recognizes its object-format-specific empty tree without writing to the repo.
            let empty_tree;
            let base = match &result.start {
                Some(oid) => oid.as_str(),
                None => {
                    empty_tree = run(dir, &["hash-object", "-t", "tree", "--stdin"])?;
                    utf8(&empty_tree)?.trim()
                }
            };
            let diff = run(dir, &["diff-tree", "--no-commit-id", "-r", "--raw", "--no-abbrev",
                "-z", "--no-ext-diff", "--no-textconv", "--ignore-submodules=none",
                "--find-renames", base, end_oid, "--"])?;
            result.changes = parse_diff(&diff)?;
        }
    }
    Ok(result)
}

fn is_shallow(dir: &Path) -> Result<bool, String> {
    Ok(utf8(&run(dir, &["rev-parse", "--is-shallow-repository"])?)?.trim() == "true")
}

pub fn on_first_parent_chain(dir: &Path, ancestor: &str, tip: &str) -> Result<bool, String> {
    for oid in [ancestor, tip] {
        if !matches!(oid.len(), 40 | 64) || !oid.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("锚点或提交号不是完整的 Git 对象号".into());
        }
    }
    if ancestor == tip {
        return Ok(true);
    }
    let history = run(dir, &["rev-list", "--first-parent", tip, "--"])?;
    Ok(history.split(|b| *b == b'\n').any(|oid| oid.eq_ignore_ascii_case(ancestor.as_bytes())))
}

fn snapshot_before(dir: &Path, tip: &str, exclusive: i64) -> Result<Option<String>, String> {
    if exclusive <= 0 {
        return Ok(None);
    }
    // --min-age is the epoch-seconds form of --until, inclusive of that second.
    let limit = format!("--min-age={}", exclusive - 1);
    let bytes = run(dir, &["rev-list", "-1", "--first-parent", &limit, tip, "--"])?;
    let oid = utf8(&bytes)?.trim();
    Ok((!oid.is_empty()).then(|| oid.to_owned()))
}

fn parse_diff(bytes: &[u8]) -> Result<Vec<Change>, String> {
    let mut fields = bytes.split(|&b| b == 0);
    let mut changes = Vec::new();
    while let Some(header) = fields.next() {
        if header.is_empty() { break; }
        let header = utf8(header)?.strip_prefix(':').ok_or("Git 差异格式异常")?;
        let parts: Vec<_> = header.split_ascii_whitespace().collect();
        let [old_mode, new_mode, old_blob, new_blob, status] = parts.as_slice() else {
            return Err("Git 差异字段异常".into());
        };
        let mut next_path = || -> Result<String, String> {
            Ok(utf8(fields.next().ok_or("Git 缺少文件路径")?)?.to_owned())
        };
        let first = next_path()?;
        let (mut kind, path, old_path) = match status.as_bytes().first() {
            Some(b'R') => ("renamed", next_path()?, Some(first)),
            Some(b'A') => ("added", first, None),
            Some(b'D') => ("deleted", first, None),
            Some(b'M' | b'T') => ("modified", first, None),
            _ => return Err(format!("未知的 Git 差异类型 {status}")),
        };
        // A gitlink represents a directory. Checked-out submodules are scanned independently.
        if *new_mode == "160000" || *old_mode == "160000" {
            match (*old_mode, *new_mode) {
                ("160000", "160000" | "000000") | ("000000", "160000") => continue,
                (_, "160000") => kind = "deleted",
                ("160000", _) => kind = "added",
                _ => {}
            }
        }
        changes.push(Change {
            kind: kind.into(), path, old_path,
            old_blob: (kind != "added").then(|| old_blob.to_string()),
            blob: (kind != "deleted").then(|| new_blob.to_string()),
            old_mode: (kind != "added").then(|| old_mode.to_string()),
            new_mode: (kind != "deleted").then(|| new_mode.to_string()),
        });
    }
    changes.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(changes)
}

fn entry_name(repo: &Repository, change: &Change) -> String {
    if repo.path.is_empty() { change.path.clone() } else { format!("{}/{}", repo.path, change.path) }
}

const MAX_DIFF_BYTES: u64 = 2 * 1024 * 1024;

pub fn file_diff(preview: &Preview, repo_index: usize, change_index: usize) -> Result<FileDiff, String> {
    let repo = preview.repositories.get(repo_index).ok_or("找不到该仓库，请重新预览。")?;
    let change = repo.changes.get(change_index).ok_or("找不到该文件变更，请重新预览。")?;
    let read = |operation: &str, oid: &str| {
        run(&repo.directory, &["cat-file", operation, oid])
            .map_err(|e| format!("读取 {} 的快照失败：{e}", entry_name(repo, change)))
    };
    let size = |oid: &str| -> Result<u64, String> {
        utf8(&read("-s", oid)?)?.trim().parse().map_err(|_| "无法读取 Git 文件大小".into())
    };
    let before_size = change.old_blob.as_deref().map(size).transpose()?;
    let after_size = change.blob.as_deref().map(size).transpose()?;
    let mut result = FileDiff {
        state: "text", before: None, after: None, before_size, after_size,
        before_encoding: None, after_encoding: None,
        before_mode: change.old_mode.clone(), after_mode: change.new_mode.clone(),
    };
    if before_size.max(after_size).is_some_and(|bytes| bytes > MAX_DIFF_BYTES) {
        result.state = "tooLarge";
        return Ok(result);
    }
    for (oid, text, encoding) in [
        (&change.old_blob, &mut result.before, &mut result.before_encoding),
        (&change.blob, &mut result.after, &mut result.after_encoding),
    ] {
        if let Some(oid) = oid {
            match decode_text(read("blob", oid)?) {
                Ok((value, name)) => {
                    *text = Some(value);
                    *encoding = Some(name);
                }
                Err(state) => {
                    result.state = state;
                    break;
                }
            }
        }
    }
    if result.state != "text" {
        result.before = None;
        result.after = None;
    }
    Ok(result)
}

fn decode_text(bytes: Vec<u8>) -> Result<(String, &'static str), &'static str> {
    let (text, encoding) = if bytes.starts_with(&[0xff, 0xfe]) || bytes.starts_with(&[0xfe, 0xff]) {
        let little_endian = bytes[0] == 0xff;
        let units = bytes[2..].chunks_exact(2);
        if !units.remainder().is_empty() {
            return Err("encoding");
        }
        let units = units.map(|b| {
            if little_endian { u16::from_le_bytes([b[0], b[1]]) } else { u16::from_be_bytes([b[0], b[1]]) }
        });
        let decoded = char::decode_utf16(units).collect::<Result<String, _>>().map_err(|_| "encoding")?;
        (decoded, if little_endian { "UTF-16 LE" } else { "UTF-16 BE" })
    } else {
        if bytes.contains(&0) {
            return Err("binary");
        }
        let mut text = String::from_utf8(bytes).map_err(|_| "encoding")?;
        if text.starts_with('\u{feff}') {
            text.drain(..'\u{feff}'.len_utf8());
            (text, "UTF-8 BOM")
        } else {
            (text, "UTF-8")
        }
    };
    if text.chars().any(|c| c.is_ascii_control() && !matches!(c, '\t' | '\n' | '\r' | '\x0c')) {
        return Err("binary");
    }
    Ok((text, encoding))
}

fn validate_entries(preview: &Preview) -> Result<(), String> {
    let mut files = HashSet::new();
    let mut directories = HashSet::new();
    for repo in &preview.repositories {
        for change in repo.changes.iter().filter(|c| c.blob.is_some()) {
            let name = entry_name(repo, change);
            for component in name.split('/') {
                let stem = component.split('.').next().unwrap_or("").trim_end().to_ascii_uppercase();
                if component.is_empty() || component.ends_with([' ', '.'])
                    || component.chars().any(|c| c.is_control() || r#"\:*?"<>|"#.contains(c))
                    || component.eq_ignore_ascii_case(".git")
                    || matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$")
                    || stem.strip_prefix("COM").or_else(|| stem.strip_prefix("LPT"))
                        .is_some_and(|n| matches!(n, "1"|"2"|"3"|"4"|"5"|"6"|"7"|"8"|"9"|"¹"|"²"|"³"))
                {
                    return Err(format!("路径无法在 Windows 安全解压：{name}"));
                }
            }
            let key = name.to_lowercase();
            if directories.contains(&key) || !files.insert(key.clone()) {
                return Err(format!("同一 ZIP 路径对应多个文件或目录：{name}"));
            }
            for (i, _) in key.match_indices('/') {
                if files.contains(&key[..i]) {
                    return Err(format!("ZIP 路径的上级目录与文件重名：{name}"));
                }
                directories.insert(key[..i].to_owned());
            }
        }
    }
    Ok(())
}

pub fn export(preview: &Preview, dest: &Path) -> Result<usize, String> {
    if !preview.errors.is_empty() {
        return Err("预览包含读取错误，请处理后重新预览".into());
    }
    if preview.file_count == 0 {
        return Err("没有可打包的文件".into());
    }
    validate_entries(preview)?;
    let dest = std::path::absolute(dest).map_err(|e| e.to_string())?;
    let mut temp = NamedTempFile::new_in(dest.parent().ok_or("保存路径缺少目录")?)
        .map_err(|e| format!("无法创建临时 ZIP：{e}"))?;
    {
        let mut zip = ZipWriter::new(temp.as_file_mut());
        for repo in &preview.repositories {
            if repo.changes.iter().any(|c| c.blob.is_some()) {
                write_repo(repo, &mut zip).map_err(|e| format!("{}：{e}", repo.directory.display()))?;
            }
        }
        zip.finish().map_err(|e| format!("写入 ZIP 索引失败：{e}"))?;
    }
    temp.as_file().sync_all().map_err(|e| format!("保存 ZIP 失败：{e}"))?;
    temp.persist(&dest).map_err(|e| format!("无法保存到 {}：{}", dest.display(), e.error))?;
    Ok(preview.file_count)
}

fn write_repo<W: Write + Seek>(repo: &Repository, zip: &mut ZipWriter<W>) -> Result<(), String> {
    // One streaming cat-file process per repository, even for large or numerous blobs.
    let mut errors = tempfile::tempfile().map_err(|e| e.to_string())?;
    let mut child = git(Some(&repo.directory)).args(["cat-file", "--batch"])
        .stdin(Stdio::piped()).stdout(Stdio::piped())
        .stderr(errors.try_clone().map_err(|e| e.to_string())?)
        .spawn().map_err(|e| e.to_string())?;
    let result = (|| {
        let mut input = child.stdin.take().ok_or("无法打开 Git 输入流")?;
        let mut output = BufReader::new(child.stdout.take().ok_or("无法打开 Git 输出流")?);
        for change in &repo.changes {
            let Some(oid) = &change.blob else { continue };
            let name = entry_name(repo, change);
            let read_error = |e: io::Error| format!("读取或写入 {name} 时失败：{e}");
            writeln!(input, "{oid}").and_then(|_| input.flush()).map_err(read_error)?;
            let mut header = String::new();
            output.read_line(&mut header).map_err(read_error)?;
            let fields: Vec<_> = header.split_ascii_whitespace().collect();
            if fields.len() != 3 || fields[0] != oid || fields[1] != "blob" {
                return Err(format!("无法读取快照文件 {name}（{oid}）"));
            }
            let size = fields[2].parse::<u64>().map_err(|e| e.to_string())?;
            let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated)
                .unix_permissions(if change.new_mode.as_deref() == Some("100755") { 0o755 } else { 0o644 })
                .large_file(size >= 0xffff_ffff - (1 << 24));
            zip.start_file(&name, options).map_err(|e| format!("{name}：{e}"))?;
            let copied = io::copy(&mut output.by_ref().take(size), zip).map_err(read_error)?;
            let mut trailer = [0; 1];
            output.read_exact(&mut trailer).map_err(read_error)?;
            if copied != size || trailer[0] != b'\n' {
                return Err(format!("Git 返回的文件数据不完整：{name}"));
            }
        }
        Ok(())
    })();
    if result.is_err() { let _ = child.kill(); }
    let status = child.wait().map_err(|e| e.to_string())?;
    if !status.success() || result.is_err() {
        errors.rewind().map_err(|e| e.to_string())?;
        let mut detail = String::new();
        let _ = errors.take(8192).read_to_string(&mut detail);
        return Err(format!("{} {}", result.err().unwrap_or_else(|| format!("Git 退出：{status}")), detail.trim()));
    }
    Ok(())
}
