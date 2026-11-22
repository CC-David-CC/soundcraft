use std::path::{Path, PathBuf};

pub enum AccountSessionRequest {
    New(serde_json::Value),
    Open,
    Save,
    SaveAs,
    Revert,
    Close,
}

pub type AccountSessionFn = Box<dyn FnMut(AccountSessionRequest) -> Result<(), String>>;

/// Transitions reported by an asynchronous microphone input.
pub enum CaptureEvent {
    Started,
    Finished(craft_audio::AudioBuffer, String),
    Failed(String),
}

/// A host that captures PCM without native input devices or take files.
pub trait Microphone {
    fn prepare(&mut self, rate: u32, max_frames: u32);
    fn poll(&mut self) -> Option<CaptureEvent>;
    fn stop(&mut self);
    fn cancel(&mut self);
}

/// Host services injected by the app. File dialogs return paths as picked. The caller converts
/// them to text where commands need it ([`crate::dialogs::unicode`]).
#[derive(Default)]
pub struct Services {
    pub microphone: Option<Box<dyn Microphone>>,
    /// Delegate the account session lifecycle to the WebOS desktop.
    pub account_session: Option<AccountSessionFn>,
    pub pick_open: Option<Box<dyn Fn(&str, &[&str]) -> Option<PathBuf>>>,
    /// Like `pick_open`, but the user may select several files (Import Audio). Empty when cancelled.
    pub pick_open_many: Option<Box<dyn Fn(&str) -> Vec<PathBuf>>>,
    /// The purpose and the file to start from.
    pub pick_save: Option<Box<dyn Fn(&str, &Path) -> Option<PathBuf>>>,
    /// The folder to start in.
    pub pick_folder: Option<Box<dyn Fn(&Path) -> Option<PathBuf>>>,
}
