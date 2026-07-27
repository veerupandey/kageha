fn main() {
    if let Err(error) = kageha_session_client::cli::run(std::env::args().skip(1).collect()) {
        eprintln!("{}", kageha_session_client::cli::clean(&error.to_string()));
        std::process::exit(1)
    }
}
