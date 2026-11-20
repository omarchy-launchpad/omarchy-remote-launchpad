mod config;
mod instance;
#[cfg(target_os = "macos")]
mod launcher;
mod remote;
#[cfg(target_os = "macos")]
mod ui;
#[cfg(target_os = "macos")]
mod viewer;

fn main() {
    #[cfg(target_os = "macos")]
    {
        let _instance = match instance::acquire(&config::directory().join("instance.lock")) {
            Ok(Some(lock)) => lock,
            Ok(None) => {
                ui::activate_existing();
                eprintln!("Seams is already running. Showing its launchpad.");
                return;
            }
            Err(error) => {
                eprintln!("Cannot acquire the Seams instance lock: {error}");
                return;
            }
        };
        ui::run();
    }
    #[cfg(not(target_os = "macos"))]
    eprintln!("Seams requires macOS.");
}
