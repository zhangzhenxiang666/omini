/// 独立 daemon 进程入口。
fn main() {
    let options = match omini_server::process::ProcessOptions::parse_from_env() {
        Ok(options) => options,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };

    if let Err(error) = omini_server::process::run_daemon_process(options) {
        eprintln!("Error: {error}");
        let mut source = error.source();
        while let Some(error) = source {
            eprintln!("  cause: {error}");
            source = error.source();
        }
        std::process::exit(1);
    }
}
