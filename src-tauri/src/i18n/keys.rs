#[derive(Debug, Clone, Copy)]
pub enum ClipboardMenuKey {
    Paste,
    PasteAsPlainText,
    PasteAsPath,
    Copy,
    SaveImage,
    OpenLink,
    SendEmail,
    RevealInFinder,
    RevealInExplorer,
    Favorite,
    Unfavorite,
    PinItem,
    UnpinItem,
    MoveToGroup,
    AddNote,
    EditNote,
    Delete,
}

#[derive(Debug, Clone, Copy)]
pub enum CommandKey {
    DragSourceFilesMissing,
    DragImageMissing,
    DragTextEmpty,
    ExternalUrlUnsupported,
    OcrModelMissing,
    OcrEngineBuildFailed,
    OcrRecognizeFailed,
    OcrPackUnknown,
    OcrPackRequestFailed,
    OcrPackHttpError,
    OcrPackDownloadInterrupted,
    OcrPackDownloadFailed,
    OcrPackSizeMismatch,
    OcrPackHashMismatch,
}

#[derive(Debug, Clone, Copy)]
pub enum TrayKey {
    Snip,
    Preference,
    StartListening,
    StopListening,
    OpenSourceAddress,
    CheckForUpdates,
    Version,
    Relaunch,
    Exit,
}
