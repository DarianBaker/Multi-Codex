use anyhow::Context;
use anyhow::Result;
use embed_manifest::embed_manifest;
use embed_manifest::new_manifest;

fn main() -> Result<()> {
    // native-windows-gui needs Common Controls v6 (via an embedded manifest)
    // to resolve at load time — without this, the built .exe fails to start
    // at all with STATUS_ENTRYPOINT_NOT_FOUND, since Windows falls back to
    // an older Common Controls version some of nwg's control code doesn't
    // find its expected entry points in.
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        embed_manifest(new_manifest("MultiCodexInstaller")).context("unable to embed manifest file")?;
    }
    println!("cargo:rerun-if-changed=build.rs");
    Ok(())
}
