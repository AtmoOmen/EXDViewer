# EXDViewer
<img align="right" src="https://github.com/AtmoOmen/EXDViewer/blob/main/viewer/assets/icon.png?raw=true" width="20%">

[![Native Build](https://img.shields.io/github/actions/workflow/status/AtmoOmen/EXDViewer/build-native.yml?style=for-the-badge&label=Native%20Build
)](https://github.com/AtmoOmen/EXDViewer/releases)
[![Web Build](https://img.shields.io/github/actions/workflow/status/AtmoOmen/EXDViewer/build-web.yml?style=for-the-badge&label=Web%20Build
)](https://github.com/AtmoOmen/EXDViewer/pkgs/container/exdviewer-web)
[![License](https://img.shields.io/github/license/AtmoOmen/EXDViewer?style=for-the-badge&)](/LICENSE)
[![FFXIV Version](https://img.shields.io/badge/dynamic/json?url=https%3A%2F%2Fexd.camora.dev%2Fapi%2F4e9a232b%2Fversions&query=latest&style=for-the-badge&label=Latest%20XIV%20Version
)](https://thaliak.xiv.dev/repository/4e9a232b)

EXDViewer 是一个现代化、快速且易用的工具，用于浏览《最终幻想 XIV》的 [Excel 文件](https://xiv.dev/game-data/file-formats/excel)。Excel 文件是结构化数据表格，存储各种游戏内信息，例如物品属性、NPC 数据等。

## 功能特性

- **Web 与原生双支持**：即刻使用 [exd.camora.dev](https://exd.camora.dev) 在线版，或下载[原生客户端](https://github.com/AtmoOmen/EXDViewer/releases)
- **轻松部署**：通过 Docker 自行托管 Web 实例
- **高性能**：高效处理所有数据表，即使是 `Item`、`Action`、`Quest` 等巨型表也毫无压力
- **EXDSchema 支持**：与 [EXDSchema](https://github.com/xivdev/EXDSchema) 深度集成，支持增强数据探索和动态在线模式编辑
- **高级过滤**：支持简单、模糊、复杂等多种过滤方式，快速定位目标数据

## 快速开始

### 在线使用

访问 [exd.camora.dev](https://exd.camora.dev) 在浏览器中使用最新版本。支持加载本地游戏安装和表定义文件（仅限 [Chromium 系浏览器](https://developer.mozilla.org/en-US/docs/Web/API/Window/showDirectoryPicker#browser_compatibility)），并支持全球、韩服、国服和台服四个区域（台服尚未发布到 [Thaliak](https://thaliak.xiv.dev/)，暂不可用）。

### 本地运行

在 [Releases 页面](https://github.com/AtmoOmen/EXDViewer/releases) 下载对应平台的预编译二进制文件。

### 通过 Docker 自托管

使用 Docker 自行部署网站：

```bash
docker pull ghcr.io/atmoomen/exdviewer-web:main
docker run -p 8080:80 ghcr.io/atmoomen/exdviewer-web:main
```

然后在浏览器中打开 [http://localhost:8080](http://localhost:8080)。稍等几秒加载最新游戏版本后，在设置中将 API 地址设为 `http://localhost:8080/api`。

## 什么是 EXD 文件

在 SqPack 中，0A 分类下的文件（即 0a0000.win32... 系列）将 Excel 数据表序列化为私有的二进制格式，供游戏读取。Excel 文件（其中 .exd 文件包含实际数据）是《最终幻想 XIV》数据存储的核心部分，包含任务、物品等表格信息，常被社区用于数据挖掘和工具开发。程序化访问这些文件通常通过 [Lumina](https://github.com/NotAdam/Lumina)（C#）、[ironworks](https://github.com/ackwell/ironworks)（Rust）或 [XIVAPI](https://xivapi.com/)（REST API）实现。

更多信息见[此处](https://xiv.dev/game-data/file-formats/excel)。

## 什么是 EXDSchema

《最终幻想 XIV》的内部开发流程会为每个数据表生成头文件，随后编译进游戏。因此游戏发布后，客户端侧的所有结构信息都会丢失。EXDSchema 项目致力于统一社区力量，创建一套语言无关的模式定义，方便任何语言解析消费，准确描述提供给客户端的 EXH 文件结构。

更多信息见[此处](https://github.com/xivdev/EXDSchema?tab=readme-ov-file#exdschema)。

## MCP（Model Context Protocol）支持

EXDViewer 内置了 MCP 服务器，允许 AI 工具（如 Claude Code、Cursor 等）直接查询 FFXIV 游戏数据。启动桌面版后，MCP 服务器会在 `http://127.0.0.1:3001/mcp` 自动运行。

### 可用工具

| 工具 | 功能 |
|---|---|
| `health_check` | 检查 MCP 服务状态和默认语言 |
| `list_asset_paths` | 搜索和分页浏览资源路径，可附带未命名哈希资源 |
| `resolve_asset_path` | 计算资源路径的 split / whole 索引哈希并检查存在性 |
| `check_asset_paths` | 批量检查资源路径是否存在 |
| `read_asset` | 按路径分页读取资源原始字节和格式识别结果 |
| `read_asset_by_hash` | 按仓库、分类和索引哈希分页读取未命名资源 |
| `inspect_asset` | 按路径结构化解析纹理、PNG、材质、字体、图标、ULD、SHPK、SHCD、SCD、LGB 和 SGB |
| `inspect_asset_by_hash` | 结构化解析未命名哈希资源 |
| `decode_texture` | 将 TEX 纹理解码为尺寸受限的 PNG 图像内容 |
| `list_sheets` | 列出数据表，支持模糊搜索、分页、杂项表开关 |
| `get_sheet_info` | 获取表元数据（列数、子行、语言） |
| `get_sheet_schema` | 获取完整结构化模式定义，包括链接目标、条件和嵌套字段 |
| `get_schema_raw` | 获取原始模式 YAML |
| `get_game_version` | 获取数据与模式来源版本信息 |
| `validate_filter` | 检查过滤 DSL 语法 |
| `validate_schema` | 验证模式 YAML |
| `get_icon_paths` | 图标 ID 转普通和高分辨率纹理路径 |
| `decompose_model_id` | 拆解装备模型 ID 或武器模型 ID |
| `search_cells` | 在限定列和行范围内搜索字符串单元格（纯文本，不支持 DSL） |
| `query_rows` | 行级分页查询，支持复杂过滤 DSL、列选择和按请求语言读取 |
| `get_row` | 按 ID 精确获取单行数据，支持列选择和详细原始数据模式 |
| `get_sheet_relations` | 获取表的完整关系映射 |
| `get_referencing_sheets` | 查询引用目标表的字段、链接和条件链接 |
| `resolve_link` | 按 schema 解析链接列并返回目标行，支持条件链接和目标列选择 |
| `decode_se_string` | 解码 SeString 单元格 |
| `save_schema` | 保存模式 YAML |

资源原始字节工具默认返回 4096 字节，单次最多返回 65536 字节；响应中的 `next_offset` 可直接用于读取下一段。结构化解析工具通过 `max_items` 控制字形、图标、着色器和布局集合的返回规模，默认 100，最多 500

`query_rows` 与 `get_row` 默认使用 `compact` 格式，将列定义放在响应顶层，行只返回值数组。对宽表应传入 `columns`，元素可为从 0 开始的列索引或 schema 列名。只有需要 SeString 原始字节、类型细节等信息时才传入 `format: "detailed"`

带普通筛选的 `query_rows` 默认在取得当前页和下一页存在性后停止扫描，此时 `matched_rows` 为 `null`。传入 `count_total: true` 可获得精确匹配总数。链接列默认按行 ID 筛选，传入 `resolve_links: true` 才会等待并使用目标行显示字段

目前 MCP 服务器仅限桌面版，WASM 平台暂不支持。

## 从源码构建

1. 克隆仓库：
    ```bash
    git clone https://github.com/AtmoOmen/EXDViewer.git
    cd EXDViewer
    ```

### 原生客户端

2. 构建项目：
    ```bash
    cargo build --bin viewer --release
    ```

### Web 版

2. 安装 trunk：
    ```bash
    cargo install --locked trunk
    ```
    或参照[安装指南](https://trunkrs.dev/guide/getting-started/installation.html)。确保 `trunk` 已安装并在 PATH 中。

3. 若不需要 API 服务器，可仅构建 viewer 以节省时间：
    ```bash
    trunk serve --release --config viewer
    ```

4. 若需要 API 服务器，构建 web 二进制（内部也会构建 viewer）：
    ```bash
    cargo run --bin web --release
    ```

## 测试

`cargo test` 覆盖解析器和 UI 逻辑。3D 查看器只能在浏览器中实际测试，这正是 `smoke/run.sh` 所做的工作：构建 wasm 应用，用无头 Chromium 驱动模型和场景查看器，遇到任何 GL 错误、panic 或 `ERROR` 级日志都会失败。参见 [smoke/README.md](smoke/README.md)。

## 参与贡献

欢迎提交贡献、Bug 报告和功能请求。请通过 [issue](https://github.com/AtmoOmen/EXDViewer/issues) 或 [pull request](https://github.com/AtmoOmen/EXDViewer/pulls) 参与。