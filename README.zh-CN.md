<div align="center">

# cowbox

### 面向 AI 编程 Agent 的毫秒级写时复制沙盒

<p>
  <a href="https://github.com/BangShou1st/cowbox/actions/workflows/ci.yml">
    <img src="https://github.com/BangShou1st/cowbox/actions/workflows/ci.yml/badge.svg?branch=main" alt="CI" />
  </a>
  <img src="https://img.shields.io/badge/license-Apache--2.0-blue.svg" alt="License" />
  <img src="https://img.shields.io/badge/rust-1.85%2B-orange.svg" alt="Rust" />
  <img src="https://img.shields.io/badge/platform-Linux%20%2F%20WSL2%20%2F%20Windows-lightgrey.svg" alt="Platform" />
</p>

**让 Agent 放手改，毫秒级回滚，宿主机保持干净。**

[English](README.md) | [简体中文](README.zh-CN.md)

</div>

> [!IMPORTANT]
> cowbox 目前是 **M3 Agent 快照预览版**。引擎运行在 Linux/WSL2，Windows 提供原生
> 启动器并委托到 WSL。`mcp-proxy` 将 stdio MCP server 的每次工具调用变成可检查、
> 可回滚的 cowbox step。Windows 原生文件编辑后端和 macOS 属于后续里程碑。
> 分阶段计划见 [docs/ROADMAP.md](docs/ROADMAP.md)。

## 为什么需要 cowbox

AI 编程 Agent 很擅长改代码，却不擅长判断什么时候该停手。传统恢复手段各有盲区：

| 故障场景 | `git checkout` / worktree | Docker / VM | cowbox |
| --- | --- | --- | --- |
| Agent 改坏未跟踪或 ignored 文件 | 管不到 | 能管，但环境不是原环境 | CoW 层覆盖 |
| Agent 损坏本地 SQLite 数据库 | 管不到 | 需要额外 volume 配置 | 当普通文件处理 |
| Agent 留下 dev server / worker | 管不到 | 删容器才能清 | PID namespace 在步骤结束时回收 |
| Agent 装坏依赖树 | 恢复慢 | 冷启动慢、丢失本地状态 | 新增 upper 层，瞬时回退 |
| 人类想先审查副作用再决定是否保留 | 不完整 | 不顺手 | `cowbox diff` 后再 `cowbox commit` |

cowbox 不是容器替代品，而是你正在编辑的 workspace 的**本地安全网**：真实工具链、
真实缓存、真实项目，外加一个成本极低的撤销按钮。

## 核心能力

```bash
cowbox run -- <command>      # 在 CoW 视图中执行命令；每次执行生成一个 step
cowbox run --network off ... # 同时在独立网络命名空间中隔离 IP 连通性
cowbox demo                  # 在隔离 fixture 中端到端验证回滚
cowbox demo --offline        # 用 network=off 真实执行、diff 并回滚
cowbox diff                  # 查看净变更与沙盒副作用
cowbox log                   # 查看 step DAG（含分支）
cowbox rollback <step>       # 只移动 head 指针，不写宿主机
cowbox undo [n]              # 回退 n 步的快捷方式
cowbox commit                # 把净变更原子写回宿主机
cowbox commit --recover      # 处理中断 commit：恢复宿主机或完成会话收尾
cowbox discard               # 丢弃沙盒状态
cowbox doctor                # 检查内核、OverlayFS、xattr、Landlock
cowbox doctor --check-run --network off # 走真实启动链路做主动诊断
cowbox stats                 # 查看逻辑步数、实际挂载层、磁盘占用
cowbox sessions              # 列出所有项目的 active 与 finalized session
cowbox gc --dry-run          # 预览可清理的旧 session 状态
cowbox mcp-proxy -- <server> # 让 stdio MCP server 的每次调用都经过 cowbox 快照
```

所有命令支持 `--json`；`run` 还支持 `--trace-chrome` 导出 namespace、mount、
diff、commit 的 Chrome Trace JSON。

## 工作原理

```text
┌──────────────────────────────────────────────────────────────┐
│ cowbox CLI（薄层）                                           │
│ init · run · diff · log · rollback · commit · doctor · stats │
└───────────────┬───────────────────────────────┬──────────────┘
                │                               │
┌───────────────▼──────────────┐  ┌─────────────▼──────────────┐
│ cowbox-core（Linux 引擎）    │  │ cowbox-dag（纯状态机）     │
│ namespaces · OverlayFS       │  │ Step 0 → Step N · 分支     │
│ Landlock · PID supervisor    │  │ rollback · undo · validate │
│ whiteout diff · commit       │  └────────────────────────────┘
└───────────────┬──────────────┘
                │
┌───────────────▼──────────────────────────────────────────────┐
│ 状态目录：$COWBOX_HOME / XDG_STATE_HOME / ~/.local/state/cowbox│
│ base manifest · 每步 upperdir · journal · events             │
└──────────────────────────────────────────────────────────────┘
```

- 真实 workspace 作为只读 **lowerdir**。
- 每个 step 在 WSL/Linux ext4 上获得一个可写 **upperdir**。
- step 结束时卸载即冻结；下一步把之前的 upper 层叠在下面。
- 长会话会压缩旧的物理挂载层，但保留全部逻辑 DAG step；压缩结果由 upper 与
  base manifest 推导、先验证再原子发布。`diff`、`commit`、回滚和分支始终使用
  原始逻辑层。
- `rollback` 只是移动 DAG head 指针，旧层仍可用于审计和分支。
- `commit` 计算 base→head 净变更，检查宿主机漂移，备份到 journal，再按安全顺序落盘。
- commit 会把 SQLite 的 `-wal`/`-shm`/`-journal` sidecar 与数据库一起处理，保留
  `user.*` xattr，并保留同一次 commit 内文件之间的硬链接。journal 中持久化的
  `applied` 阶段是提交点：在此之前中断，`cowbox commit --recover` 恢复宿主机；
  在此之后中断，它保留已提交内容并完成会话收尾。
- Landlock 拒绝 workspace 与 session 私有 home/tmp/cache 之外的写入；PID namespace
  在步骤结束时回收后台子进程。
- `--network off` 在 UID/GID 映射完成、OverlayFS 建好之后、用户命令启动之前创建
  Linux 网络命名空间；创建失败直接报错，不会回退到 host namespace。
- `cowbox mcp-proxy` 维持 Agent 的 MCP 连接，在一次性沙盒中完成工具发现；每次
  `tools/call` 都启动新的沙盒上游进程，并生成实际 step。外层协议暴露上游工具，
  以及只读的 `cowbox_diff`、只移动指针的 `cowbox_rollback`、只做 dry-run 的
  `cowbox_commit`。

完整调用链、whiteout 规则与 commit 顺序见 [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)。

## Windows 快速开始

下载 Windows release zip，解压 `cowbox.exe`，在终端运行：

```powershell
.\cowbox.exe setup
cowbox doctor
cowbox demo
cowbox demo --offline
cowbox install-skill --list
cowbox install-skill --harness codex
```

`setup` 只安装 Windows 启动器和 WSL 里的 Linux 引擎，**不会自动安装 skill**；
`install-skill` 是显式 opt-in 步骤。项目可以继续留在 `E:\...`，启动器会自动
映射到 WSL 的 `/mnt/e/...`。

`cowbox demo` 会在 `/tmp` 创建隔离 fixture，执行破坏性命令、预览 commit、
回滚并校验文件恢复，全程不触碰当前项目。使用 `--keep` 可保留现场，使用
`--json` 可获得机器可读结果。`demo --offline` 会用 `network=off` 完成真实执行、
diff 和回滚。

在项目里：

```powershell
cd E:\project\your-app
cowbox init --agent
cowbox run -- <command>
```

`init --agent` 会写 `.cowbox.toml`，以及 AGENTS.md、CLAUDE.md、Cursor 规则块。
默认触发模式是 `ask`，可用 `cowbox policy --json` 查看。

## WSL2 快速开始

### 前置条件

- Windows 11 + WSL2 + Ubuntu 24.04。
- Linux kernel 5.11+（已测试 WSL2 `6.6.x`）。
- 无特权 user namespace 与 OverlayFS：

```bash
unshare -rm echo ok
grep -w overlay /proc/filesystems
```

- 状态目录必须位于支持 xattr 的文件系统（WSL2 的 ext4）。不要把 `COWBOX_HOME`
  放到 `/mnt/c` 或 `/mnt/e`。

### 构建

```bash
git clone https://github.com/BangShou1st/cowbox.git
cd cowbox

rustup toolchain install stable --profile minimal
rustup component add rustfmt clippy
export CARGO_TARGET_DIR="$HOME/.cache/cowbox-target"
cargo install --path crates/cowbox-cli

cowbox doctor
cowbox demo
```

如果源码在 `/mnt/e`，请让 `CARGO_TARGET_DIR` 留在 WSL 文件系统。通过 9p 编译到
NTFS 又慢又没有收益。

### 试一下

```bash
cd /path/to/your/project
cowbox init

# 故意执行破坏性命令，但只发生在沙盒里。
cowbox run -- sh -c 'rm -rf src data/dev.sqlite'

cowbox diff --stat

# 实验有价值就保留：
# cowbox commit
#
# 否则回滚，宿主机完全不动：
cowbox rollback 0
```

`cowbox run` 默认从 workspace 根目录执行命令。请用 `--` 分隔 cowbox 参数与命令。
命令失败也会生成 step，方便检查或回滚。

commit 或 discard 后的 session 会保留用于审计。`cowbox sessions` 可查看磁盘占用，
`cowbox gc` 默认删除 7 天前的 finalized session，绝不会删除 active session。
`--all` 忽略时间阈值，`--dry-run` 预览，`--yes` 用于非交互执行。

### 网络策略

`cowbox run --network host|off` 为当前 step 选择网络策略。优先级为 CLI →
项目配置 → `host`：

```toml
[sandbox]
network = "off"
```

`off` 使用独立 Linux 网络命名空间，没有宿主网络接口，loopback 保持 DOWN，因此
对宿主和外部的 IPv4、IPv6、TCP、UDP 连接都会失败。这是 IP 连通性隔离，不是
防火墙，也不是 seccomp 网络策略。

基于文件路径的 Unix-domain socket 属于 IPC 边界：只要 Landlock 允许该路径，它
在 network namespace 之间仍可访问。需要禁止 Agent 访问某个 Unix socket 时，应
使用 Landlock 路径规则。

每个逻辑 step 会记录实际生效的策略。`run --json` 在 `data.policy` 中返回，
`stats --json` 返回当前快照的策略。没有记录字段的旧会话仍可读取，并报告
`"recorded": false` 与 network `"unknown"`。

```bash
cowbox run --json --network off -- python3 ./offline_job.py
cowbox stats --json
cowbox doctor --check-run --network off --json
```

`doctor --check-run` 保留原有静态诊断，并在临时 fixture 中走真实 helper、
namespace、OverlayFS、状态盘、Landlock 和用户命令路径。`run_check.stages` 会区分
user namespace、network namespace、OverlayFS、状态盘和 Landlock 故障。网络
命名空间创建失败返回 `NETWORK_NAMESPACE_FAILED`、退出码 125，并报告
`command_started: false`。

### MCP Agent 快照

`mcp-proxy` 在 stdio 上维持 MCP 客户端连接，并为每次上游工具调用创建真实
cowbox step：

```bash
cowbox mcp-proxy --network off -- python3 /path/to/your/mcp-server.py
```

在 Agent harness 中把它配置为 MCP 命令，例如：

```json
{
  "mcpServers": {
    "cowbox": {
      "command": "/home/you/.cargo/bin/cowbox",
      "args": [
        "mcp-proxy",
        "--network", "off",
        "--",
        "python3", "/path/to/your/mcp-server.py"
      ]
    }
  }
}
```

首次 `tools/list` 会在 `/tmp` 下启动一次性沙盒 session，完成上游 `initialize`
握手和分页 `tools/list`，随后删除临时状态，不会污染项目 active session。

每个上游 `tools/call` 都在当前 workspace session 中新建 step，启动全新沙盒上游
进程，完成一次 initialize/call 后退出。需要跨调用保存状态的上游 server 应把状态
写进 workspace 文件。调用串行执行，实际生效的网络策略与 Landlock 规则仍然适用；
结果元数据包含 `head_before`、实际 `step`、`head_after`、超时状态和回滚命令。

始终保留三个控制工具：

| 工具 | 行为 |
| --- | --- |
| `cowbox_diff` | 只读净变更；默认当前 head |
| `cowbox_rollback` | 移动 session head；不写宿主机 |
| `cowbox_commit` | 复用现有 commit dry-run；返回目标 step 与终端命令 |

`cowbox_commit` 永不写宿主机。人工检查后，由用户在同一 workspace 的终端执行
工具返回的 `cowbox commit --step <n> --session <id>` 并完成原有交互确认。

控制命令会显式绑定代理的 session：如果它已不是该 workspace 的 active 指针，
CLI 会拒绝操作，不会误改另一个 session。显式 `--network host` 也会覆盖项目里的
`network = "off"`。

首版 MCP 只支持 stdio 工具型上游；跨调用状态通过 workspace 文件保存，不维持长驻
上游进程。HTTP/SSE、resources、prompts 和持久 MCP 子进程尚未实现。代理行为由
`--timeout`、`--discovery-timeout`、`--max-response-bytes`、`--session`、
`--workspace` 和可重复的 `--allow-read` 约束。

### 长会话与宿主缓存

跨过物理层数或 lowerdir 长度限制时，cowbox 会自动压缩当前分支最旧的一段祖先层，
保留近期层不压缩。只有压缩记录的逻辑层列表与当前分支祖先链完全匹配时才会复用。

Cargo、npm 和 pip 可以在 `.cowbox.toml` 中显式复用宿主缓存：

```toml
[sandbox.host_caches]
cargo = "/home/you/.cargo"     # 只挂载 registry/ 与 git/
npm = "/home/you/.npm"         # 只挂载 _cacache/
pip = "/home/you/.cache/pip"   # 挂载缓存数据，包括 wheelhouse/
```

`true` 使用宿主 home 下的默认缓存，`false` 保持原来的纯隔离模式。缓存数据以只读
lowerdir 挂载，索引刷新、新下载和解包写入会话自己的 upper 缓存；宿主凭据和工具
配置仍留在隔离 HOME 中。`cowbox doctor` 会列出配置的缓存源及可用状态。pip 的
`wheelhouse/` 会通过 `PIP_FIND_LINKS` 暴露，支持带哈希锁定的离线安装。

`scripts/offline_workflow.py` 只生成本地 fixture，验证 Cargo 本地目录源、npm
`_cacache`、pip wheelhouse 的冷缺失与预热命中，以及会话缓存写入、宿主缓存哈希
不变、diff 和 undo。所有执行都使用 `--network off`，不依赖公网包仓库：

```bash
python3 scripts/offline_workflow.py \
  --binary "$COWBOX_BIN" \
  --json-output "$HOME/cowbox-bench-results/offline.json"
```

## 安全模型

- cowbox 使用 **Landlock ABI 3** 做路径级读写围栏，使用 **PID namespace** 与 PID 1
  supervisor 做进程树回收。
- 活跃 session 期间，宿主机 workspace 视为**只读**。lowerdir 是实时目录，因此
  commit 通过 base manifest 检测外部修改；默认拒绝，`--force` 也会先备份。
- 默认网络策略是 `host`：进程可以绑定宿主端口并访问网络。`run --network off`
  通过独立 Linux 网络命名空间移除 IP 连通性。它不是防火墙，不能按目标过滤，
  也不隔离基于文件路径的 Unix socket。
- cowbox 防的是**误操作和 Agent 失控**，不是恶意代码的硬化沙盒。内核漏洞与蓄意恶意
  二进制不在威胁模型内。
- `cowbox commit` 在终端中要求人工确认；非交互调用只有在取得人工批准后使用
  `cowbox commit --yes`。
- 硬链接在同一次 commit 的文件集合内保留。成功 commit 后，指向未提交宿主机文件
  的硬链接会变成独立 inode；失败回滚在文件系统允许时会使用硬链接备份恢复原链接。
- commit 备份会保留到恢复后的 base manifest 与 journal 阶段、或 committed 会话
  记录与 active 指针清除均已持久化之后。恢复发现受影响路径被外部修改时会报错并
  保留备份材料。这里保证的是进程中断恢复，不包括断电或存储故障保证。
- 只保留 `user.*` xattr；其他 xattr 命名空间由内核或安全模块管理，不会被改写。
- 宿主缓存只读且只暴露缓存数据目录；会话写入 copy-up 到会话缓存。
- `.git` 永远不提交，即使使用 `--include-excluded`。device、FIFO、socket 拒绝提交；
  setuid/setgid 位在 commit 时清除。
- OverlayFS 不可用时，cowbox 不会静默降级为全量目录复制，而是返回明确错误。

完整威胁模型见 [docs/SECURITY-MODEL.md](docs/SECURITY-MODEL.md)。

## 基准测试

只发布真实机器上测出的数字。测试方法与脚本见 [docs/BENCHMARKS.md](docs/BENCHMARKS.md)。

最终 M2 离线执行轮在 WSL2 ext4 上比较合并基线 `29af7e7` 与网络策略/离线候选
`fb4dad0`，
每个场景使用 20 个样本、1 个不计入统计的预热样本，并且每个样本都使用全新会话。
该代码对应已发布的 `v0.5.0-m2` 预览版。

| 场景 | 10,000 文件 | 200,000 文件 |
| --- | ---: | ---: |
| 首次 `cowbox run -- true`（含清单采集） | 29.92 / 31.19 ms | 124.45 / 129.62 ms |
| 固定 1 个父层的后续 run | 23.17 / 25.90 ms | 23.86 / 25.65 ms |
| 10 个文件变更的 `cowbox diff` | 5.63 / 5.80 ms | 89.98 / 94.12 ms |

表中是完整 CLI 墙钟耗时的中位数 / P95。10,000 与 200,000 文件首次 run 的
60/80 ms、150/200 ms 目标均通过，10 文件 diff 的 20 ms 与 200 ms 中位数目标也通过。
真实 300 步会话包含 279 个 `host`、21 个 `off` step，在第 151、268 步自动压缩，
并完成回滚、分支、提交和中断恢复。网络命名空间为 10k 首次 run 中位数增加
0.47 ms，为一父层 run 增加 0.38 ms。

`v0.6.0-m3` 候选版又用已发布的 `v0.5.0-m2` Linux 资产做了 20 样本 + 1 预热的
同机对照：10k 文件首次 run 中位数 / P95 为 35.60 / 38.72 ms；200k 文件为
138.06 / 150.63 ms。10 文件 diff 在 10k / 200k 下为 10.04 / 14.87 ms 与
97.33 / 101.12 ms。MCP 首次握手为 84.511 / 85.407 ms；同一会话连续工具调用为
122.918 / 142.938 ms，其中每次调用都包含新沙盒 step 与上游进程握手。

完整统计、Fixture、机器信息、口径和复跑命令见
[docs/BENCHMARKS.md](docs/BENCHMARKS.md)。P99 尚未单独测量。

## 状态与路线图

| 里程碑 | 范围 | 状态 |
| --- | --- | --- |
| **M1** | Linux/WSL2 CLI、快照 DAG、OverlayFS、Landlock、commit journal、测试、基准 | 预览可用 |
| **M1.5** | Windows 启动器、`setup`、多 harness skill 安装、项目规则与策略 | 预览可用 |
| **M2** | 长会话、网络策略、离线诊断/工作流、只读宿主缓存、20 万文件基准 | 预览可用 |
| **M3** | MCP 快照代理、diff/rollback/commit 控制工具、真实 harness 工作流 | 预览可用 |
| **M4** | Windows 原生 ReFS + Job Object 后端、macOS 后端、Eval 套件与公开榜单 | 计划中 |

M1 明确不包含 MCP、Windows 原生、macOS、网络隔离和多 Agent 赛马。M2 增加可选
网络命名空间隔离，但不宣称包过滤或恶意代码隔离。M3 为工具型 stdio MCP server
增加快照拦截，不对进程内或 HTTP 上游宣称协议级隔离。边界以
[roadmap](docs/ROADMAP.md) 为准。

## 参与贡献

请先读 [CONTRIBUTING.md](CONTRIBUTING.md) 与 [AGENTS.md](AGENTS.md)。一句话版本：
引擎必须确定性、禁止静默降级、对抗测试必须在 WSL ext4 跑、禁止发布未实测的基准数字。

安全漏洞请使用 GitHub private vulnerability reporting，不要开公开 issue。见
[SECURITY.md](SECURITY.md)。

## 许可证

Apache-2.0。见 [LICENSE](LICENSE) 与 [NOTICE](NOTICE)。
