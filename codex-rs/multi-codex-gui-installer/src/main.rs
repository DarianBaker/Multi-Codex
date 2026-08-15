use anyhow::Context;
use codex_quota_proxy::win_env::binary_install_dir;
use codex_quota_proxy::win_env::update_user_path;
use native_windows_derive::NwgUi;
use native_windows_gui as nwg;
use nwg::NativeUi;
use std::path::PathBuf;

const MULTI_CODEX_EXE: &[u8] = include_bytes!("../../target/release/multi-codex.exe");
const CODEX_QUOTA_PROXY_EXE: &[u8] = include_bytes!("../../target/release/codex-quota-proxy.exe");
const CODEX_EXE: &[u8] = include_bytes!("../../target/release/codex.exe");
const CODEX_CODE_MODE_HOST_EXE: &[u8] =
    include_bytes!("../../target/release/codex-code-mode-host.exe");

#[derive(Default, NwgUi)]
pub struct InstallerApp {
    #[nwg_control(size: (440, 220), position: (300, 300), title: "Multi-Codex Installer", flags: "WINDOW|VISIBLE")]
    #[nwg_events(OnWindowClose: [InstallerApp::exit], OnInit: [InstallerApp::init])]
    window: nwg::Window,

    #[nwg_control(parent: window, text: "Multi-Codex Installer", position: (15, 15), size: (410, 25))]
    title_label: nwg::Label,

    #[nwg_control(parent: window, text: "Installs multi-codex, codex-quota-proxy, codex, and codex-code-mode-host to %LOCALAPPDATA%\\multi-codex\\bin and adds that folder to your PATH.", position: (15, 45), size: (410, 45))]
    description_label: nwg::Label,

    #[nwg_control(parent: window, text: "Install", position: (15, 100), size: (120, 32))]
    #[nwg_events(OnButtonClick: [InstallerApp::on_install])]
    install_button: nwg::Button,

    #[nwg_control(parent: window, position: (15, 145), size: (410, 20))]
    progress_bar: nwg::ProgressBar,

    #[nwg_control(parent: window, text: "", position: (15, 172), size: (410, 30))]
    status_label: nwg::Label,

    #[nwg_control(parent: window, text: "Close", position: (320, 172), size: (105, 30))]
    #[nwg_events(OnButtonClick: [InstallerApp::exit])]
    close_button: nwg::Button,
}

impl InstallerApp {
    fn init(&self) {
        self.progress_bar.set_range(0..4);
    }

    fn on_install(&self) {
        self.install_button.set_enabled(false);
        self.status_label.set_text("Installing...");

        match self.run_install() {
            Ok(install_dir) => {
                self.progress_bar.set_pos(4);
                self.status_label.set_text(&format!(
                    "Installed to {}. Open a new terminal and run `multi-codex login <label>` \
                     or `multi-codex setup` to add accounts.",
                    install_dir.display()
                ));
            }
            Err(error) => {
                self.status_label.set_text(&format!("Install failed: {error:#}"));
                self.install_button.set_enabled(true);
            }
        }
    }

    fn run_install(&self) -> anyhow::Result<PathBuf> {
        let install_dir = binary_install_dir()?;
        std::fs::create_dir_all(&install_dir).with_context(|| {
            format!("could not create install directory {}", install_dir.display())
        })?;

        std::fs::write(install_dir.join("multi-codex.exe"), MULTI_CODEX_EXE)
            .context("could not write multi-codex.exe")?;
        self.progress_bar.set_pos(1);

        std::fs::write(install_dir.join("codex-quota-proxy.exe"), CODEX_QUOTA_PROXY_EXE)
            .context("could not write codex-quota-proxy.exe")?;
        self.progress_bar.set_pos(2);

        std::fs::write(install_dir.join("codex.exe"), CODEX_EXE).context("could not write codex.exe")?;
        self.progress_bar.set_pos(3);

        std::fs::write(
            install_dir.join("codex-code-mode-host.exe"),
            CODEX_CODE_MODE_HOST_EXE,
        )
        .context("could not write codex-code-mode-host.exe")?;

        let path_var_name = std::env::var("MULTI_CODEX_WIZARD_TEST_ENV_VAR")
            .unwrap_or_else(|_| "PATH".to_string());
        let runtime = tokio::runtime::Runtime::new().context("could not start an async runtime")?;
        runtime.block_on(update_user_path(&path_var_name, &install_dir))?;

        Ok(install_dir)
    }

    fn exit(&self) {
        nwg::stop_thread_dispatch();
    }
}

fn main() -> anyhow::Result<()> {
    nwg::init().context("failed to init Native Windows GUI")?;
    nwg::Font::set_global_family("Segoe UI").ok();
    let _app = InstallerApp::build_ui(Default::default()).context("failed to build the installer UI")?;
    nwg::dispatch_thread_events();
    Ok(())
}
