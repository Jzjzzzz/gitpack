import { editor } from "monaco-editor/esm/vs/editor/editor.api";
import EditorWorker from "monaco-editor/esm/vs/editor/editor.worker?worker";
import "monaco-editor/esm/vs/base/browser/ui/codicons/codiconStyles";
import "monaco-editor/esm/vs/editor/browser/widget/diffEditor/diffEditor.contribution";
import "monaco-editor/esm/vs/basic-languages/java/java.contribution";
import "monaco-editor/esm/vs/basic-languages/javascript/javascript.contribution";
import "monaco-editor/esm/vs/basic-languages/typescript/typescript.contribution";
import "monaco-editor/esm/vs/basic-languages/python/python.contribution";
import "monaco-editor/esm/vs/basic-languages/cpp/cpp.contribution";
import "monaco-editor/esm/vs/basic-languages/csharp/csharp.contribution";
import "monaco-editor/esm/vs/basic-languages/rust/rust.contribution";
import "monaco-editor/esm/vs/basic-languages/go/go.contribution";
import "monaco-editor/esm/vs/basic-languages/css/css.contribution";
import "monaco-editor/esm/vs/basic-languages/html/html.contribution";
import "monaco-editor/esm/vs/basic-languages/xml/xml.contribution";
import "monaco-editor/esm/vs/basic-languages/yaml/yaml.contribution";
import "monaco-editor/esm/vs/basic-languages/sql/sql.contribution";
import "monaco-editor/esm/vs/basic-languages/markdown/markdown.contribution";
import { ArrowDown, ArrowLeft, ArrowRight, ArrowUp, GitCompareArrows, GitCommitHorizontal, X, createElement } from "lucide";
import "./diff-view.css";

window.MonacoEnvironment = { getWorker: () => new EditorWorker() };

editor.defineTheme("gitpack-dark", {
  base: "vs-dark",
  inherit: true,
  rules: [
    { token: "comment", foreground: "86918D" },
    { token: "string", foreground: "A4CD91" },
    { token: "keyword", foreground: "DFA175" },
    { token: "number", foreground: "8AB9E5" },
    { token: "type.identifier", foreground: "9BD0DE" },
  ],
  colors: {
    "editor.background": "#1C201F",
    "editor.foreground": "#DFE6E1",
    "editorLineNumber.foreground": "#87948F",
    "editorLineNumber.activeForeground": "#DBE5DD",
    "editor.lineHighlightBackground": "#FFFFFF06",
    "diffEditor.insertedLineBackground": "#6193602D",
    "diffEditor.removedLineBackground": "#A960612B",
    "diffEditor.insertedTextBackground": "#72AE5763",
    "diffEditor.removedTextBackground": "#D17D7863",
    "diffEditor.diagonalFill": "#404A45",
    "diffEditor.border": "#414A45",
  },
});

export interface DiffFile {
  path: string;
  oldPath: string | null;
  kind: string;
  repo: string;
  start: string | null;
  end: string | null;
}

export interface FileContent {
  state: string;
  before: string | null;
  after: string | null;
  beforeSize: number | null;
  afterSize: number | null;
  beforeMode?: string | null;
  afterMode?: string | null;
  beforeEncoding?: string | null;
  afterEncoding?: string | null;
}

interface ViewOptions {
  files: DiffFile[];
  index: number;
  load(index: number): Promise<FileContent>;
  onClose(): void;
}

const languages: Record<string, string> = {
  java: "java", js: "javascript", jsx: "javascript", cjs: "javascript", mjs: "javascript",
  ts: "typescript", tsx: "typescript", py: "python", rs: "rust", go: "go",
  cs: "csharp", cpp: "cpp", cxx: "cpp", hpp: "cpp", h: "cpp", c: "c",
  css: "css", html: "html", htm: "html", xml: "xml", svg: "xml",
  xaml: "xml", yml: "yaml", yaml: "yaml", sql: "sql", md: "markdown",
};

function language(path: string) {
  return languages[path.toLowerCase().split(".").pop() ?? ""] ?? "plaintext";
}

function node<K extends keyof HTMLElementTagNameMap>(tag: K, className: string, value = "") {
  const el = document.createElement(tag);
  el.className = className;
  el.textContent = value;
  return el;
}

function control(id: string, label: string, shape: typeof X, action: () => void) {
  const button = node("button", "dv-control");
  button.id = id;
  button.type = "button";
  button.title = label;
  button.setAttribute("aria-label", label);
  button.append(createElement(shape));
  button.addEventListener("click", action);
  return button;
}

function versionHeader(label: string) {
  const header = node("div", "dv-version");
  const oid = node("code", "dv-oid");
  const path = node("span", "dv-path");
  header.append(node("span", "dv-label", label), createElement(GitCommitHorizontal), oid, path);
  return { header, oid, path };
}

function newlineStyle(text: string) {
  const formats = new Set<string>();
  for (const match of text.matchAll(/\r\n|\r|\n/g)) {
    formats.add(match[0] === "\r\n" ? "CRLF" : match[0] === "\n" ? "LF" : "CR");
    if (formats.size > 1) return "混合";
  }
  return formats.values().next().value ?? "";
}

function textNotes(file: DiffFile, content: FileContent) {
  const { before, after, beforeEncoding, afterEncoding, beforeMode, afterMode } = content;
  if (before === null) return after === "" ? "新增空文件" : "";
  if (after === null) return before === "" ? "删除空文件" : "";
  const notes: string[] = [];
  if (beforeEncoding && afterEncoding && beforeEncoding !== afterEncoding) {
    notes.push("编码 " + beforeEncoding + " → " + afterEncoding);
  }
  if (beforeMode && afterMode && beforeMode !== afterMode) {
    notes.push("文件模式 " + beforeMode + " → " + afterMode);
  }
  if (before === after) {
    notes.push(file.kind === "renamed" ? "文件已改名，内容相同" : "两侧文本相同");
  } else {
    const a = newlineStyle(before), b = newlineStyle(after);
    if (a && b && a !== b) notes.push("换行格式 " + a + " → " + b);
    else if (before.replace(/\r\n?/g, "\n") === after.replace(/\r\n?/g, "\n")) notes.push("换行格式有变化");
  }
  return notes.join("　·　");
}

function sizeLabel(size: number | null) {
  if (size === null) return "不存在";
  return size.toLocaleString("zh-CN") + " 字节";
}

export function openDiffViewer({ files, index, load, onClose }: ViewOptions) {
  if (!files[index]) return;
  const opener = document.activeElement;
  const dialog = node("dialog", "diff-view");
  dialog.id = "diff-view";
  dialog.setAttribute("aria-labelledby", "diff-title");
  const toolbar = node("header", "dv-toolbar");
  const heading = node("div", "dv-heading");
  const title = node("h2", "dv-title");
  title.id = "diff-title";
  const repo = node("span", "dv-repo");
  heading.append(createElement(GitCompareArrows), title, repo);
  const close = control("diff-close", "关闭代码对比 (Esc)", X, () => dialog.close());
  toolbar.append(heading, close);

  const navigation = node("div", "dv-nav");
  const fileCount = node("span", "dv-count");
  fileCount.id = "diff-file-count";
  const prevFile = control("diff-prev-file", "上一个文件 (Alt+←)", ArrowLeft, () => show(index - 1));
  const nextFile = control("diff-next-file", "下一个文件 (Alt+→)", ArrowRight, () => show(index + 1));
  const diffCount = node("span", "dv-count");
  diffCount.id = "diff-count";
  diffCount.setAttribute("aria-live", "polite");
  const prevChange = control("diff-prev-change", "上一处差异 (Alt+↑)", ArrowUp, () => goTo(current - 1));
  const nextChange = control("diff-next-change", "下一处差异 (Alt+↓)", ArrowDown, () => goTo(current + 1));
  navigation.append(prevFile, fileCount, nextFile, node("span", "dv-divider"), prevChange, diffCount, nextChange);
  const versions = node("div", "dv-versions");
  const old = versionHeader("起始版本"), updated = versionHeader("结束版本");
  versions.append(old.header, updated.header);
  const status = node("div", "dv-status");
  status.id = "diff-status";
  status.setAttribute("role", "status");
  const host = node("div", "dv-editor");
  host.id = "diff-editor";
  dialog.append(toolbar, navigation, versions, status, host);
  document.body.append(dialog);
  dialog.showModal();
  close.focus();

  const compare = editor.createDiffEditor(host, {
    theme: "gitpack-dark", automaticLayout: true, renderSideBySide: true,
    useInlineViewWhenSpaceIsLimited: false, enableSplitViewResizing: false,
    readOnly: true, originalEditable: false, domReadOnly: true, renderMarginRevertIcon: false,
    minimap: { enabled: false }, stickyScroll: { enabled: false },
    lineNumbersMinChars: 3, glyphMargin: false, folding: false, links: false,
    contextmenu: false, ignoreTrimWhitespace: false, diffAlgorithm: "advanced",
    renderIndicators: true, renderOverviewRuler: true, scrollBeyondLastLine: false,
    fontFamily: '"Cascadia Mono", Consolas, "Microsoft YaHei UI", monospace',
    fontSize: 13, lineHeight: 22, padding: { top: 8, bottom: 8 },
    originalAriaLabel: "起始版本代码", modifiedAriaLabel: "结束版本代码",
  });
  let loadVersion = 0, current = -1;
  let changes: editor.ILineChange[] = [];
  let models: editor.IDiffEditorModel | null = null;

  function clearModels() {
    compare.setModel(null);
    models?.original.dispose();
    models?.modified.dispose();
    models = null;
  }

  function updateNav() {
    diffCount.textContent = changes.length ? (current + 1) + " / " + changes.length + " 处差异" : "0 处差异";
    prevChange.disabled = current <= 0;
    nextChange.disabled = current < 0 || current >= changes.length - 1;
  }

  function goTo(which: number) {
    const change = changes[which];
    if (!change) return;
    current = which;
    const side = change.modifiedEndLineNumber === 0 ? compare.getOriginalEditor() : compare.getModifiedEditor();
    const line = change.modifiedEndLineNumber === 0 ? change.originalStartLineNumber : change.modifiedStartLineNumber;
    side.setPosition({ lineNumber: Math.max(1, line), column: 1 });
    side.revealLineInCenter(Math.max(1, line), editor.ScrollType.Immediate);
    updateNav();
  }

  compare.onDidUpdateDiff(() => {
    if (!models || compare.getModel()?.modified !== models.modified) return;
    const result = compare.getLineChanges();
    if (!result) return;
    changes = result;
    if (current < 0 && changes.length) goTo(0);
    else updateNav();
  });

  function message(text: string, full: boolean) {
    status.hidden = !text;
    status.textContent = text;
    status.classList.toggle("dv-full-status", full);
    host.hidden = full;
  }

  async function show(target: number) {
    const file = files[target];
    if (!file) return;
    const generation = ++loadVersion;
    index = target;
    clearModels();
    changes = [];
    current = -1;
    updateNav();
    diffCount.textContent = "读取中";
    fileCount.textContent = (index + 1) + " / " + files.length + " 个文件";
    prevFile.disabled = index === 0;
    nextFile.disabled = index === files.length - 1;
    title.textContent = file.path.split("/").pop() || file.path;
    title.title = file.path;
    repo.textContent = repo.title = file.repo;
    old.path.textContent = old.path.title = file.oldPath ?? file.path;
    updated.path.textContent = updated.path.title = file.path;
    old.oid.textContent = file.kind === "added" ? "文件不存在" : file.start?.slice(0, 8) ?? "空树";
    updated.oid.textContent = file.kind === "deleted" ? "文件不存在" : file.end?.slice(0, 8) ?? "空树";
    old.oid.title = file.start ?? "空树";
    updated.oid.title = file.end ?? "空树";
    message("正在读取提交中的文件…", true);
    try {
      const content = await load(target);
      if (generation !== loadVersion) return;
      if (content.state !== "text") {
        const reason = content.state === "binary" ? "二进制文件" : content.state === "tooLarge"
          ? "文件超过 2 MiB 文本预览上限" : "此文件编码暂不支持文本预览";
        message(reason + "　·　起始版本 " + sizeLabel(content.beforeSize) + "　·　结束版本 " + sizeLabel(content.afterSize), true);
        diffCount.textContent = "无文本预览";
        return;
      }
      message(textNotes(file, content), false);
      diffCount.textContent = "正在比对";
      models = {
        original: editor.createModel(content.before ?? "", language(file.oldPath ?? file.path)),
        modified: editor.createModel(content.after ?? "", language(file.path)),
      };
      compare.layout();
      compare.setModel(models);
    } catch (error) {
      if (generation !== loadVersion) return;
      message("无法读取文件。 " + String(error), true);
      diffCount.textContent = "读取失败";
    }
  }

  dialog.addEventListener("keydown", (event) => {
    if (event.key === "Escape") { event.preventDefault(); dialog.close(); return; }
    if (!event.altKey || event.ctrlKey || event.shiftKey || event.metaKey) return;
    const action = { ArrowLeft: prevFile, ArrowRight: nextFile, ArrowUp: prevChange, ArrowDown: nextChange }[event.key];
    if (action) { event.preventDefault(); action.click(); }
  }, true);
  dialog.addEventListener("close", () => {
    loadVersion++;
    clearModels();
    compare.dispose();
    dialog.remove();
    if (opener instanceof HTMLElement && opener.isConnected) opener.focus();
    onClose();
  }, { once: true });
  void show(index);
}
