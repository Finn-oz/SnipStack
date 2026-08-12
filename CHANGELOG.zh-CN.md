# 更新日志

SnipStack 的所有重要变更都会记录在本文件中。

SnipStack 是 [EcoPaste](https://github.com/EcoPasteHub/EcoPaste) 的硬分叉
(2026-08 从上游 v1.1.0 线分叉)。继承的剪贴板管理器代码历史请查阅上游更新日志。

## [Unreleased]

### 新增

- 项目从 EcoPaste 分叉并更名为 SnipStack
  (标识符 `com.snipstack.app`,备份扩展名 `.snipstackbak`,
  数据目录 `SnipStackData`)。

### 变更

- 自动更新端点改为指向 SnipStack 的 GitHub Releases
  (`latest.json` 约定);首个版本发布前禁用更新产物构建。

### 计划中

- 截屏取字 OCR:全局热键 → 框选区域 → 离线 OCR
  (PP-OCRv5 mobile,中英文)→ 剪贴板 + 历史库。
- 框选区域内 QR/条码识别。
- OCR 结果换行处理模式(保留 / 合并)。
- 复制图片后台 OCR,文本可全文搜索。
- 剪贴板隐私:遵守监控排除格式约定;历史条数/保留期限制。
