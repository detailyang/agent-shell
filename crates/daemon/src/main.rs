mod server;

use agent_shell_core::config::Config;

#[tokio::main]
async fn main() {
    let config = Config::load();

    // Ensure base directory exists
    let base_dir = Config::base_dir();
    let _ = std::fs::create_dir_all(&base_dir);

    // Write PID file
    let pid_path = base_dir.join("daemon.pid");
    let _ = std::fs::write(&pid_path, std::process::id().to_string());

    // Start the server
    let socket_path = config.socket_path();
    if let Err(e) = server::run(socket_path, config).await {
        eprintln!("daemon error: {}", e);
        std::process::exit(1);
    }
}
