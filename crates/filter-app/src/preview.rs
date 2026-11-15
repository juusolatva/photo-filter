use filter_gpu::{ExposureFrame, ExposureRenderer};
use std::sync::Arc;

pub struct PreviewCallback {
    pub renderer: Arc<ExposureRenderer>,
    pub frame: ExposureFrame,
}
impl egui_wgpu::CallbackTrait for PreviewCallback {
    fn paint(
        &self,
        _info: eframe::egui::PaintCallbackInfo,
        pass: &mut wgpu::RenderPass<'static>,
        _resources: &egui_wgpu::CallbackResources,
    ) {
        self.renderer.paint(pass, &self.frame);
    }
}
