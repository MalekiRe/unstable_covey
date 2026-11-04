//! A tokio runtime fetches web pages while the app keeps running frames.
//! Requests read what to fetch from the world, and responses come back as
//! components, all through a remote sync point.
//!
//! ```sh
//! cargo run --release --example tokio_web
//! ```
use static_ecs::prelude::*;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;

struct Update;
impl Schedule for Update {}

/// A host whose front page should be fetched.
struct Fetch(&'static str);
/// The response, inserted when it arrives.
struct Page {
    status: String,
    bytes: usize,
    elapsed: Duration,
}

/// A plain HTTP/1.1 GET: the status line and the response size.
async fn get(host: &str) -> std::io::Result<(String, usize)> {
    let mut stream = TcpStream::connect((host, 80)).await?;
    let request = format!("GET / HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).await?;
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await?;
    let status = String::from_utf8_lossy(&response)
        .lines()
        .next()
        .unwrap_or_default()
        .to_owned();
    Ok((status, response.len()))
}

fn main() {
    let (point, remote) = sync_point();
    let mut app = App::new()
        // Where in the frame remote bridges run.
        .add_systems(Update, point)
        // Runs after the sync point, so it sees pages the frame they arrive.
        .add_systems(
            Update,
            |mut arrived: Query<(&Fetch, &Page), Added<Page>>| {
                for (fetch, page) in &mut arrived {
                    println!(
                        "{:<12} {} ({} bytes in {:?})",
                        fetch.0, page.status, page.bytes, page.elapsed
                    );
                }
            },
        )
        .build();
    for host in ["example.com", "example.org", "example.net"] {
        app.world.spawn_bundle((Fetch(host),));
    }

    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    let fetching = runtime.spawn(async move {
        // Read the work list from the world. The closure borrows this task's
        // local mutably; only the request crosses into the app's frame.
        let mut hosts = Vec::new();
        remote
            .bridge(|mut fetches: Query<(Entity, &Fetch)>| {
                hosts.extend(fetches.iter_mut().map(|(entity, fetch)| (entity, fetch.0)));
            })
            .await
            .expect("the app is running");

        // Fetch concurrently on tokio; each response bridges back on its own.
        let mut requests = tokio::task::JoinSet::new();
        for (entity, host) in hosts {
            let remote = remote.clone();
            requests.spawn(async move {
                let start = Instant::now();
                let (status, bytes) = match timeout(Duration::from_secs(10), get(host)).await {
                    Ok(Ok(response)) => response,
                    Ok(Err(error)) => (format!("error: {error}"), 0),
                    Err(_) => ("error: timed out".to_owned(), 0),
                };
                let page = Page {
                    status,
                    bytes,
                    elapsed: start.elapsed(),
                };
                remote
                    .bridge(move |mut commands: Commands<_>| {
                        commands.insert(entity, (page,));
                    })
                    .await
                    .expect("the app is running");
            });
        }
        requests.join_all().await;
    });

    // The app's own loop: frames keep running while requests are in flight.
    let mut frames = 0;
    while !fetching.is_finished() {
        app.emit(&Update);
        frames += 1;
        std::thread::sleep(Duration::from_millis(16));
    }
    runtime.block_on(fetching).expect("fetching task");
    let fetched = app.world.query::<&Page>().iter_mut().count();
    println!("{fetched} pages over {frames} frames");
}
