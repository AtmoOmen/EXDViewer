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
| `check_asset_paths` | 批量检查资源路径是否存在 |
| `read_asset` | 按路径分页读取资源原始字节和格式识别结果 |
| `read_asset_by_hash` | 按仓库、分类和索引哈希分页读取未命名资源 |
| `inspect_asset` | 按路径结构化解析资源并返回完整数据；路径给文件夹时逐一遍历，配合 `output` 把结果写进指定文件夹 |
| `inspect_asset_by_hash` | 结构化解析未命名哈希资源 |
| `render_asset` | 把纹理、图像、模型、界面布局、字体、图标字体渲染成可直接查看的 PNG 图像内容 |
| `export_asset` | 批量导出资源：TEX 同时导出 PNG，SCD 同时导出 WAV，其余原样落盘 |
| `find_uld_using_texture` | 按材质路径反查引用了它的界面布局（ULD）路径 |
| `list_sheets` | 列出数据表，支持模糊搜索、分页、杂项表开关 |
| `get_sheet_schema` | 获取表的模式定义（列名、类型、描述、关系映射），可附带原始 YAML 与表元信息 |
| `get_game_version` | 获取数据与模式来源版本信息 |
| `validate_filter` | 检查过滤 DSL 语法 |
| `validate_schema` | 验证模式 YAML |
| `query_rows` | 行级分页查询，支持复杂过滤 DSL、扫描窗口、列选择、命中列报告与按请求语言读取 |
| `export_sheet` | 把数据表导出为 CSV，可只导出筛选命中的行与选中的列 |
| `get_row` | 按 ID 精确获取单行数据，支持列选择和详细原始数据模式 |
| `get_referencing_sheets` | 查询引用目标表的字段、链接和条件链接 |
| `resolve_link` | 按 schema 解析链接列并返回目标行，支持条件链接和目标列选择 |
| `list_icon_sets` | 列出图标集合：每个引用图标的数据表算一个集合，另有其他图标、语言图标与全部图标 |
| `get_icon_set` | 取一个集合内的图标 ID，可指定单页数量与返回页范围 |
| `get_icon` | 按 ID 取回图标的图像、纹理路径，以及哪些表的哪些行在引用它 |

资源原始字节工具默认返回 4096 字节，单次最多返回 65536 字节；响应中的 `next_offset` 可直接用于读取下一段。结构化解析工具通过 `max_items` 控制集合返回规模，默认 100，最多 500，并在 `truncated` 中标记被截断的集合

各格式的 `details` 均为完整解析数据：ULD 带组件与控件的节点树、动画关键帧组与标签集；TMB 带逐条 item 与全部命令字段；CUTB 带节点明细与内嵌时间轴；LGB/SGB 带图层实例的完整类型数据；字体带字距表；SHCD 带资源明细

`query_rows` 与 `get_row` 默认使用 `compact` 格式，将列定义放在响应顶层，行只返回值数组。对宽表应传入 `columns`，元素可为从 0 开始的列索引或 schema 列名。只有需要 SeString 原始字节、类型细节等信息时才传入 `format: "detailed"`

带普通筛选的 `query_rows` 默认在取得当前页和下一页存在性后停止扫描，此时 `matched_rows` 为 `null`。传入 `count_total: true` 可获得精确匹配总数。链接列默认按行 ID 筛选，传入 `resolve_links: true` 才会等待并使用目标行显示字段

搜索某段文字用 `query_rows` 的 `* *= "关键词"`，只搜指定列写成 `Name *= "关键词"`。`row_offset` 与 `max_rows` 把扫描限定在一段行内，传入 `matched_columns: true` 会在每行附上命中筛选条件的列（列索引、列名与存储偏移）

`export_sheet` 省略 `filter` 导出全表，给了就只导出命中的行，`columns` 选取要写出的列。`output` 以 `.csv` 结尾当作文件名，否则当作目录并以表名命名。CSV 逐行落盘，因此全表导出不会把整张表先攒在内存里

`export_asset` 用 `paths` 精确点名，或用 `query` 按路径筛选语法批量匹配，两者可以一起给。`match_mode` 选 `fuzzy`、`strict` 或 `regex`。输出目录下沿用游戏路径本身的层级；`.tex` 按 `max_dim` 转成 PNG，`.scd` 用 `stream` 指定导出第几条音频流，省略时导出前 64 条可解码的流。单个资源失败只记进 `failed`，不影响其余

`render_asset` 覆盖纹理、图像、模型、界面布局、字体与图标字体。`max_dim` 限制输出最长边，也是画布上限：纹理、布局与字形数量都不受这次渲染控制，所以尺寸会在分配之前收进这个框里。渲染字体时 `text` 指定要排的字，省略就把字形铺成网格；模型可用 `camera` 给方位角、俯仰角、距离与缩放，字体与图标也会在显式给出角度时按同一视角摆成有透视的一块平面。模型超过 15 万面时自动取更粗的细节等级

图标工具分三步：`list_icon_sets` 列出集合与各自的图标数，`get_icon_set` 按 `page_size` 与 `start_page`／`end_page` 取集合内的 ID，`get_icon` 给出单个图标的图像、普通与高分辨率纹理路径、语言与 `_hr1` 标记，以及引用它的行（最多 200 处）

图标集合来自把所有表的行过一遍的反查结果，首次调用要读完全部表定义与行，之后的调用走缓存。`inspect_asset` 的文件夹模式一次最多处理 200 个资源，`truncated` 会标出是否还有未处理的

`find_uld_using_texture` 接收材质路径，遍历本安装全部界面布局并返回引用了该材质的布局路径。`query` 与资源浏览器同语法，支持 `ext:` 后缀过滤和含 `/` 的字面匹配，`match_mode` 选 `fuzzy`、`strict` 或 `regex`，`limit` 省略时返回全部匹配。首次调用会读取并解析全部布局，之后的查询在内存里完成

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