# SnipStack

**框选屏幕任意文字,留住你复制过的一切。**

SnipStack 是一款免费开源的 Windows 11 工具,把 TextSniper 式的截屏取字 OCR
与完整的剪贴板历史管理合为一体:

- **框选即取字**:按下全局热键(默认 `Alt+S`),在屏幕任意位置画一个框(视频、
  PDF、远程桌面、禁止复制的界面……),识别出的文字直接进入剪贴板和历史。完全
  离线 OCR(PP-OCRv5),内置模型覆盖简繁中文、英文与日文,韩文、拉丁字母语言、
  俄语、泰文、阿拉伯文可在应用内下载语言包;支持二维码/条码识别。
- **一切皆可搜索**:每次截屏取字和每次复制(文本 + 图片)都会进入本地全文
  搜索历史——复制的图片会在后台识别文字,凭图里的字就能把截图找回来。
- **本地优先、注重隐私**:无云端、无账号、无遥测。遵守密码管理器的剪贴板
  排除格式约定;历史条数与保留期可配置。

[English README](./README.md)

## 下载安装

**[下载最新版本](https://github.com/Finn-oz/SnipStack/releases/latest)**
——获取 `SnipStack_x.y.z_x64-setup.exe`(Windows 11,x64)。

1. 运行安装器。按当前用户安装,**不需要管理员权限**。
2. Windows SmartScreen 可能提示"未知发布者"(安装包暂未做代码签名)。确认
   文件来自官方 Releases 页面后,点击**更多信息 → 仍要运行**。也可以先校验
   下载文件:

   ```powershell
   Get-FileHash .\SnipStack_x.y.z_x64-setup.exe -Algorithm SHA256
   ```

   并与 Release 说明中公布的 SHA-256 比对。
3. 从开始菜单启动 SnipStack。它常驻系统托盘(注意任务栏 `^` 溢出区)。

上手:按 `Alt+C` 打开剪贴板历史,按 `Alt+S` 框选取字。两个热键都可在
偏好设置中修改。

## 隐私与数据

- 剪贴板历史、取字结果与设置**只存储在本地**
  (`%LOCALAPPDATA%\com.snipstack.app`),不会离开你的设备。
- OCR 完全在本机运行。应用仅有的网络请求是 GitHub 更新检查和可选的
  OCR 语言包下载。
- 无账号、无云同步、无遥测统计。
- 密码管理器按标准排除格式(`ExcludeClipboardContentFromMonitorProcessing`、
  `Clipboard Viewer Ignore`、`CanIncludeInClipboardHistory=0`)标记的内容
  不会被记录;疑似密钥的文本(API key、token 等)默认不采集,可自行开启。
  这些约定是尽力而为,不构成安全边界——请像对待其他本地文件一样对待你的
  历史数据库。
- 历史条数/保留期可设上限,可随时删除单条或全部记录。

## 已知限制

- 仅支持 Windows 11 x64。
- 安装包暂未签名,首次运行会出现 SmartScreen 提示(见上)。
- "以管理员运行"(用于向提权窗口粘贴)在本版本中暂不可用,待相关流程修复。
- 选择下载的语言包后只识别对应文字系统(附带基本英文字母),识别中文请切回
  内置模型。

## 开发

环境要求:Windows 11、[Rust](https://rustup.rs/)(版本见
`rust-toolchain.toml`)、Node.js + [pnpm](https://pnpm.io/)。

```bash
pnpm install
pnpm fetch:ocr-models
pnpm build:icon
pnpm tauri dev
```

`fetch:ocr-models` 会把 PP-OCRv5 mobile 模型(约 21 MB)下载到
`src-tauri/resources/ocr/`(不入 git)。手动验收清单见
[docs/testing-win11.md](./docs/testing-win11.md)。贡献指南见
[CONTRIBUTING.zh-CN.md](./CONTRIBUTING.zh-CN.md)。

## 发布(维护者)

推送 `v*` 标签触发发布流水线(Windows x64 NSIS 安装包,附到由双语
changelog 生成的 GitHub Release 草稿)。两个 changelog 文件都必须包含与
标签版本匹配的章节。更新产物签名依赖仓库 secrets:
`TAURI_SIGNING_PRIVATE_KEY` / `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`。

## 致谢与许可

SnipStack 是 [EcoPaste](https://github.com/EcoPasteHub/EcoPaste)
(作者 [ayangweb](https://github.com/ayangweb))的独立硬分叉——剪贴板监听、
FTS5 存储、窗口管理与设置等基础设施均来自上游,特此致谢!SnipStack 与
EcoPaste 项目无隶属或背书关系。

OCR 能力来自 [PP-OCRv5](https://github.com/PaddlePaddle/PaddleOCR) 模型,
经由 [oar-ocr](https://github.com/GreatV/oar-ocr) 使用。详见
[THIRD-PARTY-NOTICES.md](./THIRD-PARTY-NOTICES.md)。

基于 [Apache License 2.0](./LICENSE) 开源,另见 [NOTICE](./NOTICE)。
