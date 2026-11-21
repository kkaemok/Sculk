fn main() -> std::process::ExitCode {
    sculk_auth_client::run_cli(&std::env::args().skip(1).collect::<Vec<_>>())
}
