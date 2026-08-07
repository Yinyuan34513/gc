mod api;
mod cli;
mod config;

fn main() -> anyhow::Result<()> {
    cli::run()
}
