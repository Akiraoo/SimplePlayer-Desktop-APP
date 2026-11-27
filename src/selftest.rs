//! `SimplePlayer --audio-selftest [device]`: plays generated test tones through the real
//! player (decoder, resampler, buffer, output) in exclusive mode at several sample rates,
//! idle and with every CPU core busy, and prints what the output really did. Made for the
//! macOS CI machines (a virtual BlackHole device), where nobody can listen.
//!
//! Exit code 1 when a test failed: dropouts, or (macOS) the device not running at the song's
//! own rate. How fast the device takes samples is only reported (see `speed`).

use std::io::Cursor;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use crate::player::{Item, Player};

const CASES: [(u32, u16); 4] = [(44_100, 16), (48_000, 24), (96_000, 24), (192_000, 24)];
const TONE_SECS: u32 = 9;
const MEASURE: Duration = Duration::from_secs(5);

/// 16/24-bit stereo WAV: 1 kHz on the left, 440 Hz on the right, -6 dBFS.
fn wav(rate: u32, bits: u16) -> Vec<u8> {
    let bytes = (bits / 8) as u32;
    let frames = rate * TONE_SECS;
    let data_len = frames * 2 * bytes;
    let mut w = Vec::with_capacity(44 + data_len as usize);
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + data_len).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&16u32.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes()); // PCM
    w.extend_from_slice(&2u16.to_le_bytes());
    w.extend_from_slice(&rate.to_le_bytes());
    w.extend_from_slice(&(rate * 2 * bytes).to_le_bytes());
    w.extend_from_slice(&((2 * bytes) as u16).to_le_bytes());
    w.extend_from_slice(&bits.to_le_bytes());
    w.extend_from_slice(b"data");
    w.extend_from_slice(&data_len.to_le_bytes());
    let full = ((1i64 << (bits - 1)) - 1) as f64;
    for i in 0..frames {
        let t = i as f64 / rate as f64;
        for f in [1000.0, 440.0] {
            let v = ((2.0 * std::f64::consts::PI * f * t).sin() * 0.5 * full) as i32;
            w.extend_from_slice(&v.to_le_bytes()[..bytes as usize]);
        }
    }
    w
}

fn item(data: Vec<u8>) -> Item {
    Item {
        open: Box::new(move || Ok(Box::new(Cursor::new(data)) as Box<dyn symphonia::core::io::MediaSource>)),
        ext: Some("wav".into()),
        alt: None,
        duration_hint: TONE_SECS as f64,
    }
}

/// Keeps every core busy (ordinary threads) until `stop` is set.
fn load(stop: Arc<AtomicBool>) -> Vec<thread::JoinHandle<()>> {
    let n = thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    (0..n)
        .map(|_| {
            let stop = stop.clone();
            thread::spawn(move || {
                let mut x = 1.0f64;
                while !stop.load(Ordering::Relaxed) {
                    for _ in 0..10_000 {
                        x = (x * 1.000_001).sin() + 1.0;
                    }
                }
                std::hint::black_box(x);
            })
        })
        .collect()
}

/// (rate the device runs at, its I/O buffer in frames), where the platform can tell.
#[allow(unused_variables)]
fn device_info(name: Option<&str>) -> (Option<f64>, Option<u32>) {
    #[cfg(target_os = "macos")]
    {
        if let Some(id) = crate::hog::find(name).or_else(|| crate::hog::find(None)) {
            return (crate::hog::nominal_rate(id), crate::hog::buffer_frames(id));
        }
    }
    (None, None)
}

pub fn run(device: Option<String>) -> i32 {
    println!("audio selftest — device: {}", device.as_deref().unwrap_or("(system default)"));
    println!("cores: {}", thread::available_parallelism().map(|n| n.get()).unwrap_or(0));
    let player = Player::new(|_| {}, device.clone(), true);
    player.set_volume(0.5);
    let mut rows = Vec::new();
    let mut failed = false;
    for (rate, bits) in CASES {
        let tone = wav(rate, bits);
        for busy in [false, true] {
            player.open(item(tone.clone()), false);
            let started = Instant::now();
            while player.position() < 0.3 && started.elapsed() < Duration::from_secs(6) {
                thread::sleep(Duration::from_millis(20));
            }
            if player.position() < 0.3 {
                println!("{rate} Hz {bits}-bit: did not start playing");
                rows.push(format!("| {rate} / {bits} | {} | did not start | | | | | | FAIL |", if busy { "busy" } else { "idle" }));
                failed = true;
                continue;
            }
            let stop = Arc::new(AtomicBool::new(false));
            let workers = if busy { load(stop.clone()) } else { Vec::new() };
            player.stats(true);
            let from = player.stats(false).played;
            let t0 = Instant::now();
            thread::sleep(MEASURE);
            let s = player.stats(false);
            let secs = t0.elapsed().as_secs_f64();
            stop.store(true, Ordering::Relaxed);
            for w in workers {
                let _ = w.join();
            }
            let (dev_rate, dev_frames) = device_info(device.as_deref());
            let taken = s.played.saturating_sub(from) as f64 / secs;
            let budget_ms = s.cb_frames as f64 * 1000.0 / s.out_rate.max(1) as f64;
            let speed_ok = (taken / s.out_rate.max(1) as f64 - 1.0).abs() < 0.01;
            let native_ok = dev_rate.map_or(true, |d| (d - rate as f64).abs() < 1.0);
            let mut why = Vec::new();
            if s.underruns > 0 {
                why.push(format!("{} dropouts", s.underruns));
            }
            // only reported: a virtual device in a VM (BlackHole on CI) does not run at real
            // speed, so frames/s says nothing there
            let speed = if speed_ok { "" } else { " (speed off)" };
            if !native_ok {
                why.push(format!("device ran at {:.0} Hz, not the song's {rate} Hz", dev_rate.unwrap_or(0.0)));
            }
            let verdict = if why.is_empty() { "ok".to_string() } else { failed = true; format!("FAIL: {}", why.join("; ")) };
            println!(
                "{rate} Hz {bits}-bit {}: device {} Hz (buffer {} frames), stream {} Hz [{}], {taken:.0} frames/s{speed}, \
                 {} dropouts, {} late, {} callbacks × {} frames, longest callback {:.3} ms of {budget_ms:.2} ms, \
                 longest gap {:.2} ms — {verdict}",
                if busy { "busy" } else { "idle" },
                dev_rate.map_or("?".into(), |d| format!("{d:.0}")),
                dev_frames.map_or("?".into(), |f| f.to_string()),
                s.out_rate,
                s.desc,
                s.underruns,
                s.late,
                s.callbacks,
                s.cb_frames,
                s.cb_max_ms,
                s.cb_gap_max_ms,
            );
            rows.push(format!(
                "| {rate} / {bits} | {} | {} | {} | {taken:.0}{speed} | {} | {:.3} / {budget_ms:.2} | {:.2} | {verdict} |",
                if busy { "busy" } else { "idle" },
                dev_rate.map_or("?".into(), |d| format!("{d:.0}")),
                s.out_rate,
                s.underruns,
                s.cb_max_ms,
                s.cb_gap_max_ms,
            ));
        }
    }
    player.stop();
    println!();
    println!("| song Hz / bits | CPU | device Hz | stream Hz | frames/s | dropouts | longest callback / budget ms | longest gap ms | result |");
    println!("|---|---|---|---|---|---|---|---|---|");
    for r in rows {
        println!("{r}");
    }
    i32::from(failed)
}
