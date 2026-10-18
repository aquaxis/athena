//! The Studio app (spec §25, P7-01): the Rust → WebAssembly pipeline's library half —
//! the app state the wasm face boots with, the static asset set the server emits, and
//! the API root the app talks to (the UI rides the Core API only, `ATH-API-001`).

use std::path::PathBuf;

/// The Studio's kernel face: which asset the server serves, and the API root the
/// wasm app binds to (`athena-studio` talks to `athena-api` **only** — never a Core
/// internal, `ATH-API-001`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StudioApp {
    /// The API root, e.g. `http://127.0.0.1:8080`.
    pub api_root: String,
}

impl StudioApp {
    /// Boot the app face for one API root.
    ///
    /// # Errors
    /// A root without an `http` scheme refuses (`ATH-API-001`: the UI never reaches a
    /// Core internal through another channel).
    pub fn boot(api_root: impl Into<String>) -> Result<Self, String> {
        let root = api_root.into();
        if root.starts_with("http://") || root.starts_with("https://") {
            Ok(Self { api_root: root })
        } else {
            Err("the studio's api root must be an http/https URL (ATH-API-001)".into())
        }
    }

    /// The named asset's body (the static set the server emits).
    ///
    /// # Errors
    /// An unknown asset name.
    pub fn asset(&self, name: Asset) -> Result<AssetBody, String> {
        Ok(match name {
            Asset::IndexHtml => AssetBody {
                content_type: "text/html; charset=utf-8",
                bytes: index_html(&self.api_root).into_bytes(),
            },
            Asset::AppWasm => AssetBody {
                content_type: "application/wasm",
                bytes: app_wasm_placeholder(),
            },
        })
    }
}

/// The static asset names the server emits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Asset {
    /// The page shell.
    IndexHtml,
    /// The wasm module.
    AppWasm,
}

/// One asset's body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetBody {
    /// The HTTP content type.
    pub content_type: &'static str,
    /// The bytes.
    pub bytes: Vec<u8>,
}

fn app_wasm_placeholder() -> Vec<u8> {
    // The wasm module's real build lands with the §25 views; the placeholder names the
    // module the pipeline emits (checked by the CI build leg, P7-01's pipeline face).
    b"athena-studio wasm module (built by the Rust -> wasm pipeline)".to_vec()
}

fn index_html(api_root: &str) -> String {
    format!(
        "<!doctype html><title>ATHENA Studio</title>\n\
         <script type=\"module\">\n  const API_ROOT = \"{api_root}\";\n  \
         import init from './athena_studio.js';\n  init().then(() => {{}});\n</script>\n\
         <div id=\"app-root\"></div>\n"
    )
}

/// The build pipeline (P7-01): the wasm target's build face — the CI step's command
/// sequence and the produced artifact paths. The runner itself is
/// `scripts/build-studio.sh`; this face is what tests and CI assert against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildPlan {
    /// The command sequence, in order.
    pub commands: Vec<&'static str>,
    /// The produced artifacts.
    pub artifacts: Vec<PathBuf>,
}

impl BuildPlan {
    /// The phase-7 pipeline (the `wasm32-unknown-unknown` target).
    ///
    /// # Errors
    /// Never — the plan is static.
    #[must_use]
    pub fn phase7() -> Self {
        Self {
            commands: vec![
                "rustup target add wasm32-unknown-unknown",
                "cargo build -p athena-studio --target wasm32-unknown-unknown --release",
            ],
            artifacts: vec![
                PathBuf::from("target/wasm32-unknown-unknown/release/athena_studio.wasm"),
                PathBuf::from("target/wasm32-unknown-unknown/release/athena_studio.js"),
            ],
        }
    }
}

/// The static server's kernel face (P7-01): one HTTP request's answer — the content
/// type plus the bytes — resolved by the app's asset set. `main.rs` runs the socket
/// loop around this face.
///
/// # Errors
/// An unknown path returns `None` (the caller renders 404).
#[must_use]
pub fn answer(app: &StudioApp, path: &str) -> Option<(&'static str, Vec<u8>)> {
    let asset = match path {
        "/" | "/index.html" => Asset::IndexHtml,
        "/athena_studio.wasm" => Asset::AppWasm,
        _ => return None,
    };
    let body = app.asset(asset).ok()?;
    Some((body.content_type, body.bytes))
    // (The tuple keeps the caller's header render simple: type twice is the deliberate
    // shape this build's tests assert; the transport's HTTP header face is the caller's.)
}

/// The wasm face's boot contract (the §25 range is later phases' build): which asset the
/// module expects at boot.
#[must_use]
pub const fn wasm_boot_asset() -> Asset {
    Asset::AppWasm
}

/// Ensure the studio's bin contract: the `--api` argument parses into a boot face.
///
/// # Errors
/// The boot face's own fault.
///
/// # Panics
/// None (the caller owns the failure text).
pub fn boot_from_args(args: &[String]) -> Result<StudioApp, String> {
    let index = args.iter().position(|arg| arg == "--api");
    let root = index
        .and_then(|position| args.get(position + 1))
        .cloned()
        .unwrap_or_else(|| "http://127.0.0.1:8080".to_string());
    StudioApp::boot(root)
}

/// The default serve port (spec §4.3.2: 5173).
pub const SERVICE_PORT: u16 = 5173;

/// The paths the wasm pipeline's outputs ride (`BuildPlan::phase7`'s face, mirrored in
/// `scripts/build-studio.sh`'s outputs).
pub mod actions;
pub mod client;
pub mod detail_views;
pub mod live;
pub mod views;
pub mod wasm {
    /// The wasm module artifact path (relative to the workspace root).
    #[must_use]
    pub fn module_path(workspace_root: &std::path::Path) -> std::path::PathBuf {
        workspace_root.join("target/wasm32-unknown-unknown/release/athena_studio.wasm")
    }
}
