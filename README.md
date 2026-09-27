# tg-junk-msg-cleaner

用你自己的 Telegram 账号（通过 [TDLib](https://github.com/tdlib/td)）按关键词批量删除**单个对话**中的垃圾消息的命令行工具，Rust 编写。

- **登录**：使用你自己的 `api_id` / `api_hash` 和手机号登录（支持验证码、两步验证密码、邮箱验证），会话保存在本地，之后无需重复登录。
- **一次只处理一个对话**：`--chat` 只接受一个值，不支持跨对话批量删除。
- **关键词**：纯文本（默认忽略大小写）或正则表达式；可选"包含任意关键词"或"包含全部关键词"；支持从文件读取。
- **匹配范围**：文字消息、图片/视频/文件等的说明文字、投票、隐藏在文字链接后的 URL 等。
- **Dry-run**：只列出会被删除的消息，不做任何删除。
- **两种用法**：交互式（直接运行，按提示操作）和参数式（`clean` 子命令，适合脚本）。
- **安全**：删除前需确认；根据权限自动选择"为所有人删除"或"仅为自己删除"，无权限的消息会被跳过并说明原因；遇到 Telegram 限流自动等待；Ctrl-C 会在当前步骤完成后安全停止。

## 准备

### 1. 获取 API 凭据

登录 <https://my.telegram.org/apps>，在 "API development tools" 中创建应用，得到 `api_id`（数字）和 `api_hash`（32 位十六进制字符串）。

### 2. 安装 TDLib

本工具在运行时加载 TDLib 的 JSON 接口库（`libtdjson`），编译时不需要 TDLib。需要 **TDLib 1.8.6 或更新版本**（已用 1.8.67 测试）。

- macOS：`brew install tdlib`
- Linux / Windows：按官方的 [构建说明](https://tdlib.github.io/td/build.html) 编译，产物为 `libtdjson.so` / `tdjson.dll`。

查找顺序：`--tdjson <路径>` 或环境变量 `TDJSON_PATH` → 可执行文件所在目录 → 系统库搜索路径（如 `LD_LIBRARY_PATH`、`/usr/local/lib`、`/opt/homebrew/lib`）。

### 3. 编译

```sh
cargo build --release
# 可执行文件：target/release/tg-junk-cleaner
```

## 使用

API 凭据可以通过参数 `--api-id` / `--api-hash`、环境变量 `TG_API_ID` / `TG_API_HASH` 提供；都没有时，交互式运行会提示输入，并可选择保存到数据目录，之后自动使用。

### 交互式

```sh
tg-junk-cleaner            # 等同于 tg-junk-cleaner interactive
tg-junk-cleaner interactive --dry-run   # 只预览，不删除
```

按提示依次：登录 → 从对话列表中选择（可输入文字筛选，或手动输入 ID / @用户名 / 链接）→ 选择匹配方式 → 逐行输入关键词 → 选择扫描方式 → 预览匹配结果 → 选择删除范围 → 确认删除。

### 参数式

```sh
# 先登录一次（会询问手机号、验证码、两步验证密码）
tg-junk-cleaner login

# 查找对话 ID
tg-junk-cleaner chats --search 币圈

# 预览：列出将被删除的消息
tg-junk-cleaner clean -c -1001234567890 -k airdrop -k 加微信 --dry-run

# 删除（会要求确认；加 --yes 跳过确认）
tg-junk-cleaner clean -c -1001234567890 -k airdrop -k 加微信

# 从文件读取关键词、使用正则、跳过确认
tg-junk-cleaner clean -c @my_group -f keywords.txt --regex --yes
```

`clean` 的参数：

| 参数 | 说明 |
| --- | --- |
| `-c, --chat <CHAT>` | 要清理的**唯一**对话：数字 ID、`@用户名`、`t.me` 链接（含 `t.me/c/...` 私有链接）或完整标题 |
| `-k, --keyword <KEYWORD>` | 关键词，可重复多次 |
| `-f, --keywords-file <FILE>` | 关键词文件，每行一个，`#` 开头的行为注释 |
| `--regex` | 关键词按正则表达式处理 |
| `--case-sensitive` | 区分大小写（默认忽略） |
| `--match-all` | 消息必须包含全部关键词（默认包含任意一个即可） |
| `--mode full\|search` | `full`（默认）：逐页读取完整历史并在本地匹配，结果准确；`search`：用 Telegram 服务器搜索各关键词再本地复核，速度快，但可能漏掉词中间的匹配，不支持正则 |
| `--limit <N>` | 最多检查 N 条消息（从最新开始） |
| `-n, --dry-run` | 只列出匹配的消息，不删除 |
| `-y, --yes` | 不询问直接删除（非终端环境下删除必须加此参数） |
| `--for-me-only` | 仅为自己删除（只适用于私聊和普通群） |
| `-q, --quiet` | 只输出汇总，不逐条列出 |

其它命令：

- `login`：登录并保存会话。
- `chats [--search 文本] [--limit N]`：列出对话及其 ID。
- `logout`：退出登录，删除本地会话和已保存的 API 凭据。

通用参数：`--data-dir`（数据目录，环境变量 `TG_CLEANER_DATA_DIR`）、`--tdjson`、`--phone`（环境变量 `TG_PHONE`）、`--tdlib-log-level`（TDLib 日志写入数据目录下的 `tdlib.log`）。

默认数据目录：Linux `~/.local/share/tg-junk-msg-cleaner`，macOS `~/Library/Application Support/tg-junk-msg-cleaner`，Windows `%LOCALAPPDATA%\tg-junk-msg-cleaner`。用不同的 `--data-dir` 可以使用多个账号。

## 删除规则

Telegram 的权限决定了哪些消息能删、怎么删，本工具会先向 TDLib 查询每条匹配消息的权限：

- **超级群 / 频道**：只能删除自己的消息，或者在你是有删除权限的管理员时删除他人消息；删除总是对所有人生效，因此不支持 `--for-me-only`。
- **私聊 / 普通群**：默认在允许时为所有人删除，否则仅为自己删除；使用 `--for-me-only` 时只做"仅为自己删除"，不会影响其他人。
- 没有权限的消息会被跳过，并在预览中标注原因。

删除不可恢复，建议先用 `--dry-run` 确认结果。

## 隐私与安全

- 所有数据（TDLib 会话、消息缓存、可选保存的 API 凭据）只保存在本地数据目录中，目录权限为仅当前用户可访问。
- 会话文件等同于账号登录状态，请勿泄露；不再使用时运行 `tg-junk-cleaner logout`。
- 请合理使用，频繁大量操作可能触发 Telegram 的限流。

## 开发

```sh
cargo test                     # 单元测试 + 基于内存假 TDLib 的端到端测试，不需要 TDLib 或网络
TDJSON_PATH=/path/to/libtdjson.so cargo test -- --ignored   # 用真实 TDLib 校验所有请求格式（不需要账号和网络）
```

代码结构：

| 文件 | 作用 |
| --- | --- |
| `src/tdjson.rs` | 运行时加载 `libtdjson` 的 FFI 层 |
| `src/client.rs` | 同步 TDLib 客户端：后台线程接收响应并按 `@extra` 分发 |
| `src/auth.rs` | 登录状态机 |
| `src/telegram.rs` | 用到的 TDLib 方法的封装、对话解析 |
| `src/message.rs` | 从消息中提取可匹配的文本 |
| `src/matcher.rs` | 关键词匹配 |
| `src/cleaner.rs` | 扫描、删除计划与执行 |
| `src/commands.rs` / `src/interactive.rs` | 参数式与交互式两种入口 |
