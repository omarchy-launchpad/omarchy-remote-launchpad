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
        if let Some(action) = std::env::args().nth(1)
            && matches!(action.as_str(), "--check-target" | "--select-target")
        {
            if let Err(error) = check_target(&action) {
                eprintln!("{error}");
                std::process::exit(1);
            }
            return;
        }
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

#[cfg(target_os = "macos")]
fn check_target(action: &str) -> Result<(), String> {
    let host = match std::env::args().skip(2).collect::<Vec<_>>().as_slice() {
        [target] => config::Host::parse(target)?,
        [] if action == "--check-target" => config::Host::load()?,
        _ => return Err(format!("Usage: seams {action} DESTINATION")),
    };
    let task = remote::catalogue(&host)?;
    let start = std::time::Instant::now();
    let apps = loop {
        if let Some(result) = task.poll() {
            break result?;
        }
        if start.elapsed().as_secs() >= 30 {
            return Err("Timed out reading desktop apps.".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    };
    if apps.is_empty() {
        return Err("No desktop apps found. Log in to Omarchy and retry.".into());
    }
    if action == "--select-target" {
        host.save().map_err(|e| e.to_string())?;
    }
    println!("{} ready ({} apps).", host.name(), apps.len());
    Ok(())
}
