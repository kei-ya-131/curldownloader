#![windows_subsystem = "windows"]

use curl_downloader::{
    app::CurlDownloaderApp, ipc, native_host, native_registration, session_shutdown,
    single_instance, startup_policy, storage,
};

fn main() -> eframe::Result {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    if native_host::is_native_host_invocation(&arguments) {
        if let Err(error) = native_host::run_native_host() {
            eprintln!("Native Messaging host error: {error}");
        }
        return Ok(());
    }

    let register_native = should_register_native(&arguments);
    run_gui(
        arguments.iter().any(|argument| argument == "--minimized"),
        register_native,
    )
}

fn initial_viewport_visible(minimized: bool) -> bool {
    !minimized
}
fn should_register_native(arguments: &[std::ffi::OsString]) -> bool {
    // Automatic restarts of an isolated smoke probe must not replace the
    // user's Firefox registration with a temporary executable path.
    #[cfg(feature = "smoke-test-native-auth")]
    if std::env::var_os("CURL_DOWNLOADER_TEST_NATIVE_CLIENT").is_some() {
        return false;
    }
    !arguments
        .iter()
        .any(|argument| argument == "--skip-native-registration")
}

fn preferred_adapter_index(candidates: &[(eframe::wgpu::DeviceType, bool)]) -> Option<usize> {
    use eframe::wgpu::DeviceType;
    candidates
        .iter()
        .enumerate()
        .filter(|(_, (_, supported))| *supported)
        .min_by_key(|(_, (kind, _))| match kind {
            DeviceType::IntegratedGpu => 0,
            DeviceType::Cpu => 1,
            DeviceType::Other | DeviceType::VirtualGpu => 2,
            DeviceType::DiscreteGpu => 3,
        })
        .map(|(index, _)| index)
}

fn low_power_wgpu_options() -> eframe::WgpuConfiguration {
    let mut setup = eframe::egui_wgpu::WgpuSetupCreateNew::without_display_handle();
    setup.power_preference = eframe::wgpu::PowerPreference::LowPower;
    #[cfg(windows)]
    {
        // One backend avoids probing both Vulkan and DX12 on a hybrid-GPU PC.
        setup.instance_descriptor.backends = eframe::wgpu::Backends::DX12;
    }
    setup.native_adapter_selector = Some(std::sync::Arc::new(|adapters, surface| {
        let candidates = adapters
            .iter()
            .map(|adapter| {
                (
                    adapter.get_info().device_type,
                    surface.is_none_or(|surface| adapter.is_surface_supported(surface)),
                )
            })
            .collect::<Vec<_>>();
        let index = preferred_adapter_index(&candidates)
            .ok_or_else(|| "找不到可呈現視窗的繪圖裝置".to_owned())?;
        let adapter = adapters[index].clone();
        // No request URL or credentials are included in this diagnostic.
        eprintln!("Curl Downloader GPU: {:?}", adapter.get_info());
        #[cfg(feature = "smoke-test-native-auth")]
        if let Some(path) = std::env::var_os("CURL_DOWNLOADER_TEST_GPU_DIAGNOSTIC") {
            let _ = std::fs::write(path, format!("{:?}", adapter.get_info()));
        }
        Ok(adapter)
    }));
    eframe::WgpuConfiguration {
        wgpu_setup: setup.into(),
        ..Default::default()
    }
}

fn run_gui(minimized: bool, register_native: bool) -> eframe::Result {
    if register_native
        && let Ok(executable) = std::env::current_exe()
        && let Err(error) = native_registration::ensure_registered(&executable)
    {
        eprintln!("Firefox Native host 自動註冊失敗：{error}");
    }

    let _instance = match single_instance::acquire() {
        Ok(Some(instance)) => instance,
        Ok(None) => {
            let request = ipc::show_window_request();
            let _ = ipc::call_pipe_with_retry(
                &request,
                std::time::Duration::from_millis(100),
                std::time::Duration::from_millis(50),
                40,
            );
            return Ok(());
        }
        Err(error) => {
            eprintln!("GUI 單例初始化失敗：{error}");
            return Ok(());
        }
    };
    let _ = session_shutdown::reset_for_gui_start();
    if !minimized && let Ok(state_path) = storage::state_path() {
        let _ = startup_policy::clear_manual_stop(&storage::manual_stop_path(&state_path));
    }
    let options = eframe::NativeOptions {
        wgpu_options: low_power_wgpu_options(),
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Curl Downloader")
            .with_visible(initial_viewport_visible(minimized))
            .with_inner_size([1180.0, 720.0])
            .with_min_inner_size([860.0, 560.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Curl Downloader",
        options,
        Box::new(move |cc| match CurlDownloaderApp::new(cc, minimized) {
            Ok(app) => Ok(Box::new(app)),
            Err(error) => {
                rfd::MessageDialog::new()
                    .set_title("Curl Downloader 啟動失敗")
                    .set_description(format!("無法安全載入狀態，原有任務資料已保留。\n{error}"))
                    .set_level(rfd::MessageLevel::Error)
                    .show();
                Err(Box::new(error))
            }
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::{initial_viewport_visible, should_register_native};

    #[test]
    fn integrated_gpu_wins_over_discrete_regardless_of_enumeration_order() {
        use eframe::wgpu::DeviceType::*;
        assert_eq!(
            super::preferred_adapter_index(&[(DiscreteGpu, true), (IntegratedGpu, true)]),
            Some(1)
        );
        assert_eq!(
            super::preferred_adapter_index(&[(IntegratedGpu, true), (DiscreteGpu, true)]),
            Some(0)
        );
    }

    #[test]
    fn gpu_selection_skips_adapters_that_cannot_present_the_window() {
        use eframe::wgpu::DeviceType::*;
        assert_eq!(
            super::preferred_adapter_index(&[(IntegratedGpu, false), (DiscreteGpu, true)]),
            Some(1)
        );
        assert_eq!(
            super::preferred_adapter_index(&[(DiscreteGpu, true), (Cpu, true)]),
            Some(1)
        );
        assert_eq!(
            super::preferred_adapter_index(&[(IntegratedGpu, false)]),
            None
        );
    }

    #[test]
    fn lifecycle_probe_can_skip_native_registration() {
        let arguments = vec![
            std::ffi::OsString::from("--minimized"),
            std::ffi::OsString::from("--skip-native-registration"),
        ];
        assert!(!should_register_native(&arguments));
        assert!(should_register_native(&[std::ffi::OsString::from(
            "--minimized"
        )]));
    }

    #[test]
    fn minimized_gui_starts_a_viewport_before_hiding_to_tray() {
        assert!(!initial_viewport_visible(true));
        assert!(initial_viewport_visible(false));
    }
}
