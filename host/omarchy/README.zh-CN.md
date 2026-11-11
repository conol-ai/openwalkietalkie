# Omarchy 适配器

让对讲机遥控 Omarchy（Hyprland）笔记本：窗口和工作区、scratchpad、Claude Code / Codex / Kimi Code 会话、语音指令。

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
```

`install.sh` 会把适配器复制到 `~/.local/share/openwalkietalkie/omarchy/`，`install` 写进各 CLI 设置的 hook 命令指向这份副本，并且脚本不存在时放行（不会因为挪了仓库而拦住你的会话）。适配器自动找到正在运行的 Hyprland 实例和 Wayland 显示，不用写环境变量。

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
- **输入到会话**：识别出的文字被输入选中会话的终端并回车。会话在 herdr 窗格里时用 `herdr pane run`（窗格被移动后会按会话 pid 重新查找），否则聚焦它的窗口再用 `wtype`。

### Kimi Code

Kimi Code 有 hook（`config.toml` 里的 `[[hooks]]`），`omarchy_hook.py install kimi` 会写入一个带标记的块，先备份，卸载时原样还原。没装 hook 时，适配器退回去读 `~/.kimi-code/sessions/*/agents/main/wire.jsonl` 判断状态（尊重 `KIMI_CODE_HOME`）。每次回复结束，回复摘要会推到对讲机上。

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
| 选中会话 | 在对讲机上选中某个会话，Omarchy 上它所在的终端窗口会被拉到前台 |
| **语音指令** | 对选中的 Omarchy 会话按 PTT 说话，识别出的文字会被输入到它的终端并回车（用 `wtype`） |

按键映射在 `omarchy_deck.py` 顶部的 `KEYMAP`，想改只改那一处。

## 刻意不做的事

- **不在对讲机上回答权限提示。** 权限对话框留在终端里，会话进入 `question` 状态时，语音指令会被拒绝（`dialog_open`），避免你说的话被当成权限回答。
- **少转发内容。** 只发事件名、状态、项目名，以及 Claude 自己显示的那行通知（用作 question 的标题）。提示词和工具输入不会离开 Omarchy；Kimi Code 的回复只发一段摘要（最多 600 字）到对讲机。
- **不把 pid 或终端信息发给 deckd。** `deckd` 会把 pid 当成本机进程去检查，所以注册时只发名字、项目目录和能力。
- **hook 一律放行。** 无论守护进程在不在、数据对不对，hook 都输出 `{}` 并以 0 退出，永远不会拦住你的工具调用。

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

也可以在终端手动跑 `python3 omarchy_deck.py` 先看日志。它会自动重连，Mac 重启、`deckd` 重启、隧道断开都不用管。

### 4. 安装 hook（让会话出现在名册里）

```sh
python3 omarchy_hook.py status            # 先看现状
python3 omarchy_hook.py install claude    # 写入 ~/.claude/settings.json（先备份）
python3 omarchy_hook.py install codex     # 写入 ~/.codex/hooks.json（先备份）
python3 omarchy_hook.py uninstall all     # 随时撤销，只删我们加的，不碰别人的
```

安装只合并、不覆盖：你已有的 hook 保持原位，重复安装是替换而不是叠加。备份文件在同目录，名字是 `*.omarchy-deck.<时间戳>.bak`。**已经打开的会话不会加载新 hook，要新开会话。**

## 测试

```sh
cd host/omarchy
for t in test_omarchy_deck test_omarchy_sessions test_omarchy_hook test_omarchy_kimi test_omarchy_integration; do python3 -B $t.py; done
```

覆盖：Hyprland 数据到 `ws` 行的转换、按键映射、进程树和窗口匹配、会话注册和释放、守护进程离线重连、语音指令的各种拒绝条件、hook 放行、安装和卸载，以及对假 `deckd` 的端到端流程。**这些都是在 macOS 上用伪造的 `/proc` 和 Hyprland 跑的，没有在真实 Omarchy 上跑过。**

---

## 已知限制

- 权限提示不能在对讲机上回答，只能在终端里答。
- 一个工作区最多显示 6 个窗口；工作区格子最多 9 个，Omarchy 的第 10 个工作区在对讲机上不显示。
- 会话只能通过 hook 发现，hook 安装之前就开着的会话不会出现，要重开。
- 只有 Kimi Code 的回复摘要会推到对讲机；Claude / Codex 的回复文字不转发，只有状态。
- 装 hook 时，Claude / Codex 需要重新运行一次 `install`，才会带上 `PostToolUse`（否则授权后状态会一直停在 question，直到本轮结束）。
- 隧道断开期间，Omarchy 页和名册会停在最后的状态，恢复后自动同步。

## 还没验证的

见 issue ZhangHanDong/openwalkietalkie#1：对讲机上的实际播报、Claude / Codex 真实会话、USB、macOS 真机。
