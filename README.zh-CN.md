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
> cowbox 目前是 **M1.5 预览版**。引擎运行在 Linux/WSL2，Windows 提供原生启动器
> 并委托到 WSL。MCP 代理、Windows 原生文件编辑后端、macOS 和网络隔离属于后续
> 里程碑。分阶段计划见 [docs/ROADMAP.md](docs/ROADMAP.md)。

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
cowbox demo                  # 在隔离 fixture 中端到端验证回滚
cowbox diff                  # 查看净变更与沙盒副作用
cowbox log                   # 查看 step DAG（含分支）
cowbox rollback <step>       # 只移动 head 指针，不写宿主机
cowbox undo [n]              # 回退 n 步的快捷方式
cowbox commit                # 把净变更原子写回宿主机
cowbox commit --recover      # 处理中断 commit：恢复宿主机或完成会话收尾
cowbox discard               # 丢弃沙盒状态
cowbox doctor                # 检查内核、OverlayFS、xattr、Landlock
cowbox stats                 # 查看步数、回滚、孤儿进程、磁盘占用
cowbox sessions              # 列出所有项目的 active 与 finalized session
cowbox gc --dry-run          # 预览可清理的旧 session 状态
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
- `rollback` 只是移动 DAG head 指针，旧层仍可用于审计和分支。
- `commit` 计算 base→head 净变更，检查宿主机漂移，备份到 journal，再按安全顺序落盘。
- commit 会把 SQLite 的 `-wal`/`-shm`/`-journal` sidecar 与数据库一起处理，保留
  `user.*` xattr，并保留同一次 commit 内文件之间的硬链接。journal 中持久化的
  `applied` 阶段是提交点：在此之前中断，`cowbox commit --recover` 恢复宿主机；
  在此之后中断，它保留已提交内容并完成会话收尾。
- Landlock 拒绝 workspace 与 session 私有 home/tmp/cache 之外的写入；PID namespace
  在步骤结束时回收后台子进程。

完整调用链、whiteout 规则与 commit 顺序见 [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)。

## Windows 快速开始

下载 Windows release zip，解压 `cowbox.exe`，在终端运行：

```powershell
.\cowbox.exe setup
cowbox doctor
cowbox demo
cowbox install-skill --list
cowbox install-skill --harness codex
```

`setup` 只安装 Windows 启动器和 WSL 里的 Linux 引擎，**不会自动安装 skill**；
`install-skill` 是显式 opt-in 步骤。项目可以继续留在 `E:\...`，启动器会自动
映射到 WSL 的 `/mnt/e/...`。

`cowbox demo` 会在 `/tmp` 创建隔离 fixture，执行破坏性命令、预览 commit、
回滚并校验文件恢复，全程不触碰当前项目。使用 `--keep` 可保留现场，使用
`--json` 可获得机器可读结果。

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

## 安全模型

- M1 使用 **Landlock ABI 3** 做路径级读写围栏，使用 **PID namespace** 与 PID 1
  supervisor 做进程树回收。
- 活跃 session 期间，宿主机 workspace 视为**只读**。lowerdir 是实时目录，因此
  commit 通过 base manifest 检测外部修改；默认拒绝，`--force` 也会先备份。
- M1 **不隔离网络**。沙盒内进程可以绑定宿主机端口、访问网络。网络策略计划在 M2。
- M1 防的是**误操作和 Agent 失控**，不是恶意代码的硬化沙盒。内核漏洞与蓄意恶意
  二进制不在威胁模型内。
- `cowbox commit` 在终端中要求人工确认；非交互调用只有在取得人工批准后使用
  `cowbox commit --yes`。
- 硬链接在同一次 commit 的文件集合内保留。成功 commit 后，指向未提交宿主机文件
  的硬链接会变成独立 inode；失败回滚在文件系统允许时会使用硬链接备份恢复原链接。
- commit 备份会保留到恢复后的 base manifest 与 journal 阶段、或 committed 会话
  记录与 active 指针清除均已持久化之后。恢复发现受影响路径被外部修改时会报错并
  保留备份材料。这里保证的是进程中断恢复，不包括断电或存储故障保证。
- 只保留 `user.*` xattr；其他 xattr 命名空间由内核或安全模块管理，不会被改写。
- `.git` 永远不提交，即使使用 `--include-excluded`。device、FIFO、socket 拒绝提交；
  setuid/setgid 位在 commit 时清除。
- OverlayFS 不可用时，cowbox 不会静默降级为全量目录复制，而是返回明确错误。

完整威胁模型见 [docs/SECURITY-MODEL.md](docs/SECURITY-MODEL.md)。

## 基准测试

只发布真实机器上测出的数字。测试方法与脚本见 [docs/BENCHMARKS.md](docs/BENCHMARKS.md)。

| 场景 | Fixture | 目标（P99） | 实测 |
| --- | --- | --- | --- |
| 冷启动 OverlayFS 挂载 | 10,000 文件 | < 5 ms | TBD |
| 单步冻结 | 10,000 文件 | < 10 ms | TBD |
| Diff 扫描（10 个变更） | 10,000 文件 | < 25 ms | 13.19 ms（12.96–13.56 ms） |
| 回滚（DAG 指针） | 10,000 文件 | < 5 ms | 10.7 ns（10.44–11.01 ns） |
| 端到端 `cowbox run -- true` | 10,000 文件 | 无硬目标 | 56.2 ms（51.76–60.88 ms，包含 CLI/namespace/mount） |

这些是目标，不是结论。实测列由 `scripts/bench.sh` 在 WSL2 ext4 上跑完后填写。

## 状态与路线图

| 里程碑 | 范围 | 状态 |
| --- | --- | --- |
| **M1** | Linux/WSL2 CLI、快照 DAG、OverlayFS、Landlock、commit journal、测试、基准 | 预览可用 |
| **M1.5** | Windows 启动器、`setup`、多 harness skill 安装、项目规则与策略 | 预览可用 |
| **M2** | 网络策略（seccomp 或更新 Landlock）、缓存 bind mount、20 万文件基准、blocked-writes 审计 | 计划中 |
| **M3** | MCP 代理（`cowbox_diff` / `cowbox_rollback` / `cowbox_commit`） | 计划中 |
| **M4** | Windows 原生 ReFS + Job Object 后端、macOS 后端、Eval 套件与公开榜单 | 计划中 |

M1 明确不包含 MCP、Windows 原生、macOS、网络隔离和多 Agent 赛马。边界以
[roadmap](docs/ROADMAP.md) 为准。

## 参与贡献

请先读 [CONTRIBUTING.md](CONTRIBUTING.md) 与 [AGENTS.md](AGENTS.md)。一句话版本：
引擎必须确定性、禁止静默降级、对抗测试必须在 WSL ext4 跑、禁止发布未实测的基准数字。

安全漏洞请使用 GitHub private vulnerability reporting，不要开公开 issue。见
[SECURITY.md](SECURITY.md)。

## 许可证

Apache-2.0。见 [LICENSE](LICENSE) 与 [NOTICE](NOTICE)。
