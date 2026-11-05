//! A tokio runtime fetches web pages while the app keeps running frames.
//! A system starts a request for each new `Fetch` entity, handing tokio the
//! world's `Remote`; responses come back as components through the `Net`
//! sync point.
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
/// Where responses enter the frame.
struct Net;
impl SyncPoint for Net {}

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
    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    let tokio = runtime.handle().clone();
    let mut app = App::new()
        // Each new `Fetch` starts a request on tokio. Any system can take the
        // world's `Remote` and hand it to async code.
        .add_systems(
            Update,
            move |remote: &Remote<_>, mut added: Query<(Entity, &Fetch), Added<Fetch>>| {
                for (entity, fetch) in &mut added {
                    let (remote, host) = (remote.clone(), fetch.0);
                    tokio.spawn(async move {
                        let start = Instant::now();
                        let (status, bytes) =
                            match timeout(Duration::from_secs(10), get(host)).await {
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
                            .bridge(Net, move |mut commands: Commands<_>| {
                                commands.insert(entity, (page,));
                            })
                            .await
                            .expect("the app is running");
                    });
                }
            },
        )
        // Where in the frame responses arrive.
        .add_systems(Update, sync_point(Net))
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
    let hosts = ["example.com", "example.org", "example.net"];
    for host in hosts {
        app.world.spawn_bundle((Fetch(host),));
    }

    // The app's own loop: frames keep running while requests are in flight.
    let mut frames = 0;
    while app.world.query::<&Page>().iter_mut().count() < hosts.len() {
        app.emit(&Update);
        frames += 1;
        std::thread::sleep(Duration::from_millis(16));
    }
    println!("{} pages over {frames} frames", hosts.len());
}
