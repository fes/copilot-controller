use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    match (args.next().as_deref(), args.next()) {
        (None | Some("-h" | "--help"), None) => {
            println!(
                "copilot-controller {}\n\
                 Offline foundation scaffold. No device discovery or action delivery.\n\
                 \n\
                 Usage: copilot-controller [--help | --version]",
                env!("CARGO_PKG_VERSION")
            );
            ExitCode::SUCCESS
        }
        (Some("-V" | "--version"), None) => {
            println!("copilot-controller {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!("unsupported command; this foundation has no action-delivery interface");
            ExitCode::FAILURE
        }
    }
}
