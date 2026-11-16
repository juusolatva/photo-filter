mod app;
mod export;
mod histogram_ui;
mod preset_ui;
mod preview;
mod resize;
use std::sync::Arc;

fn main() -> eframe::Result {
    let mut software = false;
    let mut path = None;
    for arg in std::env::args_os().skip(1) {
        if arg == "--software-gpu" {
            software = true;
        } else if arg == "--help" || arg == "-h" {
            println!("Photo Filter Studio\nUsage: filter-app [--software-gpu] [IMAGE]");
            return Ok(());
        } else if path.is_none() {
            path = Some(std::path::PathBuf::from(arg));
        } else {
            eprintln!("Expected at most one image path");
            std::process::exit(2);
        }
    }
    let mut setup = egui_wgpu::WgpuSetupCreateNew::without_display_handle();
    setup.instance_descriptor.backends =
        wgpu::Backends::from_env().unwrap_or(wgpu::Backends::PRIMARY) & wgpu::Backends::PRIMARY;
    if setup.instance_descriptor.backends.is_empty() {
        eprintln!("The image renderer needs Vulkan, DX12 or Metal; GL is unsupported.");
        std::process::exit(2);
    }
    setup.device_descriptor = Arc::new(|adapter| {
        let supported = adapter.limits();
        let limits = wgpu::Limits {
            max_texture_dimension_2d: supported.max_texture_dimension_2d,
            max_buffer_size: supported.max_buffer_size,
            max_storage_buffer_binding_size: supported.max_storage_buffer_binding_size,
            ..Default::default()
        };
        wgpu::DeviceDescriptor {
            label: Some("shared UI and filter device"),
            required_limits: limits,
            required_features: adapter.features() & wgpu::Features::FLOAT32_FILTERABLE,
            ..Default::default()
        }
    });
    if software {
        // eframe enumerates native adapters, so explicitly select the software device.
        setup.native_adapter_selector = Some(Arc::new(|adapters, surface| {
            adapters.iter().find(|a| a.get_info().device_type == wgpu::DeviceType::Cpu && surface.is_none_or(|s| a.is_surface_supported(s)))
                .cloned().ok_or_else(|| "No software GPU found. Install Mesa lavapipe on Linux or enable WARP on Windows.".to_owned())
        }));
    }
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1200.0, 800.0])
            .with_min_inner_size([640.0, 400.0]),
        wgpu_options: egui_wgpu::WgpuConfiguration {
            wgpu_setup: setup.into(),
            ..Default::default()
        },
        ..Default::default()
    };
    eframe::run_native(
        "Photo Filter Studio",
        options,
        Box::new(move |cc| Ok(Box::new(app::PhotoApp::new(cc, path)))),
    )
}
