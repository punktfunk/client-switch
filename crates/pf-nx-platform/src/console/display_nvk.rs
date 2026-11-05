//! The `Display` on NVK: the presenter on libnx's default window, resized to the operation
//! mode's size when the console docks or undocks.

use anyhow::Result;
use pf_nx_app::platform::{Display, Fit, FrameReport, OperationMode, Refresh, Rgba};
use pf_nx_vk::Presenter;

pub struct NvkDisplay {
    presenter: Presenter,
    /// The mode the presenter's size was last asked for.
    mode: OperationMode,
}

impl NvkDisplay {
    pub fn new(presenter: Presenter, mode: OperationMode) -> Self {
        Self { presenter, mode }
    }
}

impl Display for NvkDisplay {
    fn size(&self) -> (u32, u32) {
        self.presenter.size()
    }

    fn ui_size(&self) -> (u32, u32) {
        self.presenter.ui_size()
    }

    fn refresh(&self) -> Refresh {
        self.presenter.refresh()
    }

    fn mode(&self) -> OperationMode {
        super::operation_mode()
    }

    fn frame(&mut self, ui: &mut dyn FnMut(&skia_safe::Canvas)) -> Result<FrameReport> {
        let mode = super::operation_mode();
        if mode != self.mode {
            self.mode = mode;
            let size = super::mode_size(mode);
            tracing::info!("display: {mode:?}, asking for {}x{}", size.0, size.1);
            self.presenter.resize(size);
        }
        self.presenter.frame(ui)
    }

    fn set_stream(&mut self, on: bool, fit: Fit) {
        self.presenter.set_stream(on, fit);
    }

    fn set_ui_layer(&mut self, on: bool) {
        self.presenter.set_ui_layer(on);
    }

    fn read_back(&mut self) -> Result<Option<Rgba>> {
        self.presenter.read_back()
    }
}
