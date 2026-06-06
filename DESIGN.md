# openworktree (`owt`) 設計文件

## 1. 定位

> 給命令 / agent 用的 **worktree 執行沙箱**——透明前綴、即用即棄、可隔離並行。

- **不是** worktree 管理器:不做 `mv` / `rename` / 編輯類指令。
- 只保留 **GC 等級**的 `list` / `clean`(因為 oneshot 在 crash / `kill -9` 時會留下孤兒)。
- 與 [git-worktree-runner (`gtr`)](https://github.com/coderabbitai/git-worktree-runner) 的差異:gtr 是「給人用的持久 worktree 管理器」;owt 是「即用即棄的執行沙箱」,可被腳本 / agent 當底層原語呼叫。

一句話:
> gtr 是 worktree 管理器;owt 是 worktree 執行沙箱(ephemeral、可並行、可預熱)。

---

## 2. 模式與清理規則

| 模式 | 觸發 | 行為 | 清理 |
|------|------|------|------|
| **oneshot**(預設) | `owt -- <cmd>` | 建 worktree → 執行命令 → 透傳退出碼 | 依 `--on-exit` 政策(預設 `discard`),含被 Ctrl+C 中斷時 |
| **interactive** | `owt -i` | 建 worktree → 開子 shell 進去 | **不自動清**,留給之後 `owt clean` |

### oneshot 結束策略 `--on-exit`

| 政策 | worktree 目錄 | 未提交變更 | 中途已 commit | branch |
|------|------|------|------|------|
| **`discard`**(預設) | 刪除 | 丟棄 | **一起刪** | 刪除(全清,真 ephemeral) |
| **`keep`** | 刪除 | **自動 commit** | 保留 | **保留**(即使無變更也留空 branch,指向 base commit) |

- `keep` 的自動 commit 訊息:動態模板 `owt: <command> @ <時間>`,可在 config 覆寫。

---

## 3. CLI 介面

```
owt [OPTIONS] -- <command>...      # oneshot
owt -i [OPTIONS]                   # interactive
owt list [--all] [--json]          # 列出 worktree(詳見 §4.6)
owt clean [<name>] [--running] [--all] [--force] [--yes] [--dry-run]   # 清理(詳見 §4.6)
```

### 建立時共用 OPTIONS

| Flag | 預設 | 說明 |
|------|------|------|
| `--from <ref>` | `HEAD`(= 當前所在 worktree 的 HEAD) | worktree 來源 ref |
| `--name <name>` | 隨機可讀名(`形容詞-名詞`) | branch + 目錄名;**衝突則報錯阻止** |
| `--dir <path>` | `<app_cache>/owt/<repo名>__<worktree名>/` | worktree 放哪 |
| `--setup <cmd>` | 無 | 執行主命令前先跑(如 `npm ci`) |
| `--on-exit <discard\|keep>` | `discard` | oneshot 結束策略 |
| `--keep` | — | `--on-exit keep` 的簡寫 |
| `--shell <shell>` | config 設定 / 系統預設 | interactive 用哪個 shell |

---

## 4. 關鍵行為

### 4.1 Git repo 偵測
- 用 `git rev-parse --git-common-dir` 取得共享 `.git` 當錨點。
- 在主 repo 或**任何 worktree 內**執行都可以——一律對共享 repo 建立平行 worktree。
- 只有不在任何 git 樹下才**報錯**。

### 4.2 命名
- 未指定:`形容詞-名詞`(可讀,方便 `list` 辨識)。
- 指定:直接用;撞名 → 報錯阻止。

### 4.3 目錄
- 未指定:`<app_cache>/owt/<repo名>__<worktree名>/`(內嵌 repo 名避免多專案混淆)。
- 指定:用使用者給的。

### 4.4 `.worktreeinclude`(複製被 git 忽略但執行需要的檔案)
- 語法仿 `.gitignore`:支援 glob、`!` 排除。
- 支援 **symlink vs 複製**:用前綴標記區分(如 `@node_modules` 表 symlink、一般行表複製)。
- `--include <glob>` 可臨時覆寫。

### 4.5 退出碼與訊號
- **退出碼透傳**:`owt` 退出碼 = 子命令退出碼。
- **訊號處理**:SIGINT / SIGTERM(及 Windows Ctrl+C)→ oneshot 先依政策清理再退出(RAII guard 確保不留孤兒)。

### 4.6 `list` / `clean`(查閱與 GC)

兩個正交維度:
- **所有權**:owt 自己建的 vs external(使用者用 `git worktree` / gtr 等建的)。靠 worktree 內有無 `.owt-meta.json` 判斷。
- **存活狀態**:orphan(對應 process 已死)vs running(process 存活,靠 metadata 的 `pid` 檢查)。

`--all` 在兩個指令中**意義一致** = 「所有非主 worktree,含 external」(主 worktree 永遠排除)。

**list(唯讀,安全)**
```
owt list           # owt 自己的(running + orphan 都列)
owt list --all     # 所有非主 worktree,含 external(唯讀,標記來源 owt / external)
```

**clean(動作,危險,清理範圍階梯)**

| 指令 | 範圍 | 危險度 |
|------|------|------|
| `owt clean` | owt 的 orphan(死掉的) | 安全(預設) |
| `owt clean <name>` | 指定那一個 owt worktree | 低 |
| `owt clean --running` | 連還活著的 **owt** worktree 也清 | 中 |
| `owt clean --all` | **所有**非主 worktree,**含 external** | 高(核彈) |

**`--all` 的安全護欄(碰到 external 真實工作時必備):**
1. **永不刪主 worktree**:用 `git worktree list --porcelain` 辨識主樹並排除,只動 linked worktree。
2. **預設先列清單 + 互動確認**:印出將被刪的清單(標出 external / 有未提交變更),確認才動手;`--yes` 跳過。
3. **髒的不強刪**:有未提交變更者預設拒絕並警告,要 `--force` 才強刪(沿用 `git worktree remove` 行為)。
4. **尊重 lock**:被 `git worktree lock` 鎖住者跳過,除非 `--force`。
5. **`--dry-run`**:只印不刪。

範例:
```
$ owt clean --all
將刪除以下 worktree(主 worktree 不受影響):
  brave-otter    owt       orphan
  calm-finch     owt       running   ⚠ process 存活
  ../my-feature  external  dirty     ⚠ 有未提交變更(需 --force)
確認刪除?[y/N]
```

> 定位提醒:`clean --all` 含 external 讓 owt 稍微踏進「worktree 管理」,但本質仍是 GC / cleanup(非 create/mv/rename),靠上述護欄壓住誤刪風險。

---

## 5. metadata(`.owt-meta.json`)

**雙寫**:
1. **co-located 副本**:寫進該 worktree 的 **git 私有管理目錄** `.git/worktrees/<id>/owt-meta.json`(用 `git -C <wt> rev-parse --git-dir` 取得)。
   - **不在工作樹** → `git status` / `git add -A` 看不到,使用者或 keep 模式都不可能把它 commit 進去。
   - `git worktree remove` / `prune` 時由 git 自動一併刪除,生命週期與 worktree 綁定。
   - ⚠ 不要放在 worktree 工作目錄下(會被誤 commit)——這是刻意避開的設計。
2. **中央索引**:`<app_cache>/index/<name>.json`,供 `list` / `clean` 快速掃描。

```json
{
  "schema_version": 1,
  "name": "brave-otter",
  "branch": "owt/brave-otter",
  "from_ref": "HEAD",
  "base_commit": "a3f9c2e...",
  "worktree_path": "/abs/path/...",
  "repo_common_dir": "/abs/repo/.git",
  "mode": "oneshot",
  "command": ["npm", "test"],
  "on_exit": "discard",
  "pid": 48213,
  "created_at": "2026-06-06T10:30:00Z",
  "status": "running"
}
```

- `pid` + `status` → `list` 判斷 running vs orphan。
- `repo_common_dir` → `clean` 知道對哪個 repo 下 `git worktree remove`。
- `command` / `mode` / `created_at` → `list` 顯示。
- `base_commit` → 除錯用。

---

## 6. Rust 技術選型

| 面向 | crate | 備註 |
|------|------|------|
| CLI 解析 | `clap`(derive) | |
| Git 操作 | **shell out `git worktree`**(`std::process::Command`) | 比 libgit2 穩,沿用使用者 git 設定 / credential / hooks |
| `.worktreeinclude` 比對 | `ignore` + `globset` | gitignore 同款語法 |
| 隨機可讀名 | `rand` + 內建字表 | |
| 訊號處理 | `ctrlc` | 跨平台(Windows) |
| App 目錄 | `directories`(ProjectDirs) | 跨平台 cache / config |
| 狀態 / PID | `sysinfo` | running vs orphan |
| 序列化 | `serde` + `serde_json` | metadata / `--json` |

---

## 7. 模組架構

```
src/
  main.rs          # 進入點、clap 分派
  cli.rs           # 參數定義
  config.rs        # app 目錄、預設值、使用者 config
  worktree.rs      # 建立 / 刪除 worktree(包 git 命令)
  include.rs       # .worktreeinclude 解析 + 複製 / symlink
  naming.rs        # 隨機可讀名產生
  runner.rs        # spawn 子命令 / 子 shell、退出碼透傳、訊號處理
  state.rs         # metadata 讀寫、list / clean 邏輯
  cleanup.rs       # 清理流程(RAII guard,中斷時也安全)
```

---

## 8. 開發里程碑

- **M1 — 核心 oneshot**:`owt -- <cmd>`,建 → 跑 → 退出碼透傳 → `--on-exit discard` 自動清 + 訊號安全清理。含 `--from` / `--name` / `--dir`。
- **M2 — 環境準備**:`.worktreeinclude`(複製 + symlink)、`--setup`、`--on-exit keep`。
- **M3 — interactive**:`owt -i` 開子 shell、不自動清、`--shell` / config。
- **M4 — GC**:`owt list` / `owt clean` + metadata 雙寫 / PID 追蹤。
- **M5(之後)**:`--each <refs>` 跨 ref 比較、`--shard N` 隔離並行。

### 刻意不做(避免變成另一個 gtr)
`mv` / `rename` / 編輯類指令、TUI、AI 工具選單。命令越少,定位越鋒利。
