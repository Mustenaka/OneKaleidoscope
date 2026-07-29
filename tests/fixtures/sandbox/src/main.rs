use std::env;
use std::process::{self, ExitCode};
use std::thread;
use std::time::Duration;

fn main() -> ExitCode {
    match env::args().nth(1).as_deref() {
        Some("fail") => {
            eprintln!("intentional sandbox failure");
            ExitCode::from(7)
        }
        Some("wait") => {
            thread::sleep(Duration::from_secs(30));
            ExitCode::SUCCESS
        }
        Some(argument) => {
            eprintln!("unknown sandbox argument: {argument}");
            ExitCode::from(2)
        }
        None => {
            println!("fixture sandbox");
            ExitCode::SUCCESS
        }
    }
}
