mod server;

use agent_shell_core::config::Config;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::watch;

/// Global flag set by the signal handler.
static SHUTDOWN_REQUESTED: AtomicBool = AtomicBool::new(false);

extern "C" fn sig_shutdown_handler(_sig: nix::libc::c_int) {
    SHUTDOWN_REQUESTED.store(true, Ordering::Release);
}

/// Install SIGTERM/SIGINT handlers using sigaction (before tokio runtime starts).
/// This must be called BEFORE #[tokio::main] runtime initialization so that
/// the handlers are not overridden by tokio's internal signal machinery.
fn install_signal_handlers() {
    let handler = nix::sys::signal::SigHandler::Handler(sig_shutdown_handler);
    let action = nix::sys::signal::SigAction::new(
        handler,
        nix::sys::signal::SaFlags::SA_RESTART,
        nix::sys::signal::SigSet::empty(),
    );
    unsafe {
        let _ = nix::sys::signal::sigaction(nix::sys::signal::Signal::SIGTERM, &action);
        let _ = nix::sys::signal::sigaction(nix::sys::signal::Signal::SIGINT, &action);
    }
}

// Install signal handlers at program startup, before tokio runtime.
// This is done via a ctor-like approach: we call install_signal_handlers()
// at the very beginning of main().

#[tokio::main]
async fn main() {
    // Install signal handlers FIRST, before any tokio tasks are spawned.
    // tokio's signal module can override these, so we install them early
    // and use a simple AtomicBool polling approach instead.
    let config = Config::load();

    // Ensure base directory exists (before signal handler tries to write debug file)
    let base_dir = Config::base_dir();
    let _ = std::fs::create_dir_all(&base_dir);

    // Install signal handlers AFTER ensuring base_dir exists
    install_signal_handlers();

    // Write PID file
    let pid_path = base_dir.join("daemon.pid");
    let _ = std::fs::write(&pid_path, std::process::id().to_string());

    // Channel to propagate shutdown to the server accept loop
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    // Spawn a task that polls the atomic flag and triggers the watch channel
    let shutdown_trigger = shutdown_tx.clone();
    tokio::spawn(async move {
        loop {
            if SHUTDOWN_REQUESTED.load(Ordering::Acquire) {
                let _ = shutdown_trigger.send(true);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    });

    // Start the server
    let socket_path = config.socket_path();
    let socket_path_for_cleanup = socket_path.clone();
    let pid_path_for_cleanup = pid_path.clone();

    if let Err(e) = server::run(socket_path, config, shutdown_rx).await {
        eprintln!("daemon error: {}", e);
    }

    // Cleanup on exit (normal stop, signal-triggered, or error)
    let _ = std::fs::remove_file(&socket_path_for_cleanup);
    let _ = std::fs::remove_file(&pid_path_for_cleanup);
}
