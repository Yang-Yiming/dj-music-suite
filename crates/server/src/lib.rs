//! Local web UI for dj-music-suite: a small axum server bound to 127.0.0.1
//! that wraps the core convert/import flows for non-CLI users.

pub mod api;
pub mod state;

use std::sync::Arc;

use dj_music_core::config;

pub struct ServeOpts {
    pub port: u16,
    /// open the system browser after the listener is up
    pub open_browser: bool,
}

pub fn serve(opts: ServeOpts) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let Some(paths) = config::paths() else {
        return Err("cannot determine the config directory".into());
    };
    let state = Arc::new(state::AppState::new(paths));

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        let addr = std::net::SocketAddr::from(([127, 0, 0, 1], opts.port));
        let listener = tokio::net::TcpListener::bind(addr).await?;
        let url = format!("http://{addr}");
        println!("dj-music-suite web UI listening on {url} (Ctrl-C to stop)");
        if opts.open_browser {
            std::thread::spawn(move || open_browser(&url));
        }
        axum::serve(listener, api::router(state)).await?;
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    })?;
    Ok(())
}

fn open_browser(url: &str) {
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(url).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
    #[cfg(windows)]
    let _ = std::process::Command::new("cmd")
        .args(["/c", "start", "", url])
        .spawn();
}
