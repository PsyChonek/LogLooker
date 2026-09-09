mod chart;
mod config;
mod download;
mod memcache;
mod plugins;
mod rawfile;
mod search;
mod services;
mod sync;
mod updates;

/// Browser arguments for every window we open at runtime. WebView2 refuses to
/// create a second environment with different options in the same user-data
/// folder (HRESULT 0x8007139F), so a runtime window must pass exactly the same
/// args as the main window in tauri.conf.json - otherwise its build fails and
/// the chart/raw viewer never opens. Keep this string in sync with that config.
pub const WEBVIEW_BROWSER_ARGS: &str = "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection,msOverlayScrollbarWinStyle,msOverlayScrollbarWinStyleAnimation";

pub use chart::*;
pub use config::*;
pub use download::*;
pub use memcache::*;
pub use plugins::*;
pub use rawfile::*;
pub use search::*;
pub use services::*;
pub use sync::*;
pub use updates::*;
