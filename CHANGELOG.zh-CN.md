# 更新日志

SnipStack 的所有重要变更都会记录在本文件中。

SnipStack 是 [EcoPaste](https://github.com/EcoPasteHub/EcoPaste) 的硬分叉
(2026-08 从上游 v1.1.0 线分叉)。继承的剪贴板管理器代码历史请查阅上游更新日志。

## [Unreleased]

### 新增

- 可下载 OCR 语言包(韩文、拉丁字母语言、俄语/东斯拉夫、泰文、阿拉伯文,
  每个 8-13 MB):应用内下载、进度显示、大小校验、含备用镜像源;内置模型
  已覆盖简繁中文、英文与日文。所选语言包不可用时识别自动回落内置模型。
- 框选覆盖层聚焦光标所在显示器,多显示器下 Esc 立即可用。
- 截屏取字:全局热键(默认 `Alt+S`)或托盘菜单唤起每显示器框选覆盖层;选区经
  PP-OCRv5 mobile 离线识别(中英文),文本自动进剪贴板,截图连同识别文本存入
  历史并可全文搜索。换行模式(保留/合并)与自动复制可在新的「截屏取字」设置页配置。
- 框选区域内 QR/条码识别:发现码直接取值(多个码按行拼接),未命中回落 OCR;可配置开关。
- 复制图片后台 OCR:剪贴板收录的图片在后台识别,凭文字即可全文搜索;
  截屏取字条目与已索引条目不会被重复识别或覆盖。
- 剪贴板监听遵守 Windows 排除格式约定(`ExcludeClipboardContentFromMonitorProcessing`、
  `Clipboard Viewer Ignore`、`CanIncludeInClipboardHistory=0`),KeePass、1Password 等
  密码管理器复制的内容不会被记录。
- 剪贴板窗口新增截屏取字完成 toast(字数或错误信息)。
- 项目从 EcoPaste 分叉并更名为 SnipStack
  (标识符 `com.snipstack.app`,备份扩展名 `.snipstackbak`,
  数据目录 `SnipStackData`)。

### 变更

- 自动更新端点改为指向 SnipStack 的 GitHub Releases
  (`latest.json` 约定);首个版本发布前禁用更新产物构建。

### 变更

- 发布流水线只构建 Windows x64 NSIS 安装包,并在构建时拉取 OCR 模型。

### 计划中

- Windows 11 端到端验证与混合 DPI 测试矩阵(docs/testing-win11.md)。
- 更新器签名密钥与首个公开版本。
