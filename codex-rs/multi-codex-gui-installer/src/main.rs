use anyhow::Context;
use codex_quota_proxy::win_env::binary_install_dir;
use codex_quota_proxy::win_env::update_user_path;
use native_windows_derive::NwgUi;
use native_windows_gui as nwg;
use nwg::NativeUi;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;

const MULTI_CODEX_EXE: &[u8] = include_bytes!("../../target/release/multi-codex.exe");
const CODEX_QUOTA_PROXY_EXE: &[u8] = include_bytes!("../../target/release/codex-quota-proxy.exe");
const CODEX_EXE: &[u8] = include_bytes!("../../target/release/codex.exe");

/// Windows `CREATE_NEW_CONSOLE` process creation flag — spawns `multi-codex
/// login` in its own console window (it needs one for the OAuth
/// device-code fallback text) instead of trying to attach to this GUI
/// process, which has no console of its own.
const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;

#[derive(Default, NwgUi)]
pub struct InstallerApp {
    #[nwg_control(size: (440, 330), position: (300, 300), title: "Multi-Codex Installer", flags: "WINDOW|VISIBLE")]
    #[nwg_events(OnWindowClose: [InstallerApp::exit], OnInit: [InstallerApp::init])]
    window: nwg::Window,

    #[nwg_control(parent: window, text: "Multi-Codex Installer", position: (15, 15), size: (410, 25))]
    title_label: nwg::Label,

    #[nwg_control(parent: window, text: "Installs multi-codex, codex-quota-proxy, and codex to %LOCALAPPDATA%\\multi-codex\\bin and adds that folder to your PATH.", position: (15, 45), size: (410, 45))]
    description_label: nwg::Label,

    #[nwg_control(parent: window, text: "Install", position: (15, 100), size: (120, 32))]
    #[nwg_events(OnButtonClick: [InstallerApp::on_install])]
    install_button: nwg::Button,

    #[nwg_control(parent: window, position: (15, 145), size: (410, 20))]
    progress_bar: nwg::ProgressBar,

    #[nwg_control(parent: window, text: "", position: (15, 172), size: (410, 60))]
    status_label: nwg::Label,

    #[nwg_control(parent: window, text: "Account label:", position: (15, 240), size: (100, 20))]
    account_label_prompt: nwg::Label,

    #[nwg_control(parent: window, text: "work", position: (120, 237), size: (150, 24))]
    account_label_input: nwg::TextInput,

    #[nwg_control(parent: window, text: "This is my main / fallback account", position: (15, 267), size: (280, 20))]
    main_account_checkbox: nwg::CheckBox,

    #[nwg_control(parent: window, text: "Log In", position: (15, 292), size: (120, 30))]
    #[nwg_events(OnButtonClick: [InstallerApp::on_login])]
    login_button: nwg::Button,

    #[nwg_control(parent: window, text: "Close", position: (320, 292), size: (105, 30))]
    #[nwg_events(OnButtonClick: [InstallerApp::exit])]
    close_button: nwg::Button,
}

impl InstallerApp {
    fn init(&self) {
        self.progress_bar.set_range(0..3);
        self.account_label_prompt.set_visible(false);
        self.account_label_input.set_visible(false);
        self.main_account_checkbox.set_visible(false);
        self.login_button.set_visible(false);
    }

    fn on_install(&self) {
        self.install_button.set_enabled(false);
        self.status_label.set_text("Installing...");

        match self.run_install() {
            Ok(install_dir) => {
                self.progress_bar.set_pos(3);
                self.status_label.set_text(&format!(
                    "Installed to {}. Open a new terminal to use `multi-codex` as a bare command.",
                    install_dir.display()
                ));
                self.account_label_prompt.set_visible(true);
                self.account_label_input.set_visible(true);
                self.main_account_checkbox.set_visible(true);
                self.login_button.set_visible(true);
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
        std::fs::write(install_dir.join("codex.exe"), CODEX_EXE).context("could not write codex.exe")?;
        self.progress_bar.set_pos(2);

        let path_var_name = std::env::var("MULTI_CODEX_WIZARD_TEST_ENV_VAR")
            .unwrap_or_else(|_| "PATH".to_string());
        let runtime = tokio::runtime::Runtime::new().context("could not start an async runtime")?;
        runtime.block_on(update_user_path(&path_var_name, &install_dir))?;

        Ok(install_dir)
    }

    fn on_login(&self) {
        let label = self.account_label_input.text();
        let label = label.trim().to_string();
        if label.is_empty() {
            self.status_label.set_text("Enter an account label first.");
            return;
        }

        let is_main = self.main_account_checkbox.check_state() == nwg::CheckBoxState::Checked;
        let install_dir = match binary_install_dir() {
            Ok(dir) => dir,
            Err(error) => {
                self.status_label
                    .set_text(&format!("Could not find the install directory: {error:#}"));
                return;
            }
        };
        let multi_codex = install_dir.join("multi-codex.exe");

        let mut command = std::process::Command::new(&multi_codex);
        command.arg("login").arg(&label);
        if is_main {
            command.arg("--main");
        }
        command.creation_flags(CREATE_NEW_CONSOLE);

        match command.spawn() {
            Ok(_) => {
                self.status_label.set_text(
                    "A window opened to complete login in your browser. You can close this \
                     installer once you're done.",
                );
            }
            Err(error) => {
                self.status_label.set_text(&format!("Could not start login: {error}"));
            }
        }
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
