mod config;
#[cfg(target_os = "macos")]
mod dock_apps;
mod instance;
#[cfg(target_os = "macos")]
mod launcher;
#[cfg(target_os = "macos")]
mod machine_store;
#[cfg(target_os = "macos")]
mod machines;
mod remote;
#[cfg(target_os = "macos")]
mod ui;
#[cfg(target_os = "macos")]
mod unlock;
#[cfg(target_os = "macos")]
mod viewer;
#[cfg(target_os = "macos")]
mod viewer_app;
#[cfg(target_os = "macos")]
mod vm_run;

fn main() {
    #[cfg(target_os = "macos")]
    {
        if let Some(action) = std::env::args().nth(1)
            && matches!(
                action.as_str(),
                "--check-target" | "--select-target" | "--install-agent"
            )
        {
            if let Err(error) = check_target(&action) {
                eprintln!("{error}");
                std::process::exit(1);
            }
            return;
        }
        if let Some(entry) = dock_apps::entry() {
            viewer_app::run(entry);
            return;
        }
        let _instance = match instance::acquire(&config::directory().join("instance.lock")) {
            Ok(Some(lock)) => lock,
            Ok(None) => {
                ui::activate_existing();
                eprintln!("omarchy-launchpad is already running.");
                return;
            }
            Err(error) => {
                eprintln!("Cannot acquire the omarchy-launchpad instance lock: {error}");
                return;
            }
        };
        ui::run();
    }
    #[cfg(not(target_os = "macos"))]
    eprintln!("omarchy-launchpad requires macOS.");
}

#[cfg(target_os = "macos")]
fn check_target(action: &str) -> Result<(), String> {
    let host = match std::env::args().skip(2).collect::<Vec<_>>().as_slice() {
        [target] => config::Host::parse(target)?,
        [] if action == "--check-target" => config::Host::load()?,
        _ => return Err(format!("Usage: omarchy-launchpad {action} DESTINATION")),
    };
    if action == "--install-agent" {
        let task = remote::install_agent(&host)?;
        let start = std::time::Instant::now();
        loop {
            if let Some(result) = task.poll() {
                return result;
            }
            if start.elapsed().as_secs() >= 600 {
                return Err("Timed out installing the bridge agent.".into());
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
    }
    let check = machines::CatalogueCheck::start(&host)?;
    let count = loop {
        if let Some(result) = check.poll() {
            break result?;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    };
    if action == "--select-target" {
        host.save().map_err(|e| e.to_string())?;
    }
    println!("{} ready ({count} apps).", host.name());
    Ok(())
}
