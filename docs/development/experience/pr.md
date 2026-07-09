# Libra PR 长期解决方案：基于 `gh` 的 GitHub PR 命令

## 评估结论

该方案总体合理且可行：PR 是托管平台 API 概念，不属于 Git 协议；Libra 当前缺少托管平台层，第一版通过 GitHub CLI `gh` 复用认证、Enterprise host、SSO、2FA 与浏览器交互能力，比直接实现 GitHub API 更低风险。

需要补强的关键点是：

- 明确第一版只支持 GitHub remote 与同仓库 head 分支，fork PR 作为后续能力，避免 `--head` 语义过早扩张。
- 明确 `--web`、全局 `--json`/`--machine`、`--dry-run`、`--push`、`--fill`、`--title`、`--body`、`--body-file` 的互斥和优先级规则，保证接口稳定。
- 远端分支状态不能只依赖可能过期的 `refs/remotes/<remote>/<head>`，应优先用 push 结果或 `libra ls-remote --heads` 确认。
- `--push` 必须经过 Libra push 路径，且只对当前分支生效，防止 `gh` 或 `git` 绕过 Libra ref/storage 语义。
- 机器可读输出使用 Libra **全局** `--json` / `--machine`（`libra --json pr create`），成功走 `write_json_command_envelope`，失败走 `CliErrorReport`（`LBR-*`），不能把 `gh` human output 当成机器接口。
- 外部命令执行必须不经 shell，并对路径、remote URL、host、branch、输出中的敏感信息做边界处理。
- 增加 fake `gh`、fake push、stale remote refs、Enterprise host、错误分类、文档同步与兼容性测试。

按本文修订后的约束落地，方案在合理性、功能正确性、接口兼容性、安全性、可靠性、可维护性和长期扩展性上是可接受的。

## 全文评审分析（2026-07-09）

对本文按合理性、可行性、完整性、安全性、功能正确性与接口兼容性、数据流与控制流、性能与效率、可靠性与容错性、兼容性与互操作性、可扩展性与可维护性、合规性与标准符合性做复核，并扫描前后冲突。结论：**方案方向正确，第一版以 `gh` 为后端、Libra 负责 VCS 状态与 push 的分层合理**；需在落地前修正若干与 Libra 全局输出/错误契约、远端 head 判定机制、测试分层相关的表述（见下文「本次修订」与「前后冲突扫描」）。

### 逐维度评审矩阵

| 维度 | 结论 | 主要依据 / 风险 | 本文修订要求 |
| --- | --- | --- | --- |
| 合理性 | **通过** | PR 非 Git 协议；自建 GitHub API/auth 成本高；`gh` 复用 Enterprise/SSO | 维持 Libra=VCS、gh=PR API 边界 |
| 可行性 | **有条件通过** | 分 5 阶段、`fake gh` 可测；无现成 `src/command/pr.rs` | 第 1 阶段仅 `--dry-run` 可先落地；`gh` 最低版本须实测后写入 |
| 完整性 | **有条件通过** | 缺全局 `--json`/`--machine` 口径、缺 `PR_EXAMPLES`、缺 `LBR-*` 错误码草案、测试 gate 名不存在 | 补全局输出契约、错误码表、`tests/INDEX.md` 行、compat 守卫清单 |
| 安全性 | **有条件通过** | argv 无 shell、路径/URL 校验已写 | 补 `gh` 超时、stderr 长度上限、`--body-file` 大小上限 |
| 功能正确性与接口兼容性 | **需补强→已补强** | 原稿 `--json` 作子命令 flag，与全仓 `libra --json <cmd>` 惯例冲突；错误 JSON 用 `error.code` 点分串，与 `CliErrorReport` 的 `LBR-*` 不一致 | 删除子命令 `--json`；成功/失败 JSON 对齐 `OutputConfig` / `CliError` |
| 数据流与控制流 | **需补强→已补强** | 「Libra remote 查询」未点名具体命令 | 明确 `libra ls-remote --heads <remote> <head>` + push 结果优先 |
| 性能与效率 | **通过** | 非热路径、早失败、无默认 fetch | 保持；`gh auth status` 单次调用 |
| 可靠性与容错性 | **有条件通过** | push 成功 PR 失败、PR 已存在、超时 | 补 `gh` 进程超时与中断语义 |
| 兼容性与互操作性 | **有条件通过** | GHE host、非 TTY `--web`、fork 延后 | 保持第一版拒绝 fork；`gh auth login --hostname` 与 remote host 对齐 |
| 可扩展性与可维护性 | **通过** | 建议分层 `command/pr` + `internal/github` + injectable runner | 第一版可不引入 trait，但须可注入 `gh` runner |
| 合规性与标准符合性 | **有条件通过** | 未对齐 `docs/error-codes.md`、`tests/compat` 守卫 | 落地时新增 `LBR-PR-*` 或复用 `LBR-NETWORK`/`LBR-AUTH`/`LBR-REPO-003`；补 EN/zh 命令文档 |

### 前后冲突扫描与本次修订

| 编号 | 严重度 | 冲突 / 缺口 | 修订 |
| --- | --- | --- | --- |
| P1 | **高** | `libra pr create --json` 与全仓惯例（`libra --json push`、`libra --json ls-remote`、`libra open --json` 均为**全局** flag）冲突；子命令 `--json` 会导致双套 JSON 开关 | 删除子命令 `--json`；成功输出走 `emit_json_data` / `write_json_command_envelope`；脚本用 `libra --json pr create` 或 `libra --machine pr create` |
| P2 | **高** | 错误 JSON 示例为 `error.code: "pr.github.auth_failed"`，与 `src/utils/error.rs` 的 `CliErrorReport`（`error_code: "LBR-*"`、`category`、`hints[]`）不一致 | 改为 `LBR-*` 稳定码 + `compat_error_codes_doc_sync` 登记 |
| P3 | 中 | 「Libra remote 查询能力」未具名；仅读 `refs/remotes/...` 易过期 | 远端 head 判定顺序：push 结果 → `libra ls-remote --heads <remote> <head>` → 本地 tracking ref（标 `stale_risk`） |
| P4 | 中 | 测试写 `test-live-github`，`Cargo.toml` 无此 feature；L2 已有 `test-network` + `LIBRA_TEST_GITHUB_TOKEN` | 改为 L2：`--features test-network` + env gate；真网 live 测试单独登记 `tests/INDEX.md` |
| P5 | 中 | `--web` 与全局 `--json`/`--machine` 互斥未写 | 互斥表增补：`--web` 与全局 JSON/machine 输出不可同次调用 |
| P6 | 低 | 推荐 UX 写 `libra push -u origin feature/x`，`push -u` 要求显式 `repository`（`requires("repository")`） | 改为 `libra push -u origin <branch>` 并注明须带 remote 名 |
| P7 | 低 | 缺 `PR_EXAMPLES` / `compat_help_examples_banner` 契约 | 落地清单补 `PR_EXAMPLES` 与三份 compat 守卫 |

**冲突扫描结论**：P1/P2 为文档内部与 Libra 全局契约冲突，已修订；其余为完整性补强，无与「第一版仅 GitHub 同仓库 PR」目标冲突。

## 背景与问题

Libra 当前的 Git-compatible 命令族（`branch` / `switch` / `commit` / `push` / `open`）已经能完成“准备一个可被 review 的分支”的全部工作，但无法直接创建 Pull Request。原因是 PR 不是 Git 协议的一部分，而是 GitHub / GitLab / Gitea 等托管平台的 API 概念，Libra 目前没有托管平台层。

当前可行的临时流程是：

```bash
libra switch -c feature/my-change
libra add .
libra commit -s -m "feat(scope): describe change"
libra push -u origin feature/my-change
libra open https://github.com/<owner>/<repo>/compare/main...feature/my-change?expand=1
```

最后一步仍需在浏览器里手动点 “Create pull request”，不是一条命令的体验。

本方案给出一个长期可维护的实现路径：在 Libra 内新增 `libra pr` 命令族，以本地 `gh`（GitHub CLI）作为 GitHub PR 后端执行器。

## 目标与非目标

### 目标

- 提供 `libra pr create`，一条命令创建 GitHub Pull Request。
- 保持 Libra 是唯一 VCS 主体：分支状态、commit 检查、dirty 提示、push、tracking ref 更新均由 Libra 负责。
- 复用 `gh` 的 GitHub 认证、host 配置、Enterprise 支持与 PR API 能力。
- 提供稳定的人类可读输出；脚本/CI 使用全局 `libra --json pr create` 或 `libra --machine pr create`。
- 默认不静默 push，只有显式 `--push` 才推送。
- 支持离线/无副作用的 `--dry-run`，用于调试推断结果和命令参数。
- 为未来 GitHub native API、GitLab、Gitea provider 保留清晰扩展点。

### 非目标

- 第一版不实现 Libra 内置 GitHub token 存储、OAuth device flow 或 REST/GraphQL API 客户端。
- 第一版不支持 GitLab / Gitea / Bitbucket PR/MR 创建。
- 第一版不支持 fork PR 的完整自动化；可在后续阶段支持 `--head <owner>:<branch>`。
- 第一版不让 `gh` 或 `git` 执行 push，不允许它们更新 Libra ref。
- 第一版不依赖真实 GitHub 网络作为默认 CI 前置条件。

## 设计原则

### 为什么用 `gh` 作为后端，而不是直接实现 GitHub API

优点：

- 避免 Libra 自己维护 GitHub token 存储、OAuth / device flow、Enterprise host、2FA、SSO 等认证复杂度。
- `gh` 已经覆盖 GitHub.com 和 GitHub Enterprise。
- `gh pr create` 语义成熟，支持 `--fill` / `--draft` / `--reviewer` / `--label` / `--assignee` / `--project` / `--web` 等。
- Libra 先提供稳定的 PR UX，未来如需可替换为原生 GitHub API provider。
- 失败时保留必要的 `gh` 错误上下文，同时在 Libra 层补充可行动的修复建议。

缺点与控制措施：

| 风险 | 控制措施 |
|------|----------|
| 多一个外部运行时依赖 | 启动前检查 `gh --version`，缺失时给出安装建议 |
| 行为受用户本机 `gh` 版本影响 | 定义最低支持版本，并测试 argv 兼容性 |
| human output 不稳定 | 全局 `--json`/`--machine` 只输出 Libra envelope，不透传 `gh` 文本 |
| `gh` 可能尝试隐式 push 或打开交互 | 显式传参，禁用不兼容组合，push 只走 Libra |
| Enterprise host 配置不一致 | 从 remote host 推断 `--hostname` / `--repo`，认证失败时提示 `gh auth login --hostname <host>` |

长期看，这比在 Libra 内直接重写 GitHub 客户端更低风险、更容易维护。

### 职责边界

- Libra 负责：VCS 上下文、当前分支、base/head 推断、diff/ahead 检查、dirty 提示、push、安全校验、错误归一化、稳定 JSON envelope（经全局 `OutputConfig`）。
- `gh` 负责：GitHub 认证、host 配置、PR 创建、浏览器打开、GitHub Enterprise API 兼容。

关键约束：push 必须用 `libra push`，不要让 `gh` 或 `git` 替 Libra 更新 ref。Libra 是 VCS 主体，`gh` 只负责 GitHub PR API 这一步。

## 命令设计

新增命令族（第一阶段只实现 `create`）：

```bash
libra pr create [OPTIONS]
libra pr status [OPTIONS]             # 后续阶段
libra pr view [<number>] [OPTIONS]    # 后续阶段
libra pr checkout <number> [OPTIONS]  # 后续阶段
```

### `libra pr create` 参数

```bash
libra pr create \
  [--base <branch>] \
  [--head <branch>] \
  [--title <title>] \
  [--body <body>] \
  [--body-file <path>] \
  [--draft] \
  [--fill] \
  [--web] \
  [--push] \
  [--dry-run] \
  [--require-clean]
```

**机器可读输出**：使用 Libra **全局** flag，不在 `pr create` 上定义子命令级 `--json`（与 `libra --json push`、`libra --json ls-remote` 一致）：

```bash
libra --json pr create --fill          # pretty JSON envelope
libra --json=compact pr create --dry-run
libra --machine pr create --push --fill   # ndjson + no-pager + quiet（CI/脚本推荐）
```

### 接口契约与互斥规则

| 规则 | 结果 |
|------|------|
| `--head` 缺省 | 使用当前分支名 |
| `--base` 缺省 | 显式 upstream base → remote 默认分支 → `main` / `master` 探测 |
| `--body` 与 `--body-file` 同时出现 | 拒绝 |
| `--fill` 与 `--title` / `--body` / `--body-file` 同时出现 | 第一版拒绝，避免 `gh` 版本差异导致覆盖顺序不一致 |
| `--web` 与全局 `--json` / `--machine` 同时出现 | 拒绝；浏览器流程不保证可机器解析 URL，且全局 JSON 已禁用 pager/颜色语义 |
| `--web` 与 `--dry-run` 同时出现 | 拒绝，dry-run 必须无副作用 |
| `--head` 含 `:`（fork 语法 `owner:branch`） | 第一版拒绝 |
| `--push` 与非当前分支 `--head` 同时出现 | 第一版拒绝，避免推错分支或跨仓库推送 |
| 当前分支 detached HEAD | 拒绝 |
| 当前分支相对 base 没有 ahead commit | 拒绝 |
| 工作区 dirty | 默认允许但提示；`--require-clean` 时拒绝 |
| 当前分支未 push | 默认拒绝并提示；显式 `--push` 时走 Libra push |
| remote 非 GitHub | 第一版拒绝 |

说明：未来若确认 `gh` 版本行为稳定，可放宽 `--fill` 与 `--title` 的组合，但必须先固定覆盖优先级并补充兼容测试。

### 默认行为推断

- `--head`：缺省取当前分支。
- 目标 remote：优先使用当前分支 upstream remote；找不到则使用 `origin`；仍找不到则报错并要求用户配置 remote。
- `--base`：优先级为显式 `--base` → upstream merge branch → remote 默认分支 → `main` / `master` 探测。
- 当前分支未 push 时默认报错并提示 `--push`，不静默 push。
- 工作区 dirty 时默认允许创建 PR，因为 PR 关注的是已 push commit，不是工作区；但 human 输出给出提示；全局 `--json`/`--machine` 时在 `data.dirty: true` 中体现。
- 成功后输出 PR URL；`--web` 只打开浏览器，不与全局 `--json`/`--machine` 同次调用。
- 结构化输出使用 Libra 全局 `OutputConfig` envelope（`ok` + `command` + `data`），不直接透传 `gh` human output；失败时走 `CliError` / `CliErrorReport`（`LBR-*` 稳定码）。

## 数据流与控制流

`libra pr create` 按以下顺序执行：

1. 解析 CLI 参数，并在进入外部命令前完成互斥规则校验。
2. 检查当前目录是 Libra repo。
3. 检查当前分支不是 detached HEAD。
4. 推断 head branch、remote、base branch。
5. 解析 remote URL，确认 host、owner、repo，并确认第一版支持范围内是 GitHub remote。
6. 检查工作区 dirty 状态；默认记录 warning，`--require-clean` 时返回错误。
7. 计算当前 head 相对 base 是否有 ahead commit；没有 ahead commit 则拒绝。
8. 判断远端 head 分支状态（见下节「远端分支状态判断」）。
9. 若远端 head 不存在或与本地不一致：无 `--push` 时拒绝；有 `--push` 时调用 **`libra push -u <remote> <head>`**（`push -u` 要求显式 remote 名），并用 push 结果更新远端状态判断。
10. 检查 `gh` 是否存在，并确认版本满足最低支持版本。
11. 非 `--dry-run`：检查 `gh auth status --hostname <host>`；`--dry-run` 默认跳过认证与 `gh pr create`，在 JSON `data.auth_checked: false` 中标注（第 1 阶段允许仅做本地推断）。
12. 组装 `gh pr create` 参数（显式 `--repo`，Enterprise 时 host 与 auth 一致）。
13. `--dry-run`：不调用 `gh pr create`，输出将执行的动作和推断结果（含 `gh_args` 脱敏）。
14. 非 dry-run：通过 `std::process::Command` 执行 `gh pr create`，设置合理超时（建议默认 120s，可配置）。
15. 解析 PR URL；失败时将 `gh` stderr 归一化为 Libra `CliError`（`LBR-*`），并保留必要上下文。
16. 输出 human 或全局 JSON/machine 结果。

### 远端分支状态判断

不能只依赖 `refs/remotes/<remote>/<head>`，因为它可能是过期引用。推荐顺序：

1. 如果刚执行过 `libra push`（或 `libra push -u ...`）且成功，使用 push 结果作为远端已同步的依据（`remote_head_oid == local_head_oid`）。
2. 如果没有 push，调用 **`libra ls-remote --heads <remote> <head>`**（或等价 ref `refs/heads/<head>`）查询远端 tip OID，与当前 Libra HEAD 比较。
3. 如果只能读取本地 remote-tracking ref，则必须把结果标记为 `stale_risk: true`；当本地 ref 缺失或与 HEAD 不一致时，提示用户执行 `libra push -u <remote> <head>` 或使用 `--push`。
4. 第一版不要调用 `git fetch` 或让 `gh` 隐式修正远端 ref，避免绕过 Libra 的存储和 ref 语义。

### PR URL 解析

`gh pr create` 成功时通常输出 PR URL。实现应仅接受以下形式作为成功 URL：

```text
https://<host>/<owner>/<repo>/pull/<number>
```

如果 `gh` 返回成功退出码但无法解析 URL：

- human 模式输出 `gh` 成功信息并提示无法解析 URL。
- 全局 `--json`/`--machine` 模式返回结构化 `CliError`（`LBR-PR-006` 或等价码），避免输出不完整成功 schema。

## 参数映射

Libra 参数到 `gh` 参数：

| Libra 选项 | `gh` 参数 | 备注 |
|------------|-----------|------|
| `--base main` | `--base main` | base branch 名，不包含 remote 前缀 |
| `--head feature/x` | `--head feature/x` | 第一版限制为同仓库 branch |
| `--title "..."` | `--title "..."` | 与 `--fill` 互斥 |
| `--body "..."` | `--body "..."` | 与 `--fill`、`--body-file` 互斥 |
| `--body-file PR.md` | `--body-file PR.md` | 路径规范化并校验可读 |
| `--draft` | `--draft` | 布尔开关 |
| `--fill` | `--fill` | 第一版不与 title/body 混用 |
| `--web` | `--web` | 与 `--dry-run`、全局 `--json`/`--machine` 互斥 |

所有实际创建都应显式传 `--repo <host>/<owner>/<repo>` 或 `--repo <owner>/<repo>` 加 host 上下文，避免 `gh` 从当前目录误判仓库。对 GitHub Enterprise，需要按 `gh` 支持的格式传递 repo，并确保认证检查使用同一个 host。

后续可扩展的 GitHub 元数据（直接透传到 `gh`，但仍需 argv 组装测试）：

```bash
--reviewer <login>
--assignee <login>
--label <name>
--milestone <name>
--project <name>
```

## 安全边界

因为要执行外部命令，必须避免 shell 拼接。使用 `std::process::Command` 逐个传 argv：

```rust
Command::new("gh")
    .arg("pr")
    .arg("create")
    .arg("--base")
    .arg(base)
    .arg("--head")
    .arg(head);
```

不要构造字符串再交给 shell：

```rust
// 禁止：sh -c "gh pr create --title ..."
```

其他安全要求：

- `--body-file` 必须规范化路径，校验文件存在、是普通文件且可读，错误要带上下文。
- `--body-file` 允许仓库外路径，但错误信息不得泄露不必要的敏感目录内容。
- remote URL 解析不能允许 `file://`、本地路径、无 host URL、`ssh://evil` 等被误判成 GitHub。
- GitHub.com 只接受 `github.com`；Enterprise host 必须来自 remote URL，不能从不可信 PR body 或 title 推断。
- branch、title、body、label 等用户输入只作为 argv 值传递，绝不拼进命令字符串。
- branch 名虽然通过 argv 传递，也应拒绝空值、包含 NUL、换行或无法被 Libra ref 解析的值。
- 全局 `--json`/`--machine` 模式下不要打开浏览器（与 `--web` 互斥）。
- `--body-file` 建议限制可读大小（例如默认 512 KiB，可配置），防止意外读取超大文件进入 `gh` argv 或日志。
- `gh` 子进程应设置超时（建议默认 120s）与 stderr 捕获长度上限（例如 64 KiB），超时后 kill 并返回可行动错误。
- `--dry-run` 不得调用真正的 `gh pr create`，不得 push，默认不打开浏览器。
- 不要打印 token、`gh auth status` 的敏感信息、环境变量或完整 credential helper 输出。
- 捕获 `gh` stderr 时应做敏感信息过滤，再写入 Libra 错误详情。
- fake `gh` 测试必须覆盖 title/body 中包含空格、引号、分号、换行等字符的场景，证明未经过 shell。

## 功能正确性与接口兼容性

### 正确性不变量

- 创建 PR 的 head commit 必须等于当前 Libra head commit，或由刚完成的 `libra push` 明确确认已同步。
- base/head 比较必须发生在同一个 remote/repository 上；第一版不跨 fork 比较。
- 没有 ahead commit 时不得创建 PR。
- `--push` 不得推送非当前分支。
- `--dry-run` 不得产生远端副作用。
- 全局 `--json`/`--machine` 成功时必须包含 `ok: true`、`command`、`data.provider`、`data.backend`、`data.remote`、`data.repository`、`data.base`、`data.head`、`data.draft`。
- 全局 `--json`/`--machine` 失败时必须输出 `CliErrorReport`（`ok: false`、`error_code: "LBR-*"`、`category`、`hints[]`），不依赖 human 文本解析。

### CLI 兼容性

- `libra pr` 是 Libra extension，不声明为 Git-compatible 命令。
- 未来新增选项必须保持已有选项语义不变。
- 第一版拒绝不确定组合，优先稳定而不是模拟所有 `gh pr create` 行为。
- 文档、help text、compat 测试必须一起更新。

### `gh` 版本兼容性

实现时应定义最低支持版本，例如 `gh >= 2.0.0` 或经测试确认的更高版本。版本过低时返回：

```text
error: GitHub CLI version is unsupported
hint: upgrade gh to version <minimum> or newer
```

不要依赖未在最低版本中存在的 flag。若后续使用新 flag，必须提高最低版本并更新文档。

## 错误处理

把常见失败转成 Libra 风格的可行动错误：

```text
error: GitHub CLI is not installed
hint: install it from https://cli.github.com/ or use `brew install gh`
```

```text
error: GitHub CLI is not authenticated for github.com
hint: run `gh auth login --hostname github.com`
```

```text
error: current branch 'feature/x' has not been pushed to origin
hint: run `libra push -u origin feature/x` or retry with `libra pr create --push`
```

```text
error: remote 'origin' is not a GitHub remote
hint: `libra pr create` currently supports only GitHub remotes through gh
```

```text
error: no commits to propose
hint: commit changes first, or choose a different base with `--base`
```

```text
error: option conflict: --web cannot be used with --json or --machine
hint: remove one of the options and retry
```

```text
error: failed to create GitHub pull request
hint: gh reported an error; verify repository permissions and branch protection settings
```

错误分类建议：

| 类别 | 示例 | 是否可重试 |
|------|------|------------|
| 配置错误 | 缺少 remote、非 GitHub remote、gh 未安装 | 修复配置后重试 |
| 认证错误 | `gh auth status` 失败、SSO 未授权 | 重新登录或授权后重试 |
| 状态错误 | detached HEAD、无 ahead commit、未 push | 修改 Libra repo 状态后重试 |
| 参数错误 | 互斥选项、无效 branch、body-file 不可读 | 修改命令后重试 |
| GitHub API 错误 | 无权限、PR 已存在、branch protection | 根据 `gh` 上下文修复后重试 |
| 外部工具错误 | `gh` 退出码非 0、输出无法解析 | 保留 sanitized stderr 并提示诊断 |

## JSON 输出

机器可读输出通过 Libra **全局** `--json` / `--machine` 触发（`libra --json pr create`），由 `OutputConfig` + `write_json_command_envelope` 构造成功 envelope；失败由 `CliError::render_json` 输出 `CliErrorReport`。Libra 自己定义稳定 `data` schema，不透传 `gh` 的输出。

成功示例（`libra --json pr create --fill`）：

```json
{
  "ok": true,
  "command": "pr create",
  "data": {
    "provider": "github",
    "backend": "gh",
    "remote": "origin",
    "repository": "owner/repo",
    "host": "github.com",
    "base": "main",
    "head": "feature/my-change",
    "url": "https://github.com/owner/repo/pull/123",
    "number": 123,
    "pushed": true,
    "dirty": false,
    "draft": false
  }
}
```

`libra --json pr create --dry-run`：

```json
{
  "ok": true,
  "command": "pr create",
  "data": {
    "dry_run": true,
    "provider": "github",
    "backend": "gh",
    "remote": "origin",
    "repository": "owner/repo",
    "host": "github.com",
    "base": "main",
    "head": "feature/my-change",
    "would_push": false,
    "auth_checked": false,
    "dirty": false,
    "gh_args": [
      "pr", "create",
      "--repo", "owner/repo",
      "--base", "main",
      "--head", "feature/my-change",
      "--fill"
    ]
  }
}
```

失败示例（`libra --json pr create`，认证失败）——对齐 `CliErrorReport`，**不是**嵌套 `error.code` 点分串：

```json
{
  "ok": false,
  "error_code": "LBR-PR-002",
  "category": "auth",
  "exit_code": 128,
  "severity": "fatal",
  "message": "GitHub CLI is not authenticated for github.com",
  "hints": [
    "run `gh auth login --hostname github.com`"
  ]
}
```

### 建议稳定错误码（落地时登记 `docs/error-codes.md` + `compat_error_codes_doc_sync`）

| 码 | category | 场景 | 可复用现有码？ |
| --- | --- | --- | --- |
| `LBR-PR-001` | `cli` | `gh` 未安装或版本低于最低支持 | 否（外部工具缺失） |
| `LBR-PR-002` | `auth` | `gh auth status --hostname <host>` 失败 | 可近似 `LBR-AUTH-001`，但建议独立以便 hint 精准 |
| `LBR-PR-003` | `repo` | remote 非 GitHub、无法解析 owner/repo/host | 可近似 `LBR-REPO-003` |
| `LBR-PR-004` | `repo` | head 未 push / 远端 OID 与本地不一致且无 `--push` | 否 |
| `LBR-PR-005` | `cli` | 互斥选项（`--web`+`--json`、`--fill`+`--title` 等） | 可近似 `LBR-CLI-002` |
| `LBR-PR-006` | `network` | `gh pr create` 非 0、超时、或成功但无法解析 PR URL | 可近似 `LBR-NET-001` |
| `LBR-PR-007` | `conflict` | 无 ahead commit、`--require-clean` 遇 dirty、detached HEAD | 可近似 `LBR-CONFLICT-002` |

`gh_args` 是否暴露可通过 option 控制；若默认暴露，必须保证不含 secret、token、credential helper 输出或 body 内容。为避免泄露长正文，建议 `gh_args` 中将 `--body` 值替换为 `"<redacted>"`，保留 `--body-file` 路径 basename 或规范化路径的安全展示形式。

## 性能与效率

该命令不是热路径，主要性能目标是避免不必要的网络和外部进程调用：

- 参数互斥、repo 状态、dirty 检查应在调用 `gh` 前完成，尽早失败。
- `--dry-run` 默认只做本地推断，不调用 `gh pr create`，可选择是否检查 `gh --version`。
- 不默认 fetch，不默认打开浏览器，不默认 push。
- `gh --version` 与 `gh auth status` 每次执行一次即可，不需要循环调用。
- remote 默认分支查询可复用已有 Libra remote metadata；没有 metadata 时再做最小必要查询。
- JSON 输出构造应避免读取完整 diff；只需要 commit graph/ahead 判断。

## 可靠性与容错性

- 外部命令调用必须设置清晰的 stdin/stdout/stderr 策略，禁止意外交互阻塞；需要交互的场景应让用户显式使用 `--web` 或先运行 `gh auth login`。
- `gh auth status` 失败时不要继续调用 `gh pr create`。
- `libra push` 成功但 `gh pr create` 失败时，不能回滚 push；应提示分支已推送但 PR 未创建。
- `gh pr create` 返回“PR already exists”时，应尽量解析已有 PR URL；解析失败则返回专门错误并提示 `gh pr view` 或后续 `libra pr view`。
- 如果创建命令超时或被中断，不能假设 PR 未创建；错误提示应建议用户用 GitHub 页面或 `gh pr list --head <branch>` 检查。
- 所有用户可见错误必须带上下文和下一步建议。

## 兼容性与互操作性

- GitHub.com：支持 `git@github.com:owner/repo.git`、`https://github.com/owner/repo.git`、`ssh://git@github.com/owner/repo.git`。
- GitHub Enterprise：支持 `git@github.example.com:owner/repo.git`、`https://github.example.com/owner/repo.git`、`ssh://git@github.example.com/owner/repo.git`，前提是 `gh auth status --hostname github.example.com` 成功。
- 非 GitHub remote：第一版明确拒绝，不尝试猜测 GitLab/Gitea API。
- Fork PR：第一版拒绝 `--head owner:branch` 或标记为 experimental；后续支持时必须明确 push remote、head owner、base repo 的关系。
- CI/脚本：推荐使用 `libra --machine pr create`（或 `libra --json pr create`），并依赖 `error_code`（`LBR-*`）而不是 human message。
- TTY/非 TTY：非 TTY 下默认禁止需要交互的流程；`--web` 在无 GUI 环境下应返回可行动错误。

## 可扩展性与可维护性

不要把 `gh` 调用直接塞进 `src/command/pr.rs` 的大函数里，建议分层：

```text
src/command/pr.rs            CLI 参数、输出格式、错误展示
src/internal/github/mod.rs   GitHub remote 识别、owner/repo/host 解析
src/internal/github/gh.rs    gh 可用性检查、auth status、pr create 调用
src/internal/pr.rs           PR 创建前置校验、base/head 推断、调用 provider
```

可抽象一个轻量 trait，但第一版不必过度设计：

```rust
trait PullRequestProvider {
    async fn create(&self, request: CreatePullRequestRequest) -> Result<CreatePullRequestResponse>;
}
```

当前只实现 `GhGitHubProvider`。未来如需原生 API 或其他平台，可新增 `NativeGitHubProvider` / `GitLabProvider` / `GiteaProvider`。第一版可以先不引入 trait，除非测试需要 mock；如果不引入 trait，也应至少把外部命令执行封装成可注入 runner，便于 fake `gh` 测试。

建议核心数据结构：

```rust
struct CreatePullRequestRequest {
    provider: PullRequestProviderKind,
    backend: PullRequestBackendKind,
    host: String,
    repository: String,
    remote: String,
    base: String,
    head: String,
    title: Option<String>,
    body: Option<String>,
    body_file: Option<PathBuf>,
    draft: bool,
    fill: bool,
    web: bool,
    push: bool,
    dry_run: bool,
}
```

## 合规性与标准符合性

- 该命令是托管平台扩展，不应伪装成 Git 标准命令；必须在 `COMPATIBILITY.md` 标注为 Libra extension。
- commit 流程仍遵守现有 Libra/Git 兼容约束，PR 命令不改变 commit、签名、DCO 或 ref 规则。
- 不新增 token 存储，因此不引入新的凭据持久化合规面；凭据生命周期由 `gh` 管理。
- 日志和错误信息不得泄露 token、cookie、authorization header、credential helper 输出或用户正文中的敏感内容。
- 文档应说明真实 GitHub live test 需要显式 env gate（`LIBRA_TEST_GITHUB_TOKEN` + `--features test-network`），默认 CI 不访问外网。

## 推荐默认 UX

最理想的一条命令体验：

```bash
libra pr create --fill --push
```

行为：

1. 检查当前分支。
2. 自动推送当前分支到目标 remote 并设置 upstream（`libra push -u <remote> <branch>`）。
3. 调用 `gh pr create --fill`。
4. 输出 PR URL。

典型输出：

```text
Pushed feature/my-change to origin.
Created pull request:
https://github.com/owner/repo/pull/123
```

若不想自动 push：

```bash
libra pr create --fill
```

当前分支未 push 时：

```text
error: current branch 'feature/my-change' has not been pushed to origin
hint: run `libra push -u origin feature/my-change` or retry with `libra pr create --push --fill`
```

## 测试策略

### 单元测试

- GitHub remote URL 解析：
  - `git@github.com:owner/repo.git`
  - `https://github.com/owner/repo.git`
  - `ssh://git@github.com/owner/repo.git`
  - GitHub Enterprise host
  - 非 GitHub remote 拒绝
  - `file://` 与本地路径拒绝
- base / head / remote 推断。
- `gh` argv 组装（不使用 shell）。
- `--body` / `--body-file` / `--fill` 冲突规则。
- `--web` 与全局 `--json`/`--machine`、`--dry-run` 冲突规则。
- branch 名、host、repo 名校验。
- `--dry-run` 不执行外部创建、不 push、不打开浏览器。
- PR URL 解析与“成功但无法解析 URL”错误路径。
- 全局 `--json`/`--machine` 成功 envelope 与 `CliErrorReport` 稳定性（含 Display pin 测试）。

### 集成测试

- fake `gh` binary 放到临时 `PATH`，记录 argv。
- fake `gh` 返回成功 URL，验证 `libra --json pr create` / human 输出。
- fake `gh` 返回 auth 失败，验证 `LBR-PR-002` 与 hints。
- fake `gh` 返回非 0，验证 Libra 不吞错误。
- fake `gh` 输出包含疑似 token，验证错误过滤。
- fake `gh` 返回“PR already exists”，验证已有 PR 处理。
- `--push` 时验证调用的是 Libra push 逻辑（`libra push -u <remote> <branch>`），或在测试中 mock push 层。
- 远端 tracking ref 过期时验证不误判为已同步（应走 `ls-remote --heads` 或标 `stale_risk`）。
- dirty workspace 默认 warning，`--require-clean` 拒绝。
- 非 TTY 下 `--web` 的错误行为。

不建议默认 CI 依赖真实 GitHub 网络。可选 **L2** live 测试（与 `network_remotes_test` 同 gate）：

```bash
# 需 LIBRA_TEST_GITHUB_TOKEN + LIBRA_TEST_GITHUB_NAMESPACE
cargo test --features test-network --test pr_github_live_test
```

登记 `tests/INDEX.md`（Wave 3，network）时注明 env gate；**不要**引入不存在的 `test-live-github` Cargo feature。

### 验收门槛

- `cargo +nightly fmt --all --check`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo test --all`
- 新增或更新 `tests/INDEX.md` 中对应 integration target。
- CLI help、命令文档、错误码文档和兼容性文档同步。

## 文档更新

落地时需同步：

- 新增 `docs/commands/pr.md` 与 `docs/commands/zh-CN/pr.md`（含 Examples / Common Commands 小节）。
- `src/command/pr.rs` 定义 `PR_EXAMPLES` 并挂到 `#[command(after_help = …)]`（满足 `compat_help_examples_banner`）。
- `COMPATIBILITY.md`：标记 `pr` 为 Libra GitHub extension，非 Git 命令。
- `docs/error-codes.md`：登记 `LBR-PR-*`（或明确复用 `LBR-AUTH`/`LBR-NET`/`LBR-REPO-003` 的映射表）。
- README command list 更新。
- `tests/INDEX.md`（若新增 integration target）。
- CLI help snapshot 或 compat 测试（`compat_help_examples_banner`、`compat_command_docs_examples_section`、根 help 命令组行）。

## 分阶段落地

### 第 1 阶段：只读准备能力

```bash
libra pr create --dry-run
```

完成 remote 解析、base / head 推断、互斥校验、gh 可用性检查、argv 生成，不真正创建 PR，不 push。

### 第 2 阶段：最小可创建

```bash
libra pr create --base main --title "..." --body "..."
libra pr create --fill
```

要求分支已 push，不自动 push。支持 fake `gh` 集成测试与 `CliErrorReport` JSON 失败路径。

### 第 3 阶段：自动 push

```bash
libra pr create --push --fill
```

用 `libra push -u <remote> <branch>` 推送，成功后调用 `gh`。如果 push 成功但 PR 创建失败，明确提示分支已推送。

### 第 4 阶段：完整 GitHub UX

```bash
libra pr create --draft --reviewer alice --label bug --web
libra pr status
libra pr view
libra pr checkout
```

增加 reviewer、label、assignee、milestone、project 等 metadata，并补齐 PR 查询和 checkout 能力。

### 第 5 阶段：多 provider 与 fork PR

```bash
libra pr create --head owner:feature --base main
```

在明确 push remote、base repo、head repo、权限与 URL schema 后，再支持 fork PR、native GitHub API、GitLab 或 Gitea。

## 结论

长期最优解不是让用户手动组合 `libra push` + `gh pr create`，而是在 Libra 里提供稳定的 `libra pr create` 门面：

```bash
libra pr create --push --fill
```

内部规则：

- Libra 负责仓库状态和 push。
- `gh` 负责 GitHub PR 创建。
- 不通过 shell 调用 `gh`。
- 默认不静默 push，必须显式 `--push`。
- 机器可读输出使用 Libra 全局 `--json`/`--machine` 与 `CliErrorReport`（`LBR-*`），不在 `pr create` 上重复定义 `--json`。
- 第一版只支持 GitHub 同仓库 PR，非 GitHub remote 和 fork PR 明确拒绝或延后。
- 远端状态判断以 push 结果或 `libra ls-remote --heads` 为准，不单独依赖可能过期的 tracking ref。
- 错误必须可行动，且不泄露凭据或敏感正文。

这能在很低实现成本下获得长期可维护的 GitHub PR 能力，同时不把 Libra 绑定到一套自维护的 GitHub API / auth 实现。
