use crate::engine::{self, AnchorMap, Preview};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::ErrorKind;
use std::path::Path;
use tempfile::NamedTempFile;

// The outer key identifies the selected packaging directory, including for single-repo exports.
type AllAnchors = BTreeMap<String, ScopeAnchors>;

#[derive(Default, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScopeAnchors {
    pub commits: AnchorMap,
    resets: BTreeMap<String, BTreeMap<String, u64>>,
}

impl ScopeAnchors {
    fn generation(&self, repo: &engine::Repository) -> u64 {
        self.resets.get(&repo.path).and_then(|branches| branches.get(&repo.branch_key))
            .copied().unwrap_or(0)
    }
}

pub fn scope_key(root: &Path) -> Result<String, String> {
    fs::canonicalize(root).map_err(|e| format!("无法识别目录 {}：{e}", root.display()))?
        .into_os_string().into_string().map_err(|_| "打包目录名称无法转换为 UTF-8".into())
}

fn read_all(data: &Path) -> Result<AllAnchors, String> {
    let path = data.join("anchors-v1.json");
    match File::open(&path) {
        Ok(file) => serde_json::from_reader(file)
            .map_err(|e| format!("锚点记录无法解析（{}）：{e}", path.display())),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(AllAnchors::new()),
        Err(e) => Err(format!("无法读取锚点（{}）：{e}", path.display())),
    }
}

pub fn load(data: &Path, scope: &str) -> Result<ScopeAnchors, String> {
    Ok(read_all(data)?.remove(scope).unwrap_or_default())
}

fn update(data: &Path, scope: &str, observed: &ScopeAnchors, preview: &Preview, confirm_rebuild: bool) -> Result<(), String> {
    fs::create_dir_all(data).map_err(|e| format!("无法创建应用数据目录：{e}"))?;
    // Lock a stable sibling: the JSON file itself is replaced by rename on every update.
    let lock = OpenOptions::new().read(true).write(true).create(true).truncate(false)
        .open(data.join("anchors-v1.lock")).map_err(|e| format!("无法打开锚点锁：{e}"))?;
    lock.lock().map_err(|e| format!("无法锁定锚点：{e}"))?;
    let mut all = read_all(data)?;
    let saved = all.entry(scope.to_owned()).or_default();
    let mut changed = false;
    for repo in &preview.repositories {
        let Some(end) = repo.end.as_deref() else { continue };
        let generation = saved.generation(repo);
        let branches = saved.commits.entry(repo.path.clone()).or_default();
        let current = branches.get(&repo.branch_key).map(String::as_str);
        if current == Some(end) { continue }

        let conflict = || format!("{}（{}）的锚点已变化，请重新预览。", repo.directory.display(), repo.branch);
        // An old preview cannot reintroduce commits abandoned by an explicit history reset,
        // even when the new anchor has exactly the same OID as the old preview's baseline.
        if generation != observed.generation(repo) { return Err(conflict()) }
        let confirmed_reset = repo.rebuild_anchor && confirm_rebuild && current == repo.scanned_anchor.as_deref();
        if confirmed_reset {
            saved.resets.entry(repo.path.clone()).or_default().insert(repo.branch_key.clone(), generation + 1);
        } else {
            if let Some(old) = current {
                if !engine::on_first_parent_chain(&repo.directory, old, end)? {
                    if engine::on_first_parent_chain(&repo.directory, end, old)? { continue }
                    return Err(conflict());
                }
            } else if repo.scanned_anchor.is_some() {
                return Err(conflict());
            }
        }
        branches.insert(repo.branch_key.clone(), end.to_owned());
        changed = true;
    }
    if changed {
        let file = NamedTempFile::new_in(data).map_err(|e| format!("无法创建锚点临时文件：{e}"))?;
        serde_json::to_writer(file.as_file(), &all).map_err(|e| format!("写入锚点失败：{e}"))?;
        file.as_file().sync_all().map_err(|e| format!("保存锚点失败：{e}"))?;
        file.persist(data.join("anchors-v1.json")).map_err(|e| format!("替换锚点文件失败：{}", e.error))?;
    }
    Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportResult {
    pub file_count: usize,
    pub anchor_error: Option<String>,
}

pub fn export(data: &Path, scope: &str, observed: &ScopeAnchors, preview: &Preview, dest: &Path, confirm_rebuild: bool) -> Result<ExportResult, String> {
    if preview.repositories.iter().any(|r| r.rebuild_anchor) && !confirm_rebuild {
        return Err("请先确认重建已列出的仓库锚点，然后导出。".into());
    }
    let file_count = engine::export(preview, dest)?;
    let anchor_error = update(data, scope, observed, preview, confirm_rebuild).err();
    Ok(ExportResult { file_count, anchor_error })
}
