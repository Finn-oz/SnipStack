# SnipStack

**框选屏幕任意文字,留住你复制过的一切。**

SnipStack 是一款 Windows 11 工具,把 TextSniper 式的截屏取字 OCR 与完整的剪贴板
历史管理合为一体:

- **框选即取字**:按下全局热键,在屏幕任意位置画一个框(视频、PDF、远程桌面、
  禁止复制的界面……),识别出的文字直接进入剪贴板。完全离线 OCR(PP-OCRv5),
  首发支持简体中文 + 英文混排,并支持二维码/条码识别。
- **一切皆可搜索**:每次截屏取字(原图 + 识别文本)和每次复制(文本 + 图片)
  都会进入本地全文搜索历史——凭图片里的文字就能把截图找回来。
- **本地优先、注重隐私**:无云端、无账号。遵守密码管理器的剪贴板排除格式约定;
  历史条数与保留期可配置。

> **状态:早期开发中。** 剪贴板历史基础已可用;截屏取字管线建设中。仅支持
> Windows 11。

[English README](./README.md)

## 开发

环境要求:Windows 11、[Rust](https://rustup.rs/)(版本见
`rust-toolchain.toml`)、Node.js + [pnpm](https://pnpm.io/)。

```bash
pnpm install
pnpm tauri dev
```

## 路线图

- **M1** — 框选取字 MVP:热键 → 每显示器框选覆盖层 → 离线 PP-OCRv5 识别 →
  剪贴板 + 历史库。
- **M2** — 二维码/条码识别、换行处理模式、复制图片后台 OCR、剪贴板隐私约定、
  历史保留期。
- **M3** — 多显示器混合 DPI 打磨、可下载语言包、NSIS 安装包、v0.1 发布。

## 致谢与许可

SnipStack 是 [EcoPaste](https://github.com/EcoPasteHub/EcoPaste)
(作者 [ayangweb](https://github.com/ayangweb))的硬分叉——剪贴板监听、FTS5
存储、窗口管理与设置等基础设施均来自上游,特此致谢!

基于 [Apache License 2.0](./LICENSE) 开源,另见 [NOTICE](./NOTICE)。
