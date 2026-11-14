# Omarchy 适配器

让对讲机遥控 Omarchy（Hyprland）笔记本：窗口和工作区、scratchpad、Claude Code / Codex / Kimi Code 会话、语音指令。[English](README.md)

**Linux 加上本适配器是正式支持的平台。** 每个 pull request 和每次推送到 `main`，CI 的 “Linux host” 任务都会在 Ubuntu 24.04 上为 Linux 构建 host 工作区并运行它的测试（除 Mac 应用以外的所有 crate：`deckd`、`deck-proto`、`deck-model`、`deck-client`、`deck-gui-core` 和 `deck-wrap`），再运行本目录的 Python 测试（见[测试](#测试)）。窗口应用只有 macOS 版，Linux 上暂时没有窗口。适配器通过页面数据接口（`device.ws.set`，以及 `device.ws`、`device.key`、`device.link` 事件）和 `deckd` 通信，这是公开接口，写在 [native companion API 文档](https://openwalkietalkie.org/docs/native-companions/#feed-the-omarchy-face)里。

**在 Linux 上，`deckd` 分不清你的各个进程。** macOS 版服务的两项检查在 Linux 上没有实现：祖先进程上限（让由 agent CLI 启动的进程最多只能申请 `observe,agent,voice,hooks:ingest`），以及受信任的主体（签名的应用）。所以你这个用户的任何进程，包括由 Claude Code、Codex 或 Kimi Code 启动的进程，都可以向 `deckd` 的 socket 申请 `admin`（它的设置、集成、自检和令牌）；也没有任何客户端会被视为受信任，所以客户端为权限卡片发来的允许会被拒绝。固件安装仍然需要在对讲机上按一下，除非对讲机已经处于恢复模式。

**推荐方案：Linux 版 `deckd` 直接装在 Omarchy 上。** 对讲机用蓝牙直连 Omarchy：BlueZ 把它的 HID 服务暴露成 `/dev/hidraw*`，由 `hid.rs` 驱动；语音走对讲机的音频 GATT 服务（BlueZ），用本机 Whisper 识别。Omarchy 上跑两个 systemd 用户服务：`deckd.service`（Linux 版 `deckd`）和 `omarchy-deck.service`（本目录的 Python 适配器）。

```
对讲机 ──蓝牙── Omarchy ─┬─ deckd（Linux 版）
                        ├─ omarchy_deck.py   适配器（Hyprland ⇄ deckd）
                        └─ omarchy_hook.py   Claude Code / Codex / Kimi Code 的 hook
```

对讲机连着 Mac 时也能用：见下面“备选：对讲机连在 Mac 上”，那时 Omarchy 上不跑 `deckd`。

```sh
# 在 Omarchy 上：构建 Linux 版 deckd，再装成 systemd 用户服务
cd host && cargo build --release --target x86_64-unknown-linux-gnu -p deckd
sh omarchy/install.sh target/x86_64-unknown-linux-gnu/release/deckd
python3 omarchy/omarchy_hook.py install kimi     # Kimi Code；claude / codex 同理

# 一次性，用 root：让 deckd（以你的用户运行）能打开对讲机的 /dev/hidraw*
sudo install -m 644 omarchy/70-openwalkietalkie.rules /etc/udev/rules.d/
sudo udevadm control --reload && sudo udevadm trigger --subsystem-match=hidraw
```

构建需要 `pkg-config`、libudev（hidapi）、ALSA（cpal）和 D-Bus（bluer）的开发文件，以及 whisper.cpp 要用的 CMake 和 libclang。CI 在 Ubuntu 上装的是 `pkg-config libudev-dev libasound2-dev libdbus-1-dev cmake libclang-dev`（`.github/workflows/ci.yml`）；在 Omarchy（Arch）上是 `sudo pacman -S --needed cmake clang`，其余已经自带。

**udev 规则。** 没有它，`/dev/hidraw*` 属于 root，`deckd` 日志里是 `cannot open HID device`：`deckd probe` 能看到对讲机，语音也能通过 BLE 连上，但 HID 链路始终起不来。`70-openwalkietalkie.rules` 把对讲机的节点交给坐在这台机器前的用户，USB 和蓝牙都适用：它按 HID id 匹配（`KERNELS=="*:1209:5754.*"`），因为走蓝牙时对讲机是一个没有 USB 父设备的 uhid 设备，按 `ATTRS{idVendor}` 写的规则永远匹配不上。`install.sh` 写不了 `/etc`，规则缺失时会打印上面两条命令；装好之后 `deckd` 会自己打开对讲机，不用重启。

`install.sh` 会把适配器复制到 `~/.local/share/openwalkietalkie/omarchy/`；给了二进制文件时，把 `deckd` 装到 `~/.local/bin/deckd`；再启用并启动 `systemd/` 里的两个单元（可以重复执行；`systemctl --user disable --now deckd omarchy-deck` 撤销）。`deckd.service` 以 `DECKD_DATA_DIR=~/.local/share/openwalkietalkie` 运行 `deckd run`，所以它的配置、socket 和缓存都在这个目录下；`omarchy-deck.service` 在这个 socket 上启动适配器。（不设 `DECKD_DATA_DIR` 启动的 `deckd` 改用 XDG 目录，socket 在 `$XDG_RUNTIME_DIR/openwalkietalkie/deckd.sock`，没有 `XDG_RUNTIME_DIR` 时在 `${XDG_CACHE_HOME:-~/.cache}/openwalkietalkie/deckd.sock`；不带 `--socket` 启动的适配器也去这里找。）`omarchy_hook.py install` 写进各 CLI 设置的 hook 命令指向这份副本，并且脚本不存在时放行（不会因为挪了仓库而拦住你的会话）。`install.sh` 还会关掉 `deckd` 自己对同一批会话的来源（`deckd config set sources.claude=false codex.daemon=false`）：它的 Claude hook 和 `claude agents` 名册种子，以及它的 Codex app-server 连接，会让每个会话换一个键在名册上再出现一次，和适配器注册的并列。适配器自动找到正在运行的 Hyprland 实例和 Wayland 显示，不用写环境变量。

给这个服务用的 `deckd` 命令也要指向同一个目录，否则它会去找 XDG 的 socket，找不到：`DECKD_DATA_DIR=~/.local/share/openwalkietalkie deckd care checks` 检查安装，每条维护命令前面都加同样的前缀（`deckd care sources|pause|resume|test …`、`deckd config get|set`、`deckd device get|set`、`deckd firmware status`；对照表见[主机服务设计](../../docs/host-service.md)第 10.7 节）。Linux 上没有祖先进程上限（见上文），在任何终端里都能用。

### 语音（PTT）

- **识别**：本机 Whisper，没有本地麦克风（对讲机的麦克风是唯一输入）。先下载模型再用。CPU 上 `large-v3-turbo` 每次约 20 秒，建议用 `small`（约 2–4 秒）：

```sh
echo '[voice.stt]
engine = "whisper"
model = "small"
language = "auto"' >> ~/.local/share/openwalkietalkie/config.toml
DECKD_DATA_DIR=~/.local/share/openwalkietalkie deckd stt install --model small
```

`stt install` 走 HuggingFace；连不上时用镜像下载同名文件（`https://hf-mirror.com/ggerganov/whisper.cpp/resolve/main/ggml-small.bin`）放到 `~/.local/share/openwalkietalkie/caches/stt/`。中文会被识别为简体（Linux 上识别出中文时会用简体提示词再解码一次）。

- **播报**：回复由本机一个语音程序朗读（文本走 stdin，WAV 走 stdout）。默认是 `espeak-ng`（`sudo pacman -S espeak-ng`），可以在 `[voice.tts] command` 里换成 piper 等；没装时每次播报会在日志里报错，其他功能不受影响。声音取 `[voice.tts] voices` 的第一项，没配置时按文本含不含中文选 `cmn` / `en-us`。
- **输入到会话**：识别出的文字被输入选中会话的终端并回车。会话在 herdr 窗格里时用 `herdr pane run`（窗格被移动后会按会话 pid 重新查找），否则聚焦它的窗口再用 `wtype`。无法输入的指令（对话框开着、找不到窗口、`wtype` 失败）会连同原因报告给 `deckd`（`agent.order.result`），对讲机会显示指令未送达；不支持这个报告的旧版 `deckd` 不会再被询问。

### Kimi Code

Kimi Code 有 hook（`config.toml` 里的 `[[hooks]]`），`omarchy_hook.py install kimi` 会写入一个带标记的块，先备份，卸载时原样还原；它用 `tomllib` 检查写入结果，所以需要 Python 3.11 或更新版本。没装 hook 时，适配器退回去读 `~/.kimi-code/sessions/*/agents/main/wire.jsonl` 判断状态（尊重 `KIMI_CODE_HOME`）。每次回复结束，回复摘要会推到对讲机上。

## 能做什么

| 功能 | 怎么用 |
|---|---|
| **Omarchy 页** | 显示当前聚焦显示器的工作区（Omarchy 每个显示器各自 1 到 10）和当前工作区最多 6 个窗口，聚焦的窗口高亮 |
| 切换窗口 | 滚轮逆时针 / 顺时针 = 下一个 / 上一个窗口（`cycle_next`）；点窗口行聚焦它 |
| 切换工作区 | 点工作区格子；LEFT / RIGHT = 上一个 / 下一个工作区（按固定的 1–10 槽位轮转，空工作区也会去） |
| 焦点移动 | SHIFT+LEFT / SHIFT+RIGHT = 聚焦左 / 右边窗口（SHIFT 就是右侧最下面那个键 HOME，按住它再按方向键） |
| 全屏 | ENTER = 切换全屏 |
| scratchpad | SHIFT+CANCEL = 打开 / 收起 scratchpad；SHIFT+ENTER = 把当前窗口收进 scratchpad（相当于 Super+S / Super+Alt+S） |
| **名册里的 Omarchy 会话** | Omarchy 上每个 Claude Code / Codex / Kimi Code 会话出现在 Agents 名册，状态是 working / ready / question / error |
| 选中会话 | 在对讲机上选中某个会话，或客户端发 `op.focus`，Omarchy 上它所在的终端窗口会被拉到前台 |
| **语音指令** | 对选中的 Omarchy 会话按 PTT 说话，识别出的文字会被输入到它的终端并回车（用 `wtype`） |
| 回复 | 一轮结束时，它的回复（最多 600 字）显示在会话的 Ready 卡片上并播报；Claude Code 和 Codex 取自 Stop hook，Kimi Code 取自它的日志 |
| **Macropad** | 带按键组合（如 `cmd+shift+4`）的 Macropad 键，在 Omarchy 上用 `wtype` 输入：`cmd` 是 Super，`opt` / `alt` 是 Alt。仅限 Linux 版 `deckd`：连着 Mac 的 `deckd` 会在 Mac 上输入 |
| **System 页** | 由 Linux 版 `deckd` 填写：CPU（`/proc/stat`）、已用内存（`MemTotal - MemAvailable`）、`/` 的剩余空间，以及驱动报告负载的 GPU 中最忙的一块（`gpu_busy_percent`：amdgpu 有；Intel 和 NVIDIA 闭源驱动没有） |

按键映射在 `omarchy_deck.py` 顶部的 `KEYMAP`，想改只改那一处。

## 刻意不做的事

- **不在对讲机上回答权限提示。** 权限对话框留在终端里，会话进入 `question` 状态时，语音指令会被拒绝（`dialog_open`），避免你说的话被当成权限回答。
- **少转发内容。** 只发事件名、状态、项目名，以及 Claude 自己显示的那行通知（用作 question 的标题）。提示词和工具输入不会离开 Omarchy；每轮回复最多只发前 600 字到对讲机。
- **不把 pid 或终端信息发给 deckd。** `deckd` 会把 pid 当成本机进程去检查，所以注册时只发名字、项目目录和能力。
- **hook 一律放行。** 无论守护进程在不在、数据对不对，hook 都输出 `{}` 并以 0 退出，永远不会拦住你的工具调用。
- **对讲机不在时什么都不收集。** `deckd` 的 `device.link` 连续十分钟表示没有对讲机连接之后，桥接程序停止更新 Omarchy 页，也不再查看会话：不读 Kimi Code 日志、不检查进程，也不向 `deckd` 发送任何会话信息。这期间 hook 报告的内容只更新桥接程序自己的会话表。对讲机一连上，它就按每个会话当时的状态重新注册（静默期间打开的对话框重新显示为提问，结束的会话被释放，新开的会话出现，结束了一轮的 Kimi Code 会话会读取它的回复），并发送页面。`deckd` 对自己的采集程序也是这样（`docs/host-service.md` §5.8）。
- **`deckd` 会自己检查新版本。** 启动五分钟后，以及之后在过去十分钟内连接过对讲机的期间每六小时一次，它会从 GitHub 获取发布清单（一次 `curl`，最多 128 KB），看是否有更新的对讲机固件；除了这个请求，不发送任何内容。在 `~/.local/share/openwalkietalkie/config.toml` 中写入 `[updates] check = false` 即可关闭（`docs/host-service.md` §10.3）。

## 备选：对讲机连在 Mac 上

设备同一时刻只存一份蓝牙配对。如果对讲机连着 Mac，就让 Omarchy 上的适配器通过 SSH 转发 Mac `deckd` 的 Unix socket 接进来（这时 Omarchy 上不跑 `deckd`，也没有语音）。文件都在 `host/omarchy/`，只用 Python 3.10+ 标准库，不需要 `pip install`。

### 1. Mac 上准备 SSH 转发用的 socket 路径

socket 路径里有空格，转发起来麻烦，建一个软链接：

```sh
ln -sf "$HOME/Library/Application Support/OpenWalkieTalkie/deckd.sock" "$HOME/.deckd.sock"
```

并在「系统设置 → 通用 → 共享 → 远程登录」里打开远程登录。

### 2. Omarchy 上建立转发

手动试：

```sh
ssh -N -o ServerAliveInterval=15 -o ExitOnForwardFailure=yes \
    -L "$XDG_RUNTIME_DIR/deckd.sock:/Users/<你>/.deckd.sock" <你>@<Mac地址>
```

长期用，放成 systemd 用户服务 `~/.config/systemd/user/deckd-tunnel.service`：

```ini
[Unit]
Description=deckd socket tunnel to the Mac
After=network-online.target

[Service]
ExecStartPre=/usr/bin/rm -f %t/deckd.sock
ExecStart=/usr/bin/ssh -N -o ServerAliveInterval=15 -o ExitOnForwardFailure=yes -o StreamLocalBindUnlink=yes -L %t/deckd.sock:/Users/<你>/.deckd.sock <你>@<Mac地址>
Restart=always
RestartSec=5

[Install]
WantedBy=default.target
```

`systemctl --user enable --now deckd-tunnel`。

### 3. 启动适配器

按 Omarchy 的惯例，在 `~/.config/hypr/autostart.lua` 加一行：

```lua
o.launch_on_start("python3 /path/to/host/omarchy/omarchy_deck.py")
```

也可以在终端手动跑 `python3 omarchy_deck.py` 先看日志。不带 `--socket` 时，它连 `$DECKD_SOCKET`，其次是 `$DECKD_DATA_DIR/deckd.sock`，再次是不设 `DECKD_DATA_DIR` 启动的 Linux `deckd` 监听的位置（见上面 `install.sh` 那段）；那里没有 socket 时，连隧道的这一端 `$XDG_RUNTIME_DIR/deckd.sock`（没有 `XDG_RUNTIME_DIR` 时是 `~/.local/state/deckd.sock`）。每次连接前它都会重新查找，并自动重连：Mac 重启、`deckd` 重启、隧道断开都不用管。

### 4. 安装 hook（让会话出现在名册里）

```sh
python3 omarchy_hook.py status            # 先看现状
python3 omarchy_hook.py install claude    # 写入 ~/.claude/settings.json（先备份）
python3 omarchy_hook.py install codex     # 写入 ~/.codex/hooks.json（先备份）
python3 omarchy_hook.py uninstall all     # 随时撤销，只删我们加的，不碰别人的
```

安装只合并、不覆盖：你已有的 hook 保持原位，重复安装是替换而不是叠加。备份文件在同目录，名字是 `*.omarchy-deck.<时间戳>.bak`。**已经打开的会话不会加载新 hook，要新开会话。**

## 测试

CI 在仓库根目录、Ubuntu 24.04 的 Python 3.12 上运行它们，排在 Linux host 的 Rust 测试之后：

```sh
python3 -m unittest discover -s host/omarchy -p 'test_*.py'
```

覆盖：Hyprland 数据到 `ws` 行的转换、按键映射、进程树和窗口匹配、会话注册和释放、守护进程离线重连、语音指令的各种拒绝条件、hook 放行、安装和卸载、没有对讲机十分钟后静默及对讲机连上后恢复，以及对假 `deckd` 的端到端流程。其中三个（Kimi Code 的安装）需要 `tomllib`，在 3.11 以前的 Python 上会报 `ModuleNotFoundError: No module named 'tomllib'`。**这些都是在伪造的 `/proc` 和 Hyprland 上跑的。** 在真实 Omarchy 上验证过的内容（Hyprland 0.56.2，Rev B 对讲机，固件 0.13.0，蓝牙）见“在 Omarchy 上验证过的”。

---

## 已知限制

- 权限提示不能在对讲机上回答，只能在终端里答。
- 一个工作区最多显示 6 个窗口；工作区格子最多 9 个，Omarchy 的第 10 个工作区在对讲机上不显示。
- 会话只能通过 hook 发现，hook 安装之前就开着的会话不会出现，要重开。
- 之前装好的 Claude / Codex hook 不用重装也会转发回复：回复取自现有 hook 已经收到的 Stop 数据。
- Omarchy 会话在阅读器里没有历史、计划和工具行：只发每轮最后的回复。
- 装 hook 时，Claude / Codex 需要重新运行一次 `install`，才会带上 `PostToolUse`（否则授权后状态会一直停在 question，直到本轮结束）。
- 隧道断开期间，Omarchy 页和名册会停在最后的状态，恢复后自动同步。

## 在 Omarchy 上验证过的

2026-11-14，Hyprland 0.56.2，Rev B 对讲机，固件 0.13.0，蓝牙连接，`install.sh` 和单元文件与这里一致：

- `deckd` 通过蓝牙 HID（装了 udev 规则后）和 BLE 语音连上对讲机，并加载 Whisper 模型。
- `KEYMAP` 的每个按键、LEFT / RIGHT、工作区标签和窗口行，以 `device.key` 和 `device.ws` 事件交给适配器，效果和上表一致。Hyprland 0.56 只接受每个调度的 Lua 写法，旧写法会被拒绝。
- 真实的 Claude Code 会话：hook 注册它，状态依次为 `idle`、`working`、`ready`；`op.order` 用 `wtype` 输入它的终端并得到回答；`op.focus` 把它的窗口拉到前台。

## 还没验证的

见 issue ZhangHanDong/openwalkietalkie#1：对讲机上实际听到的播报、端到端的 PTT（上面的指令是用 `op.order` 发的，不是口述的）、Codex 和 Kimi Code 真实会话、USB、macOS 真机。
