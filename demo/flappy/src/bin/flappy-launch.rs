//! Launches `flappy` repeatedly and reports, per launch step, how long after
//! the spawn it finished: process start, Wayland connection, first frame
//! drawn, window configured, first frame committed, and first frame shown
//! (the compositor's presentation timestamp). `flappy-launch [runs] [binary]`.
use std::process::Command;

fn now_ns() -> u64 {
    let t = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
    t.tv_sec as u64 * 1_000_000_000 + t.tv_nsec as u64
}

const STEPS: [&str; 9] = [
    "exec",
    "connect",
    "built",
    "drawn",
    "configure",
    "commit",
    "presented",
    "refresh",
    // Not after spawn: a later frame's commit-to-scanout latency.
    "steady",
];

fn main() {
    let mut args = std::env::args().skip(1);
    let runs: usize = args.next().map_or(20, |n| n.parse().expect("runs"));
    let binary = args.next().map_or_else(
        || {
            let me = std::env::current_exe().expect("current exe");
            me.with_file_name("flappy")
        },
        Into::into,
    );
    let mut samples: Vec<Vec<f64>> = vec![Vec::new(); STEPS.len()];
    for run in 0..runs {
        let launched = now_ns();
        let output = Command::new(&binary)
            .env("FLAPPY_T0_NS", launched.to_string())
            .env("FLAPPY_BENCH", "1")
            .output()
            .expect("launch flappy");
        let line = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "run {run}: {line}{}",
            String::from_utf8_lossy(&output.stderr)
        );
        for field in line.split_whitespace() {
            let (key, value) = field.split_once('=').expect("key=value");
            let step = STEPS.iter().position(|&s| s == key).expect("known step");
            let value: f64 = value.parse().expect("number");
            if value.is_finite() {
                samples[step].push(value);
            }
        }
        // Let the compositor finish unmapping before the next window opens.
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    println!("{} launches of {}: ms after spawn", runs, binary.display());
    println!(
        "{:>10} {:>8} {:>8} {:>8} {:>8}",
        "step", "min", "median", "p90", "max"
    );
    for (step, values) in STEPS.iter().zip(&mut samples) {
        if values.is_empty() {
            println!("{step:>10} (not reported)");
            continue;
        }
        values.sort_by(f64::total_cmp);
        let at = |q: f64| values[((values.len() - 1) as f64 * q).round() as usize];
        println!(
            "{step:>10} {:>8.3} {:>8.3} {:>8.3} {:>8.3}",
            values[0],
            at(0.5),
            at(0.9),
            values[values.len() - 1]
        );
    }
}
