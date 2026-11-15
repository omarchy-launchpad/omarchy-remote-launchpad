mod instance;
mod model;
mod session;
mod store;
#[cfg(target_os = "macos")]
mod ui;

fn main() {
    #[cfg(target_os = "macos")]
    {
        let _instance = match instance::acquire(&store::directory().join("instance.lock")) {
            Ok(Some(lock)) => lock,
            Ok(None) => {
                ui::activate_existing();
                eprintln!("Seams is already running. Showing its connection manager.");
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
