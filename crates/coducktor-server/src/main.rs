use std::env;
use std::net::SocketAddr;
use std::path::PathBuf;

use coducktor_server::{ServerConfig, serve};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut bind = "127.0.0.1:4321".parse::<SocketAddr>()?;
    let mut repo_root = env::current_dir()?;
    let mut version = "0.1.0-dev".to_owned();
    let mut args = env::args().skip(1);
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--bind" => bind = args.next().ok_or("--bind needs an address")?.parse()?,
            "--repo-root" => {
                repo_root = PathBuf::from(args.next().ok_or("--repo-root needs a path")?)
            }
            "--version" => version = args.next().ok_or("--version needs a value")?,
            "-h" | "--help" => {
                println!(
                    "coducktor-server [--bind HOST:PORT] [--repo-root DIR] [--version VERSION]"
                );
                return Ok(());
            }
            unknown => return Err(format!("unknown argument: {unknown}").into()),
        }
    }
    let listener = tokio::net::TcpListener::bind(bind).await?;
    serve(listener, ServerConfig::new(repo_root, version)).await?;
    Ok(())
}
