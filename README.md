<div align="center">

# GOODScanner

**中文 | [English](README_EN.md)**

基于 [yas](https://github.com/1803233552/yas) 编写的原神 GOOD 格式扫描器

扫描游戏内角色、武器、圣遗物数据，导出为 [GOOD v3](https://frzyc.github.io/genshin-optimizer/#/doc) 格式 JSON，可直接导入 [GGArtifact](https://ggartifact.com/)、[Genshin Optimizer](https://frzyc.github.io/genshin-optimizer/) 等配装工具。

[![Build](https://github.com/Anyrainel/GOODScanner/actions/workflows/rust.yml/badge.svg)](https://github.com/Anyrainel/GOODScanner/actions)

</div>

## 功能

- **角色扫描**：名称、等级、突破、命座、天赋
- **武器扫描**：名称、等级、突破、精炼、装备角色、锁定状态
- **圣遗物扫描**：套装、位置、主词条、副词条、等级、稀有度、锁定、星标、祝圣秘境标记、待激活词条
- **双引擎 OCR**：PPOCRv4（通用）+ PPOCRv5（特殊部分专用），自动选择最优结果
- **副词条验证**：Roll Solver 基于游戏精确度验证词条合法性

## 快速开始

### 下载

从 [Releases](https://github.com/Anyrainel/GOODScanner/releases) 页面下载最新的 `GOODScanner.exe`。

### 使用步骤

1. 以**管理员身份**运行 `GOODScanner.exe`
2. 首次运行会提示输入自定义角色名（旅行者/流浪者等），配置保存在 `data/good_config.json`
3. 确保原神已运行，按回车开始扫描（程序会自动切换到游戏窗口并打开对应界面）
4. 扫描过程中可按**鼠标右键**终止
5. 结果输出为当前目录下的 `GOODv3.json`

### 扫描目标

默认扫描全部（角色 + 武器 + 圣遗物）。也可以指定：

```shell
GOODScanner.exe                    # 扫描全部
GOODScanner.exe --characters       # 仅扫描角色
GOODScanner.exe --weapons          # 仅扫描武器
GOODScanner.exe --artifacts        # 仅扫描圣遗物
GOODScanner.exe --characters --weapons  # 组合扫描
```

## 注意事项

- 需要**管理员权限**（用于模拟键鼠输入）
- 仅支持**简体中文**游戏客户端
- 推荐 **16:9** 分辨率（1920×1080、2560×1440 等）
- 扫描过程中请勿操作鼠标
- 默认 4 星以下圣遗物不扫描（可通过 `--artifact-min-rarity` 调整）

## 命令行参数

### 通用选项

| 参数 | 说明 |
|------|------|
| `-v, --verbose` | 显示详细扫描信息 |
| `--continue-on-failure` | 单项失败时继续扫描 |
| `--log-progress` | 逐项显示扫描进度 |
| `--output-dir <DIR>` | 输出目录（默认当前目录） |
| `--ocr-backend <NAME>` | 覆盖 OCR 后端（ppocrv4 或 ppocrv5） |
| `--dump-images` | 保存 OCR 区域截图到 `debug_images/` |

### 扫描器配置

| 参数 | 说明 |
|------|------|
| `--weapon-min-rarity <N>` | 最低武器稀有度（默认 3） |
| `--artifact-min-rarity <N>` | 最低圣遗物稀有度（默认 4） |
| `--char-max-count <N>` | 最大角色数（0 = 不限） |
| `--weapon-max-count <N>` | 最大武器数（0 = 不限） |
| `--artifact-max-count <N>` | 最大圣遗物数（0 = 不限） |
| `--weapon-skip-delay` | 跳过武器面板等待（更快但锁定检测可能不准） |
| `--artifact-skip-delay` | 跳过圣遗物面板等待（更快但锁定/星标检测可能不准） |
| `--artifact-substat-ocr <NAME>` | 副词条 OCR 后端（默认 ppocrv4） |

### 配置文件

时序参数和角色名通过 `data/good_config.json` 配置，无需命令行参数：

```json
{
  "traveler_name": "",
  "wanderer_name": "",
  "manekin_name": "",
  "manekina_name": "",
  "char_tab_delay": 400,
  "char_open_delay": 1200,
  "weapon_grid_delay": 60,
  "weapon_scroll_delay": 200,
  "artifact_grid_delay": 60,
  "artifact_scroll_delay": 200
}
```

## 从源码构建

```shell
# 需要 stable Rust 工具链
rustup default stable

# 确保安装 Git LFS
git lfs pull

# 构建
cargo build --release

# 产物位于 target/release/GOODScanner.exe
```

## 致谢

- [wormtql/yas](https://github.com/wormtql/yas) — 原始项目，提供核心 OCR 扫描框架
- [1803233552/yas](https://github.com/1803233552/yas) — fork 版本，本项目基于此分支开发
- [Andrewthe13th/Inventory_Kamera](https://github.com/Andrewthe13th/Inventory_Kamera) — GOOD 格式扫描器的参考实现

## 反馈

- [GitHub Issues](https://github.com/Anyrainel/GOODScanner/issues)
