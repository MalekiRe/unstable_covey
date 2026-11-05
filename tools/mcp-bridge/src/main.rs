//! Bridges an MCP client's stdio to a static-ecs app serving devtools on a
//! unix socket (`devtools::mcp::Session::unix`). Configure the agent with
//! `static-ecs-mcp /path/to/app.sock`; the app may start later, the bridge
//! waits for its socket.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

fn main() {
    let path = std::env::args()
        .nth(1)
        .or_else(|| std::env::var("STATIC_ECS_MCP").ok())
        .unwrap_or_else(|| {
            eprintln!("usage: static-ecs-mcp <socket path>   (or set STATIC_ECS_MCP)");
            std::process::exit(2);
        });
    // The agent may launch before the app: wait for the socket, up to a minute.
    let deadline = Instant::now() + Duration::from_secs(60);
    let stream = loop {
        match UnixStream::connect(&path) {
            Ok(stream) => break stream,
            Err(error) if Instant::now() >= deadline => {
                eprintln!("static-ecs-mcp: cannot connect to {path}: {error}");
                std::process::exit(1);
            }
            Err(_) => std::thread::park_timeout(Duration::from_millis(200)),
        }
    };
    let mut to_app = stream.try_clone().expect("socket handle");
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if writeln!(to_app, "{line}").is_err() {
                break;
            }
        }
        let _ = to_app.shutdown(std::net::Shutdown::Write);
    });
    let mut stdout = std::io::stdout().lock();
    for line in BufReader::new(stream).lines() {
        let Ok(line) = line else { break };
        if writeln!(stdout, "{line}")
            .and_then(|_| stdout.flush())
            .is_err()
        {
            break;
        }
    }
}
