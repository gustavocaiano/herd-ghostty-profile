use std::env;
use std::process::ExitCode;

use herdr_desktop_switcher::launch::{
    LaunchOutcome, LaunchPlan, RuntimePaths, authority_name, client_desktop, launch_desktop,
    load_and_plan,
};
use herdr_desktop_switcher::pick;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("herdr-desktop-switcher: {error}");
            ExitCode::from(1)
        }
    }
}

fn run() -> Result<(), String> {
    let arguments: Vec<String> = env::args().skip(1).collect();
    let paths = RuntimePaths::from_env().map_err(|error| error.to_string())?;

    match arguments.as_slice() {
        [command] if command == "summon" => {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| format!("could not start API runtime: {error}"))?;
            runtime.block_on(pick::summon(&paths))?;
        }
        [command] if command == "picker" => pick::picker(&paths)?,
        [command, desktop_id] => match command.as_str() {
            "launch" => {
                match launch_desktop(&paths, desktop_id).map_err(|error| error.to_string())? {
                    LaunchOutcome::Launched { pid } => {
                        println!("launched {desktop_id} (pid {pid})")
                    }
                    LaunchOutcome::Focused { pid } => {
                        println!("focused {desktop_id} (pid {pid})")
                    }
                }
            }
            "client" => client_desktop(&paths, desktop_id).map_err(|error| error.to_string())?,
            "plan" => {
                match load_and_plan(&paths, desktop_id).map_err(|error| error.to_string())? {
                    LaunchPlan::Local => println!("local"),
                    LaunchPlan::Remote {
                        target,
                        session,
                        keybindings,
                    } => println!(
                        "remote\ttarget={target}\tsession={session}\tkeybindings={}",
                        authority_name(keybindings)
                    ),
                }
            }
            _ => {
                return Err(format!(
                    "unknown command {command:?}; expected launch, client, plan, summon, or picker"
                ));
            }
        },
        _ => {
            return Err(
                "usage: herdr-desktop-switcher <launch|client|plan> <desktop-id> | summon | picker"
                    .into(),
            );
        }
    }
    Ok(())
}
