import { invoke } from "@tauri-apps/api/core";
import { open, save } from "@tauri-apps/plugin-dialog";
import {
  AlertCircle, Archive, CalendarDays, History, CheckCircle2, ChevronDown, ChevronRight,
  Clock3, createElement, Download, FolderGit2, FolderOpen,
  GitBranch, GitCompareArrows, GitPullRequest, LoaderCircle,
  ScanSearch, SlidersHorizontal,
} from "lucide";
import "./style.css";

type Kind = "added" | "modified" | "deleted" | "renamed";
interface Change { kind: Kind; path: string; oldPath: string | null }
interface Repository { path: string; branch: string; start: string | null; end: string | null; changes: Change[]; rebuildAnchor: boolean }
interface Preview { id: string; repositories: Repository[]; errors: string[]; fileCount: number }
interface ExportResult { fileCount: number; anchorError: string | null }
interface RemoteStatus {
  state: "synced" | "ahead" | "behind" | "diverged" | "unavailable";
  upstream: string | null;
  ahead: number;
  behind: number;
  reason: string | null;
}
type Precision = "day" | "minute";
type Busy = "idle" | "choosing" | "scanning" | "exporting";

const icons = {
  "git-pull-request": GitPullRequest, "sliders-horizontal": SlidersHorizontal,
  "folder-open": FolderOpen, "chevron-right": ChevronRight, "clock-3": Clock3,
  "scan-search": ScanSearch, archive: Archive, download: Download,
  "folder-git": FolderGit2, branch: GitBranch, compare: GitCompareArrows,
  "chevron-down": ChevronDown, alert: AlertCircle, calendar: CalendarDays, history: History,
  success: CheckCircle2, loader: LoaderCircle,
};
type IconName = keyof typeof icons;
function icon(name: IconName) {
  const node = createElement(icons[name]);
  node.setAttribute("aria-hidden", "true");
  if (name === "loader") node.classList.add("spin");
  return node;
}
document.querySelectorAll<HTMLElement>("i[data-lucide]").forEach(node => {
  node.replaceWith(icon(node.dataset.lucide as IconName));
});
function element<K extends keyof HTMLElementTagNameMap>(tag: K, className?: string, value?: string) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (value !== undefined) node.textContent = value;
  return node;
}
function byId<T extends HTMLElement = HTMLElement>(id: string): T {
  return document.getElementById(id) as T;
}

const form = byId<HTMLFormElement>("conditions");
const startAt = byId<HTMLInputElement>("start-at");
const endAt = byId<HTMLInputElement>("end-at");
const dateMode = byId<HTMLButtonElement>("date-mode");
const anchorMode = byId<HTMLButtonElement>("anchor-mode");
const latest = byId<HTMLInputElement>("anchor-latest");
const rebuildPanel = byId("rebuild-panel");
const rebuildList = byId("rebuild-list");
const confirmRebuild = byId<HTMLInputElement>("confirm-rebuild");
const chooseRoot = byId<HTMLButtonElement>("choose-root");
const scan = byId<HTMLButtonElement>("scan");
const exportButton = byId<HTMLButtonElement>("export");
const body = byId("result-body");
const exportCount = byId("export-count");
const repoCount = byId("repo-count");
const notice = byId("notice");
const modeButtons = document.querySelectorAll<HTMLButtonElement>("[data-mode]");
const label: Record<Kind, string> = { added: "新增", modified: "修改", deleted: "删除", renamed: "改名" };
let root = "";
let gitReady = false;
let snapshot: Preview | null = null;
let revision = 0;
let busy: Busy = "idle";
let precision: Precision = "day";
let fromAnchor = false;
let remoteCells: HTMLElement[] = [];
let nextRemoteIndex = 0;
let activeRemoteChecks = 0;

function localDate(date: Date) {
  const pad = (n: number) => String(n).padStart(2, "0");
  return date.getFullYear() + "-" + pad(date.getMonth() + 1) + "-" + pad(date.getDate());
}
const today = localDate(new Date());
startAt.value = today;
endAt.value = today;
const zone = Intl.DateTimeFormat(undefined, { timeZoneName: "shortOffset" }).formatToParts(new Date())
  .find(part => part.type === "timeZoneName")?.value;
byId("local-zone").textContent = zone ? "本地时间 · " + zone : "本地时间";

function bounds() {
  if (fromAnchor && latest.checked) return { start: null, endExclusive: null };
  if (!endAt.value || (!fromAnchor && !startAt.value)) throw new Error(fromAnchor ? "请选择截止时间。" : "请选择完整的起止时间。");
  function parse(value: string) {
    const local = value.length === 10 ? value + "T00:00" : value;
    const date = new Date(local);
    if (!Number.isFinite(date.valueOf()) || (value.length > 10
      && (date.getHours() !== Number(local.slice(11, 13)) || date.getMinutes() !== Number(local.slice(14, 16))))) {
      throw new Error("所选时间在本地时区中无效，请调整时间。");
    }
    return date;
  }
  const start = fromAnchor ? null : parse(startAt.value);
  const end = parse(endAt.value);
  if (precision === "day") end.setDate(end.getDate() + 1);
  else end.setTime(end.getTime() + 60000);
  if (start && start >= end) throw new Error("结束时间不能早于开始时间。");
  return { start: start ? start.getTime() / 1000 : null, endExclusive: end.getTime() / 1000 };
}

function notify(message = "", type: "error" | "success" | "warning" | "info" = "info") {
  notice.hidden = !message;
  notice.className = "notice " + type;
  notice.replaceChildren();
  if (message) notice.append(icon(type === "success" ? "success" : "alert"), element("span", "", message));
}
function emptyState(symbol: IconName, title: string, detail: string) {
  const state = element("div", "empty-state");
  const mark = element("div", "empty-mark");
  mark.append(icon(symbol));
  state.append(mark, element("h3", "", title), element("p", "", detail));
  body.replaceChildren(state);
}
function refreshControls() {
  const locked = busy !== "idle";
  const usesTime = !fromAnchor || !latest.checked;
  chooseRoot.disabled = dateMode.disabled = anchorMode.disabled = locked;
  latest.disabled = locked || !fromAnchor;
  startAt.disabled = locked || fromAnchor;
  endAt.disabled = locked || !usesTime;
  confirmRebuild.disabled = locked || rebuildPanel.hidden;
  for (const button of modeButtons) button.disabled = locked || !usesTime;
  scan.disabled = locked || !gitReady;
  scan.replaceChildren(icon(busy === "scanning" ? "loader" : "scan-search"),
    element("span", "", busy === "scanning" ? "正在比对" : "预览差异"));
  exportButton.disabled = locked || !snapshot || !snapshot.fileCount || snapshot.errors.length > 0
    || (!rebuildPanel.hidden && !confirmRebuild.checked);
  exportButton.replaceChildren(icon(busy === "exporting" ? "loader" : "download"),
    element("span", "", busy === "exporting" ? "正在导出" : "导出 ZIP"));
}
function resetPreview() {
  revision++;
  snapshot = null;
  remoteCells = [];
  nextRemoteIndex = 0;
  rebuildPanel.hidden = true;
  rebuildList.replaceChildren();
  confirmRebuild.checked = false;
  notify();
  repoCount.textContent = "";
  exportCount.textContent = "等待预览";
  emptyState("compare", "等待比对", root ? "点击「预览差异」查看文件变更。" : "选择项目目录后即可预览文件变更。");
  refreshControls();
}
function rootName() {
  return root.split(/[\\/]/).filter(Boolean).pop() || root;
}

chooseRoot.addEventListener("click", async () => {
  if (busy !== "idle") return;
  busy = "choosing";
  refreshControls();
  try {
    const selected = await open({ directory: true, multiple: false, title: "选择 Git 项目所在目录", defaultPath: root || undefined });
    if (typeof selected === "string" && selected !== root) {
      root = selected;
      byId("root-name").textContent = rootName();
      byId("root-detail").textContent = selected;
      byId("root-detail").title = selected;
      chooseRoot.classList.add("has-root");
      chooseRoot.title = "更换项目目录";
      resetPreview();
    }
  } catch (error) {
    notify("选择目录失败：" + String(error), "error");
  } finally {
    busy = "idle";
    refreshControls();
  }
});
function updateRangeFields() {
  for (const button of [dateMode, anchorMode]) {
    const selected = (button === anchorMode) === fromAnchor;
    button.classList.toggle("selected", selected);
    button.setAttribute("aria-pressed", String(selected));
  }
  byId("latest-choice").hidden = !fromAnchor;
  byId("start-field").hidden = fromAnchor;
  byId("time-fields").hidden = fromAnchor && latest.checked;
  resetPreview();
}
for (const mode of [dateMode, anchorMode]) {
  mode.addEventListener("click", () => {
    if (busy !== "idle" || fromAnchor === (mode === anchorMode)) return;
    fromAnchor = mode === anchorMode;
    updateRangeFields();
  });
}
latest.addEventListener("change", updateRangeFields);
confirmRebuild.addEventListener("change", refreshControls);
for (const button of modeButtons) {
  button.addEventListener("click", () => {
    const next = button.dataset.mode as Precision;
    if (next === precision) return;
    precision = next;
    for (const field of [startAt, endAt]) {
      const value = field.value.slice(0, 10);
      field.type = next === "day" ? "date" : "datetime-local";
      field.value = value && next === "minute" ? value + (field === startAt ? "T00:00" : "T23:59") : value;
    }
    for (const item of modeButtons) {
      item.classList.toggle("selected", item === button);
      item.setAttribute("aria-pressed", String(item === button));
    }
    resetPreview();
  });
}
for (const field of [startAt, endAt]) field.addEventListener("input", resetPreview);

let openingDiff = false;
async function showFile(data: Preview, repo: Repository, change: Change, opener: HTMLButtonElement) {
  if (openingDiff || data !== snapshot || busy !== "idle") return;
  openingDiff = true;
  try {
    const { openDiffViewer } = await import("./diff-view");
    if (data !== snapshot || busy !== "idle") return;
    const files = data.repositories.flatMap((item, repoIndex) =>
      item.changes.map((file, changeIndex) => ({
        ...file, repo: item.path || rootName(), start: item.start, end: item.end, repoIndex, changeIndex,
      })));
    const repoIndex = data.repositories.indexOf(repo);
    const changeIndex = repo.changes.indexOf(change);
    openDiffViewer({
      files,
      index: files.findIndex(file => file.repoIndex === repoIndex && file.changeIndex === changeIndex),
      load: index => invoke("file_diff", { id: data.id, repoIndex: files[index].repoIndex, changeIndex: files[index].changeIndex }),
      onClose: () => opener.focus({ preventScroll: true }),
    });
  } catch (error) {
    notify("打开代码对比失败：" + String(error), "error");
  } finally {
    openingDiff = false;
  }
}
function showRemoteStatus(cell: HTMLElement, status: RemoteStatus) {
  const upstream = status.upstream || "上游分支";
  let text: string;
  switch (status.state) {
    case "synced": text = `与 ${upstream} 已同步`; break;
    case "ahead": text = `领先 ${upstream} ${status.ahead} 个提交`; break;
    case "behind": text = `落后 ${upstream} ${status.behind} 个提交`; break;
    case "diverged": text = `与 ${upstream} 已分叉，本地独有 ${status.ahead} 个提交，远程独有 ${status.behind} 个提交`; break;
    case "unavailable": text = "远程状态未核实" + (status.reason ? " · " + status.reason : ""); break;
  }
  cell.dataset.state = status.state;
  cell.replaceChildren();
  if (status.state === "behind" || status.state === "diverged") {
    cell.append(icon("alert"));
    text += "。如需包含远程改动，请手动拉取后重新预览。";
  }
  cell.append(document.createTextNode(text));
}

async function checkRemote(data: Preview, repoIndex: number, cell: HTMLElement) {
  try {
    const status = await invoke<RemoteStatus>("check_remote", { id: data.id, repoIndex });
    if (snapshot === data) showRemoteStatus(cell, status);
  } catch (error) {
    if (snapshot === data) {
      showRemoteStatus(cell, { state: "unavailable", upstream: null, ahead: 0, behind: 0, reason: String(error) });
    }
  } finally {
    activeRemoteChecks--;
    dispatchRemoteChecks();
  }
}

function dispatchRemoteChecks() {
  const data = snapshot;
  if (!data) return;
  while (activeRemoteChecks < 3 && nextRemoteIndex < remoteCells.length) {
    const index = nextRemoteIndex++;
    activeRemoteChecks++;
    void checkRemote(data, index, remoteCells[index]);
  }
}

function renderRepository(data: Preview, repo: Repository) {
  const section = element("details", "repository");
  section.open = repo.changes.length > 0;
  const summary = element("summary");
  const marker = element("span", "repo-marker");
  marker.append(icon("folder-git"));
  const heading = element("div", "repo-heading");
  const name = repo.path || rootName();
  const title = element("strong", "repo-name", name);
  title.title = name;
  const meta = element("div", "repo-meta");
  const branch = element("span", "branch");
  branch.append(icon("branch"), document.createTextNode(repo.branch));
  branch.title = repo.branch;
  const commits = element("span", "commits",
    (repo.start?.slice(0, 8) ?? "空树") + " → " + (repo.end?.slice(0, 8) ?? "空树"));
  commits.title = (repo.start ?? "空树") + " → " + (repo.end ?? "空树");
  meta.append(branch, commits);
  const remote = element("div", "remote-status", "正在检查远程分支…");
  remote.dataset.state = "checking";
  remote.setAttribute("role", "status");
  remoteCells.push(remote);
  heading.append(title, meta, remote);
  summary.append(marker, heading, element("span", "change-count", repo.changes.length + " 项"), icon("chevron-down"));
  section.append(summary);
  if (!repo.changes.length) {
    section.append(element("p", "repo-empty", repo.end ? "此范围内没有净差异。" : "结束时间前没有提交。"));
    return section;
  }
  const rows = element("div", "change-list");
  let shown = 0;
  const more = element("button", "show-more");
  more.type = "button";
  function appendPage() {
    const fragment = document.createDocumentFragment();
    const limit = Math.min(shown + 100, repo.changes.length);
    while (shown < limit) {
      const change = repo.changes[shown++];
      const row = element("button", "change-row");
      row.type = "button";
      row.title = "查看文件差异";
      row.setAttribute("aria-label", `查看文件差异：${change.path}（${label[change.kind]}）`);
      row.addEventListener("click", () => void showFile(data, repo, change, row));
      const path = element("span", "file-path");
      if (change.oldPath) path.append(element("span", "old-path", change.oldPath + " → "));
      path.append(document.createTextNode(change.path));
      row.append(element("span", "kind " + change.kind, label[change.kind]), path, icon("chevron-right"));
      fragment.append(row);
    }
    rows.append(fragment);
    more.textContent = "显示更多（剩余 " + (repo.changes.length - shown) + " 项）";
    more.hidden = shown === repo.changes.length;
  }
  section.append(rows, more);
  more.addEventListener("click", appendPage);
  appendPage();
  return section;
}
function renderPreview(data: Preview) {
  repoCount.textContent = data.repositories.length + " 个仓库";
  exportCount.textContent = data.errors.length ? "扫描有错误，暂不可导出" :
    data.fileCount ? data.fileCount + " 个文件可打包" : "没有可打包的文件";
  body.replaceChildren();
  const rebuilds = data.repositories.filter(repo => repo.rebuildAnchor);
  if (!fromAnchor && !data.errors.length && data.fileCount && rebuilds.length) {
    for (const repo of rebuilds) {
      const item = element("li", "", (repo.path || rootName()) + " · " + repo.branch + " → ");
      const commit = element("code", "", repo.end?.slice(0, 8));
      commit.title = repo.end ?? "";
      item.append(commit);
      rebuildList.append(item);
    }
    rebuildPanel.hidden = false;
  }
  if (!data.repositories.length) {
    emptyState(data.errors.length ? "alert" : "folder-git",
      data.errors.length ? "未能完成预览" : "未找到 Git 仓库",
      data.errors.length ? "请查看扫描错误并重新预览。" : "请重新选择项目所在的目录。");
  } else {
    const list = element("div", "repositories");
    for (const repo of data.repositories) list.append(renderRepository(data, repo));
    body.append(list);
    dispatchRemoteChecks();
  }
  if (data.errors.length) {
    const errors = element("div", "error-list");
    errors.setAttribute("role", "alert");
    errors.append(element("strong", "", "扫描错误（" + data.errors.length + "）"));
    for (const reason of data.errors) errors.append(element("p", "", reason));
    body.prepend(errors);
  } else if (data.repositories.some(repo => repo.changes.some(c => c.kind === "deleted" || c.kind === "renamed"))) {
    notify("解压覆盖时，已删除和改名前的旧路径需手动清理。");
  }
}
form.addEventListener("submit", async event => {
  event.preventDefault();
  if (busy !== "idle" || !gitReady) return;
  if (!root) {
    notify("请先选择项目目录。", "error");
    chooseRoot.focus();
    return;
  }
  let range: ReturnType<typeof bounds>;
  try { range = bounds(); }
  catch (error) { notify(error instanceof Error ? error.message : String(error), "error"); return; }
  resetPreview();
  const request = revision;
  busy = "scanning";
  emptyState("loader", "正在比对", "正在读取仓库和提交记录…");
  refreshControls();
  try {
    const result = await invoke<Preview>("scan_range", { root, ...range });
    if (revision !== request) return;
    snapshot = result;
    renderPreview(result);
  } catch (error) {
    if (revision !== request) return;
    emptyState("alert", "比对失败", "调整条件后可重新预览。");
    notify(String(error), "error");
  } finally {
    busy = "idle";
    refreshControls();
  }
});
exportButton.addEventListener("click", async () => {
  if (exportButton.disabled || !snapshot) return;
  const current = snapshot;
  busy = "exporting";
  refreshControls();
  try {
    const startLabel = fromAnchor ? "从上次打包" : startAt.value;
    const endLabel = fromAnchor && latest.checked ? "最新提交" : endAt.value;
    const dest = await save({ title: "导出 ZIP 压缩包",
      defaultPath: ("Git差异_" + startLabel + "_" + endLabel).replace(/:/g, "-") + ".zip",
      filters: [{ name: "ZIP 压缩包", extensions: ["zip"] }] });
    if (!dest || snapshot !== current) return;
    const result = await invoke<ExportResult>("export_zip", {
      id: current.id, dest, confirmRebuild: !rebuildPanel.hidden && confirmRebuild.checked,
    });
    const saved = "已导出 " + result.fileCount + " 个文件到 " + dest;
    if (result.anchorError) {
      notify(saved + "。锚点保存失败：" + result.anchorError, "warning");
    } else {
      notify(saved + "。打包进度已保存。", "success");
    }
  } catch (error) {
    notify("导出失败：" + String(error), "error");
  } finally {
    busy = "idle";
    refreshControls();
  }
});
resetPreview();
async function checkGit() {
  try {
    const version = await invoke<string>("git_version");
    gitReady = true;
    byId("version").textContent = version.replace(/^git version /i, "Git ");
    byId("git-status").classList.add("ready");
  } catch (error) {
    byId("version").textContent = "Git 不可用";
    byId("git-status").classList.add("unavailable");
    byId("git-status").title = String(error);
    notify("无法使用 Git，请确认已安装 Git 并加入 PATH 后重新启动。" + String(error), "error");
  }
  refreshControls();
}
void checkGit();
