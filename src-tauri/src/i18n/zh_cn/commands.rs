use crate::i18n::keys::CommandKey as Key;

/// 返回简体中文 Tauri 命令错误根因文案。
pub fn label(key: Key) -> &'static str {
    match key {
        Key::DragSourceFilesMissing => "拖拽源文件已不存在",
        Key::DragImageMissing => "图片文件已不存在",
        Key::DragTextEmpty => "文本内容为空",
        Key::ExternalUrlUnsupported => "只能打开 http 或 https 开头的链接",
        Key::OcrModelMissing => "OCR 模型文件缺失,请重新安装应用",
        Key::OcrEngineBuildFailed => "无法构建 OCR 识别引擎",
        Key::OcrRecognizeFailed => "文字识别未能完成",
        Key::OcrPackUnknown => "未知的语言包",
        Key::OcrPackRequestFailed => "网络请求失败",
        Key::OcrPackHttpError => "下载源返回错误",
        Key::OcrPackDownloadInterrupted => "下载中断",
        Key::OcrPackDownloadFailed => "所有下载源均不可用",
        Key::OcrPackSizeMismatch => "下载的文件不完整,已丢弃",
    }
}
