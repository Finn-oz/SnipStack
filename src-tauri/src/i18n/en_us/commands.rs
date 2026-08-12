use crate::i18n::keys::CommandKey as Key;

/// 返回美式英文 Tauri 命令错误根因文案。
pub fn label(key: Key) -> &'static str {
    match key {
        Key::DragSourceFilesMissing => "The dragged source files no longer exist",
        Key::DragImageMissing => "The image file no longer exists",
        Key::DragTextEmpty => "Text content is empty",
        Key::ExternalUrlUnsupported => "Only links starting with http or https can be opened",
        Key::OcrModelMissing => "OCR model files are missing, please reinstall the app",
        Key::OcrEngineBuildFailed => "Could not build the OCR engine",
        Key::OcrRecognizeFailed => "Text recognition could not complete",
        Key::OcrPackUnknown => "Unknown language pack",
        Key::OcrPackRequestFailed => "Network request failed",
        Key::OcrPackHttpError => "The download source returned an error",
        Key::OcrPackDownloadInterrupted => "Download interrupted",
        Key::OcrPackDownloadFailed => "No download source is reachable",
        Key::OcrPackSizeMismatch => "Downloaded file is incomplete and was discarded",
        Key::OcrPackHashMismatch => {
            "Downloaded file failed verification (corrupt or tampered) and was discarded"
        }
    }
}
