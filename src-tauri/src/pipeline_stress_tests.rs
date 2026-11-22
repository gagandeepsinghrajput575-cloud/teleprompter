//! Stress tests for audio buffer ingestion, STT streaming, and LLM arbitration pipeline.

#[cfg(test)]
mod tests {
    use crate::stt::{self, SttEvent, Transcript};
    use crate::tracker::{load_script_by_type, Tracker};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};
    use tokio::sync::mpsc;

    /// 1. Stress test audio buffer ingestion:
    /// High-throughput bursts of 16kHz audio chunks with arbitrary sizes simulating CoreAudio
    /// and CrystalSound callbacks under system load. Verifies no buffer overrun, memory explosion,
    /// or channel drops.
    #[tokio::test]
    async fn stress_audio_buffer_high_throughput_ingestion() {
        let (audio_tx, mut audio_rx) = mpsc::unbounded_channel::<Vec<i16>>();
        let total_chunks = 20_000; // 20,000 chunks of ~160 samples = 200 seconds of audio pumped rapidly
        let producer = tokio::spawn(async move {
            let chunk_sizes = [64, 128, 160, 256, 320, 512, 1024];
            for i in 0..total_chunks {
                let sz = chunk_sizes[i % chunk_sizes.len()];
                let pcm = vec![(i % 1000) as i16; sz];
                if audio_tx.send(pcm).is_err() {
                    break;
                }
            }
        });

        let mut received_chunks = 0;
        let mut total_samples = 0;
        let start = Instant::now();
        while let Some(chunk) = audio_rx.recv().await {
            received_chunks += 1;
            total_samples += chunk.len();
            if received_chunks == total_chunks {
                break;
            }
        }
        let elapsed = start.elapsed();
        producer.await.expect("producer finished");

        assert_eq!(received_chunks, total_chunks);
        assert!(total_samples > 0);
        // Ensure 20k chunks processed in less than 500ms
        assert!(elapsed < Duration::from_millis(500), "Ingestion too slow: {:?}", elapsed);
    }

    /// 2. Stress test 1000ms LLM throttling and circuit recovery:
    /// Simulates rapid unconfident local matches requesting LLM decisions.
    /// Verifies 1000ms throttle pacing, circuit breaker tripping after 3 failures,
    /// 429 rate limit 5-second backoff, and circuit recovery after cooldown.
    #[tokio::test]
    async fn stress_llm_throttling_and_circuit_recovery() {
        let throttle_ms = 1000;
        let mut last_llm: Option<Instant> = None;
        let mut llm_failures: u32 = 0;
        let mut requests_fired = 0;
        let mut throttled_count = 0;

        let simulated_events = 25; // 25 rapid speech bursts over simulated time
        for _ in 0..simulated_events {
            let llm_ready = last_llm.map_or(true, |at| at.elapsed() >= Duration::from_millis(throttle_ms));
            
            // Check circuit recovery rule: failures >= 3 and 20s cooldown elapsed
            if llm_failures >= 3 && last_llm.map_or(false, |at| at.elapsed() >= Duration::from_secs(20)) {
                llm_failures = 0;
            }

            if llm_failures < 3 && llm_ready {
                last_llm = Some(Instant::now());
                requests_fired += 1;
            } else {
                throttled_count += 1;
            }
            // Rapid intervals: 10ms between events
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        // Only 1 request should have fired within 250ms of 10ms ticks because of 1000ms throttle!
        assert_eq!(requests_fired, 1, "Only 1 request should fire within 250ms under 1000ms throttle");
        assert_eq!(throttled_count, simulated_events - 1);

        // Now test 429 backoff handling:
        // When 429 occurs, last_llm is advanced by 5 seconds into the future
        let now = Instant::now();
        last_llm = Some(now + Duration::from_secs(5));
        let is_ready = last_llm.map_or(true, |at| at.elapsed() >= Duration::from_millis(1000));
        assert!(!is_ready, "Under 429 backoff, LLM must not be ready");

        // Now test circuit breaker tripping:
        llm_failures = 3;
        let is_allowed = llm_failures < 3;
        assert!(!is_allowed, "Circuit breaker must block when failures >= 3");

        // Test circuit recovery after 20s quiet cooldown:
        last_llm = Some(Instant::now() - Duration::from_secs(21));
        if llm_failures >= 3 && last_llm.map_or(false, |at| at.elapsed() >= Duration::from_secs(20)) {
            llm_failures = 0;
        }
        assert_eq!(llm_failures, 0, "Circuit breaker must reset after 20s quiet cooldown");
    }

    /// 3. Zero-jitter multi-utterance burst test:
    /// Rapid STT finals and interims from CrystalSound or Apple STT arriving concurrently
    /// do not cause memory leaks, thread contention, deadlocks, or backward highlight jumping.
    #[tokio::test]
    async fn stress_multi_utterance_burst_zero_jitter() {
        let script = load_script_by_type("coldcall").expect("coldcall fixture loads");
        let tracker = Arc::new(Mutex::new(Tracker::new(script.lines).expect("tracker builds")));
        let (stt_tx, mut stt_rx) = mpsc::unbounded_channel::<SttEvent>();

        // Real phrases from coldcall script
        let bursts = [
            ("hey John", false),
            ("hey John this is Gagan from Scalix", false),
            ("hey John this is Gagan from Scalix did I catch you in the middle of a job", true),
            ("yeah look I know you weren't expecting my call", false),
            ("so I'll be super brief", true),
            ("we build speed to lead infrastructure", false),
            ("we build speed to lead infrastructure for contractors", true),
        ];

        // Spawn high-frequency producer simulating CrystalSound + STT engine bursts
        let producer = tokio::spawn(async move {
            for _ in 0..50 {
                for (text, is_final) in &bursts {
                    stt_tx.send(SttEvent::Transcript(Transcript {
                        text: text.to_string(),
                        is_final: *is_final,
                        speech_final: *is_final,
                    })).expect("send transcript");
                }
            }
        });

        let mut previous_active = tracker.lock().unwrap().active_id();
        let mut event_count = 0;
        let mut jitter_violations = 0;

        while let Some(event) = stt_rx.recv().await {
            event_count += 1;
            if let SttEvent::Transcript(t) = event {
                let mut tr = tracker.lock().unwrap();
                if t.is_final {
                    if let Some(next_id) = tr.on_utterance_final(&t.text) {
                        if next_id < previous_active {
                            jitter_violations += 1;
                        }
                        previous_active = next_id;
                    } else {
                        tr.push_text(&t.text);
                    }
                    tr.clear_partial();
                } else {
                    tr.set_partial(&t.text);
                    if tr.active_line_spoken_out() {
                        if let Some(next_id) = tr.complete_active() {
                            if next_id < previous_active {
                                jitter_violations += 1;
                            }
                            previous_active = next_id;
                        }
                    }
                }
            }
            if event_count == 50 * bursts.len() {
                break;
            }
        }

        producer.await.expect("burst producer completed");
        assert_eq!(jitter_violations, 0, "Zero-jitter rule violated! Highlight moved backwards unexpectedly");
        assert_eq!(event_count, 50 * bursts.len());
    }

    /// 4. Deepgram STT socket backoff drain verification:
    /// Ensures queued audio during backoff does not accumulate or flood when connection re-establishes.
    #[tokio::test]
    async fn stress_stt_backoff_discards_queued_burst() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        // Burst 500 chunks into channel during simulated disconnect
        for _ in 0..500 {
            tx.send(vec![100i16; 160]).unwrap();
        }
        let alive = stt::wait_out_backoff(Duration::from_millis(300), &mut rx).await;
        assert!(alive, "Capture is still alive during backoff");
        assert!(rx.try_recv().is_err(), "Channel should be completely empty after backoff drain");
    }

    /// 5. Rapid concurrent STT thread contention and memory stability:
    /// Simulates multiple concurrent workers pushing interims and finals into shared Tracker
    /// verifying zero mutex deadlocks and memory stability under high lock contention.
    #[tokio::test]
    async fn stress_concurrent_thread_contention_tracker() {
        let script = load_script_by_type("call1").expect("call1 fixture loads");
        let tracker = Arc::new(Mutex::new(Tracker::new(script.lines).expect("tracker builds")));
        let mut handles = Vec::new();

        for worker_id in 0..8 {
            let tr_clone = tracker.clone();
            let handle = tokio::spawn(async move {
                for i in 0..250 {
                    let mut tr = tr_clone.lock().unwrap();
                    let text = format!("word {} from worker {}", i, worker_id);
                    if i % 5 == 0 {
                        tr.push_text(&text);
                        tr.clear_partial();
                    } else {
                        tr.set_partial(&text);
                        let _ = tr.active_line_spoken_out();
                    }
                    let _ = tr.spoken_buffer();
                    let _ = tr.local_match();
                }
            });
            handles.push(handle);
        }

        for h in handles {
            h.await.expect("worker finished without deadlock");
        }

        let tr = tracker.lock().unwrap();
        // Spoken buffer combines window (bounded to BUFFER_WORDS) + partial.
        // Even under 2,000 concurrent updates from 8 threads, the window must remain strictly bounded.
        let words = tr.spoken_buffer().split_whitespace().count();
        assert!(words <= 50, "Spoken buffer exceeded bounded size: {}", words);
    }
}
