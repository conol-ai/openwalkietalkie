# Omarchy 适配器

让对讲机遥控 Omarchy（Hyprland）笔记本：窗口和工作区、Claude Code / Codex 会话、语音指令。

对讲机继续只连这台 Mac（设备同一时刻只存一份蓝牙配对，Omarchy 笔记本没有 Type-C）。Omarchy 上跑一个小守护进程，通过 SSH 转发 Mac `deckd` 的 Unix socket 接进来。不需要 Linux 版 `deckd`。

```
对讲机 ──蓝牙/USB── Mac (deckd) ══SSH 转发 socket══ Omarchy 笔记本
                                                   ├─ omarchy_deck.py   守护进程（Hyprland ⇄ deckd）
                                                   └─ omarchy_hook.py   Claude Code / Codex 的 hook
```

> **当前推荐方案：Linux 版 `deckd` 直接装在 Omarchy 上。** 对讲机用蓝牙直连 Omarchy（BlueZ 的 HID 服务会把它暴露成 `/dev/hidraw*`，现有的 `hid.rs` 直接驱动），不再需要 Mac 和 SSH 转发。下面“部署”里的 SSH 转发方案只在对讲机连着 Mac 时用。
>
> ```sh
> # 在 Omarchy 上：构建 Linux 版 deckd，再装成 systemd 用户服务
> cd host && cargo build --release --target x86_64-unknown-linux-gnu -p deckd
> sh omarchy/install.sh target/x86_64-unknown-linux-gnu/release/deckd
> python3 omarchy/omarchy_hook.py install kimi     # Kimi Code；claude / codex 同理
> ```
>
> 服务是 `deckd.service` 和 `omarchy-deck.service`（`systemctl --user status ...`）。适配器会自动找到正在运行的 Hyprland 实例和 Wayland 显示，不用写环境变量。**语音（PTT）** 已支持：`deckd` 的 Linux 版通过 BlueZ 连对讲机的音频 GATT 服务，用本机 Whisper 识别，没有本地麦克风、也没有语音合成（回复不会被朗读）。先下载模型再用：

> ```sh
> echo '[voice.stt]
> engine = "whisper"
> model = "large-v3-turbo-q5_0"
> language = "auto"' >> ~/.local/share/openwalkietalkie/config.toml
> DECKD_DATA_DIR=~/.local/share/openwalkietalkie deckd stt install --model large-v3-turbo-q5_0
> ```
>
> `stt install` 走 HuggingFace；连不上时用镜像下载同名文件（`https://hf-mirror.com/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo-q5_0.bin`）放到 `~/.local/share/openwalkietalkie/caches/stt/`。
> Kimi Code 有 hook（`config.toml` 里的 `[[hooks]]`），`omarchy_hook.py install kimi` 会写入一个带标记的块，先备份，卸载时原样还原。没装 hook 时，适配器会退回去读 `~/.kimi-code/sessions/*/agents/main/wire.jsonl` 判断状态。

## 能做什么

| 功能 | 怎么用 |
|---|---|
| **Omarchy 页** | 显示当前聚焦显示器的工作区（Omarchy 每个显示器各自 1 到 10）和当前工作区最多 6 个窗口，聚焦的窗口高亮 |
| 切换窗口 | 滚轮逆时针 / 顺时针 = 下一个 / 上一个窗口（`cycle_next`）；点窗口行聚焦它 |
| 切换工作区 | 点工作区格子；SHIFT+LEFT / SHIFT+RIGHT = 上一个 / 下一个工作区 |
| 焦点移动 | LEFT / RIGHT = 聚焦左 / 右边窗口 |
| 全屏 | ENTER = 切换全屏 |
| **名册里的 Omarchy 会话** | Omarchy 上每个 Claude Code / Codex 会话出现在 Agents 名册，状态是 working / ready / question / error |
| 选中会话 | 在对讲机上选中某个会话，Omarchy 上它所在的终端窗口会被拉到前台 |
| **语音指令** | 对选中的 Omarchy 会话按 PTT 说话，识别出的文字会被输入到它的终端并回车（用 `wtype`） |

按键映射在 `omarchy_deck.py` 顶部的 `KEYMAP`，想改只改那一处。

## 刻意不做的事

- **不在对讲机上回答权限提示。** 权限对话框留在终端里，会话进入 `question` 状态时，语音指令会被拒绝（`dialog_open`），避免你说的话被当成权限回答。
- **不转发内容。** 只发事件名、状态、项目名，以及 Claude 自己显示的那行通知（用作 question 的标题）。提示词、工具输入、回复都不会离开 Omarchy。
- **不把 Linux 的 pid 或终端信息发给 Mac。** Mac 上的 `deckd` 会把 pid 当成本机进程去检查，所以注册时只发名字、项目目录和能力。
- **hook 一律放行。** 无论守护进程在不在、数据对不对，hook 都输出 `{}` 并以 0 退出，永远不会拦住你的工具调用。

## 部署

文件都在 `host/omarchy/`，只用 Python 3.10+ 标准库，不需要 `pip install`。

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
for t in test_omarchy_deck test_omarchy_sessions test_omarchy_hook test_omarchy_integration; do python3 -B $t.py; done
```

覆盖：Hyprland 数据到 `ws` 行的转换、按键映射、进程树和窗口匹配、会话注册和释放、守护进程离线重连、语音指令的各种拒绝条件、hook 放行、安装和卸载，以及对假 `deckd` 的端到端流程。**这些都是在 macOS 上用伪造的 `/proc` 和 Hyprland 跑的，没有在真实 Omarchy 上跑过。**

---

# 需要你配合的 Checklist

**这些都不阻塞代码**，代码和测试已经完成。以下是只有你能在真机上做的验证，按顺序做，遇到问题把输出发给我。

## A. 必做，一次性

- [ ] 1. 把 `host/omarchy/` 放到 Omarchy 笔记本（`git clone` 或 `scp -r`）。
- [ ] 2. 确认 Omarchy 上有 `wtype`（`command -v wtype`，Omarchy 基础包自带）和 `python3 --version` ≥ 3.10。
- [ ] 3. Mac：打开「远程登录」，建软链接（见上面第 1 步）。
- [ ] 4. Omarchy：手动跑一次 SSH 转发，确认 `ls -l $XDG_RUNTIME_DIR/deckd.sock` 存在。
- [ ] 5. Omarchy：`python3 omarchy_deck.py`，看日志有没有报错。它用 `device` 和 `agent` 权限通过 socket 连 `deckd`。我从源码核对过：Mac 上 `[api] uds_max_scope` 默认是 `admin`，转发进来的连接的对端是 sshd 的子进程，祖先链里没有 `claude`、`codex`、`gemini`、`node`，不会被"agent 祖先"上限卡住，所以理论上能通过。**但没有真机验证过。** 如果日志报 `forbidden`：先看 Mac 的 `config.toml` 里有没有改过 `[api] uds_max_scope`；再不行，把报错发给我，我改成走 HTTP 加 token。
- [ ] 6. 把 `deckd-tunnel` 做成 systemd 用户服务，适配器加进 `autostart.lua`（见上面部署第 2、3 步）。

## B. Omarchy 页（窗口切换）验证

- [ ] 7. 对讲机切到 Omarchy 页：应该能看到工作区格子和当前窗口列表，聚焦的窗口高亮。
- [ ] 8. 滚轮转一格：焦点应该切到下一个窗口。如果没动，在 Omarchy 上手动试：
  `hyprctl dispatch 'hl.dsp.window.cycle_next()'`，再试 `hyprctl dispatch cyclenext`，告诉我哪个有效。
- [ ] 9. 点一个工作区格子、点一个窗口行、按 LEFT / RIGHT / SHIFT+LEFT / ENTER，看是否符合上面表格。**ENTER = 全屏**是我选的，不合适就告诉我换成什么。
- [ ] 10. 如果你用多个显示器，看看格子数和窗口列表是不是对应当前聚焦的那个显示器。

## C. 会话与语音验证

- [ ] 11. `python3 omarchy_hook.py install claude`，然后在 Omarchy 上**新开**一个 Claude Code 会话，发一句话：对讲机的 Agents 名册里应该出现一个以项目目录命名的行，状态跟着变化。
- [ ] 12. 在对讲机上选中这一行：Omarchy 上对应的终端窗口应该被拉到前台。
  **注意：** 如果你的终端是单实例 kitty 之类（多个窗口共用一个进程），窗口匹配靠标题里的项目名，匹配不到时选最近聚焦的，可能选错。出现了告诉我你用的终端。
- [ ] 13. 对这个会话按 PTT 说一句话：文字应该出现在终端并回车。先在一个**空闲、没有对话框**的会话上试。
- [ ] 14. 让 Claude 弹一个权限对话框，这时在对讲机上说话：应该被拒绝（日志 `dialog_open`），文字不会被输入。
- [ ] 15. 可选：`python3 omarchy_hook.py install codex`，然后在 Codex 里用它自己的 hook 信任界面（`/hooks`）信任这些 hook。**Codex 的 hook 载荷格式我是按 deckd 里已有的实现推断的，没有实机验证。**

## D. 需要你拍板的

- [ ] 16. 语音识别在 Mac 上做，目前是 Apple 本机识别，`locale = "zh-TW"`（中文，繁体输出），英文会变差。要不要下载系统的简体中文听写模型，或者换 Whisper 的量化小模型？
- [ ] 17. Codex 会话要不要和 Claude 一样支持语音输入（现在代码支持，但没验证过）？
- [ ] 18. 要不要提交这批文件，提交到哪个分支，要不要提 PR？（我没有提交。）

## E. 已知限制（不用处理，知道就行）

- 权限提示不能在对讲机上回答，只能在终端里答。
- 一个工作区最多显示 6 个窗口；工作区格子最多 9 个，Omarchy 的第 10 个工作区在对讲机上不显示。
- 会话只能通过 hook 发现，hook 安装之前就开着的会话不会出现，要重开。
- 不转发 Claude 的回复文字，所以对讲机上看不到 Omarchy 会话的回复内容，只有状态。
- 隧道断开期间，Omarchy 页和名册会停在最后的状态，恢复后自动同步。
