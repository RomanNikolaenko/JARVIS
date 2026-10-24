//! Live diagnostics with the real microphone. Ignored by default (CI has no
//! audio devices and the microphone must not open unasked); run with
//! `cargo test -p jarvis-voice --test live -- --ignored --nocapture`.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::print_stdout)]

use std::time::{Duration, Instant};

use jarvis_voice::audio::{Capture, CaptureEvent};
use jarvis_voice::vad::{
    EarshotScorer, FRAME, SegmentEvent, Segmenter, SegmenterConfig, SpeechScorer,
};

/// Listens for `JARVIS_LIVE_SECONDS` (default 6) and prints how the VAD
/// scores the room: the share of frames above each threshold and the
/// phrases the segmenter would cut.
#[test]
#[ignore = "opens the real microphone"]
fn microphone_and_vad_probe() {
    let seconds: u64 = std::env::var("JARVIS_LIVE_SECONDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(6);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::channel(1024);
    let capture = Capture::start(None, tx).expect("microphone");
    println!("microphone: {}", capture.device_name());

    let mut scorer = EarshotScorer::default();
    let mut segmenter = Segmenter::new(SegmenterConfig::default());
    let mut scores = Vec::new();
    let mut levels = Vec::new();
    let mut pending: Vec<f32> = Vec::new();
    let mut events = Vec::new();
    let started = Instant::now();
    runtime.block_on(async {
        while started.elapsed() < Duration::from_secs(seconds) {
            let Ok(Some(event)) = tokio::time::timeout(Duration::from_secs(1), rx.recv()).await
            else {
                continue;
            };
            match event {
                CaptureEvent::Audio(samples) => {
                    for event in segmenter.push(&samples) {
                        events.push(match event {
                            SegmentEvent::Started => {
                                format!("{:.2}s started", started.elapsed().as_secs_f32())
                            }
                            SegmentEvent::Finished(p) => format!(
                                "{:.2}s finished ({:.1} s)",
                                started.elapsed().as_secs_f32(),
                                p.len() as f32 / 16_000.0
                            ),
                        });
                    }
                    pending.extend_from_slice(&samples);
                    while pending.len() >= FRAME {
                        let frame: Vec<f32> = pending.drain(..FRAME).collect();
                        levels
                            .push((frame.iter().map(|s| s * s).sum::<f32>() / FRAME as f32).sqrt());
                        scores.push(scorer.score(&frame));
                    }
                }
                CaptureEvent::Failed(err) => println!("stream error: {err}"),
            }
        }
    });
    drop(capture);

    let share =
        |t: f32| scores.iter().filter(|s| **s >= t).count() as f32 / scores.len().max(1) as f32;
    let mut sorted = scores.clone();
    sorted.sort_by(f32::total_cmp);
    let pct = |p: f32| {
        sorted
            .get(((sorted.len() as f32 - 1.0) * p) as usize)
            .copied()
            .unwrap_or(0.0)
    };
    println!("frames: {}", scores.len());
    println!(
        "score p50 {:.2} p90 {:.2} p99 {:.2} max {:.2}",
        pct(0.5),
        pct(0.9),
        pct(0.99),
        pct(1.0)
    );
    println!(
        ">=0.5: {:.1}%  >=0.7: {:.1}%  >=0.9: {:.1}%",
        share(0.5) * 100.0,
        share(0.7) * 100.0,
        share(0.9) * 100.0
    );
    let max_level = levels.iter().copied().fold(0.0f32, f32::max);
    let mean_level = levels.iter().sum::<f32>() / levels.len().max(1) as f32;
    println!("level mean {mean_level:.4} max {max_level:.4}");
    // Longest run of voiced frames (0.5).
    let mut run = 0;
    let mut longest = 0;
    for s in &scores {
        run = if *s >= 0.5 { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    println!("longest voiced run: {longest} frames ({} ms)", longest * 16);
    for event in events {
        println!("segmenter: {event}");
    }
}
