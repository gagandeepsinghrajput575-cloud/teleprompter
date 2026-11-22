//! Deepgram Nova-3 streaming transcription.
//!
//! Two things here are load-bearing and easy to get wrong:
//!  * Deepgram closes an idle socket after 10s with `NET-0001`, and a sales call is full
//!    of long silences, so `KeepAlive` must be sent every 4s.
//!  * The socket dies when the laptop sleeps or wifi drops, so reconnection is not an
//!    edge case, it is the normal path.

use futures_util::{Sink, SinkExt, StreamExt};
use serde::Deserialize;
use std::collections::VecDeque;
use std::future::Future;
use std::time::Duration;
use tokio::sync::mpsc::error::TryRecvError;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;

/// Must stay well under Deepgram's 10s idle timeout.
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(4);
const MAX_BACKOFF_SECS: u64 = 10;
/// A connect that has not completed in this long is not going to: DNS is wedged or the network
/// is gone. Without a deadline the socket sits in the OS retry for over a minute, with the HUD
/// showing "listening" and nothing arriving.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Maximum time a single WebSocket write (audio, KeepAlive, Ping/Pong) may block before we
/// declare the socket wedged and trigger a clean reconnect.
const SEND_TIMEOUT: Duration = Duration::from_secs(3);
/// Maximum consecutive 4s KeepAlive/Ping intervals with zero incoming server frames (Results,
/// Metadata, Pong, etc.) before treating a silent half-open TCP socket as dead (3 * 4s = 12s).
const MAX_UNACKED_PINGS: u32 = 3;
/// A session that lived this long was healthy, so a later drop should reconnect in 1s rather
/// than inheriting the backoff that a failed connect earned.
const HEALTHY_SESSION: Duration = Duration::from_secs(5);
/// Maximum buffered live audio sent in one batch during transient socket jitter:
/// 400ms at 16 kHz mono = 6,400 samples. Anything older is discarded so Deepgram always
/// transcribes real-time speech rather than replaying a delayed burst.
const MAX_LIVE_QUEUE_SAMPLES: usize = 6_400;

#[derive(Clone, Debug, serde::Serialize)]
pub struct Transcript {
    pub text: String,
    pub is_final: bool,
    pub speech_final: bool,
}

#[derive(Debug)]
pub enum SttEvent {
    Connected,
    Transcript(Transcript),
    Error(String),
}

fn listen_url() -> String {
    // Nova-3 streaming transcription.
    // Note: Deepgram streaming returns HTTP 400 if utterance_end_ms < 1000 or if invalid query
    // parameters like no_delay are provided. endpointing=100 provides crisp endpointing.
    let params = [
        ("model", "nova-3"),
        ("language", "en-US"),
        ("encoding", "linear16"),
        ("sample_rate", "16000"),
        ("channels", "1"),
        ("interim_results", "true"),
        ("punctuate", "true"),
        ("smart_format", "true"),
        ("endpointing", "100"),
        ("utterance_end_ms", "1000"),
        ("vad_events", "true"),
    ];
    let query = params
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&");
    format!("wss://api.deepgram.com/v1/listen?{query}")
}

#[derive(Deserialize)]
struct DgMessage {
    #[serde(rename = "type", default)]
    kind: Option<String>,
    #[serde(default)]
    is_final: Option<bool>,
    #[serde(default)]
    speech_final: Option<bool>,
    #[serde(default)]
    channel: Option<DgChannel>,
}

#[derive(Deserialize)]
struct DgChannel {
    #[serde(default)]
    alternatives: Vec<DgAlternative>,
}

#[derive(Deserialize)]
struct DgAlternative {
    #[serde(default)]
    transcript: Option<String>,
}

/// Extracts a spoken fragment, ignoring control messages such as Metadata/UtteranceEnd.
fn parse(raw: &str) -> Option<Transcript> {
    let message: DgMessage = serde_json::from_str(raw).ok()?;
    if let Some(kind) = message.kind.as_deref() {
        if kind != "Results" {
            return None;
        }
    }
    let text = message
        .channel?
        .alternatives
        .first()?
        .transcript
        .clone()?
        .trim()
        .to_string();
    if text.is_empty() {
        return None;
    }
    Some(Transcript {
        text,
        is_final: message.is_final.unwrap_or(false),
        speech_final: message.speech_final.unwrap_or(false),
    })
}

/// Non-blocking drain of every PCM chunk currently queued in `audio_rx`.
/// Returns `(dropped_samples, channel_closed)`.
pub(crate) fn drain_all_pending(audio_rx: &mut UnboundedReceiver<Vec<i16>>) -> (usize, bool) {
    let mut dropped = 0usize;
    loop {
        match audio_rx.try_recv() {
            Ok(pcm) => dropped += pcm.len(),
            Err(TryRecvError::Empty) => return (dropped, false),
            Err(TryRecvError::Disconnected) => return (dropped, true),
        }
    }
}

/// Coalesces `first_pcm` with any additional chunks already queued in `audio_rx`, clamping the
/// total sample count to at most `max_samples` by discarding the oldest leading samples.
///
/// - Healthy path (0 extra chunks queued, `first_pcm.len() <= max_samples`): zero drops, returns
///   `first_pcm` immediately.
/// - Network jitter / reconnect burst path: drops stale backlog in O(1) bounded memory and
///   returns only the freshest `max_samples` tail so the tracker stays locked to live speech.
///
/// Returns `(fresh_pcm, dropped_samples, channel_closed)`.
pub(crate) fn clamp_live_batch(
    first_pcm: Vec<i16>,
    audio_rx: &mut UnboundedReceiver<Vec<i16>>,
    max_samples: usize,
) -> (Vec<i16>, usize, bool) {
    let mut queue = VecDeque::new();
    let mut total_samples = first_pcm.len();
    let mut dropped_samples = 0usize;
    let mut disconnected = false;

    queue.push_back(first_pcm);

    loop {
        match audio_rx.try_recv() {
            Ok(pcm) => {
                total_samples += pcm.len();
                queue.push_back(pcm);
                // Evict full oldest chunks immediately so memory stays O(max_samples)
                // even if thousands of chunks backed up during a stall.
                while queue.len() > 1 {
                    let front_len = queue.front().map_or(0, Vec::len);
                    if total_samples.saturating_sub(front_len) >= max_samples {
                        if let Some(evicted) = queue.pop_front() {
                            total_samples -= evicted.len();
                            dropped_samples += evicted.len();
                        }
                    } else {
                        break;
                    }
                }
            }
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Disconnected) => {
                disconnected = true;
                break;
            }
        }
    }

    // Fast path: single chunk already within the live budget -> zero extra allocation/copy.
    if queue.len() == 1 && total_samples <= max_samples {
        let single = queue.pop_front().unwrap_or_default();
        return (single, dropped_samples, disconnected);
    }

    let excess = total_samples.saturating_sub(max_samples);
    dropped_samples += excess;

    let mut out = Vec::with_capacity(total_samples.min(max_samples));
    let mut skip = excess;
    for chunk in queue {
        if skip >= chunk.len() {
            skip -= chunk.len();
            continue;
        }
        out.extend_from_slice(&chunk[skip..]);
        skip = 0;
    }

    (out, dropped_samples, disconnected)
}

#[derive(Debug)]
pub(crate) enum ConnectOutcome<T, E> {
    Connected(T, usize),
    CaptureClosed,
    TimedOut(usize),
    Failed(E, usize),
}

/// Drives `connect_fut` while continuously draining `audio_rx` so zero stale PCM accumulates
/// during DNS/TCP/TLS/WebSocket handshakes, and aborts immediately if capture stops mid-connect.
pub(crate) async fn connect_while_draining<F, T, E>(
    connect_fut: F,
    audio_rx: &mut UnboundedReceiver<Vec<i16>>,
    timeout: Duration,
) -> ConnectOutcome<T, E>
where
    F: Future<Output = Result<T, E>>,
{
    let (mut dropped_samples, pre_closed) = drain_all_pending(audio_rx);
    if pre_closed {
        return ConnectOutcome::CaptureClosed;
    }

    tokio::pin!(connect_fut);
    let deadline = tokio::time::sleep(timeout);
    tokio::pin!(deadline);

    loop {
        // Biased order ensures handshake completion and timeout are never starved by a
        // high-frequency audio producer, while still draining every chunk while connect is pending.
        tokio::select! {
            biased;
            res = &mut connect_fut => {
                let (post_dropped, post_closed) = drain_all_pending(audio_rx);
                dropped_samples += post_dropped;
                if post_closed {
                    return ConnectOutcome::CaptureClosed;
                }
                return match res {
                    Ok(val) => ConnectOutcome::Connected(val, dropped_samples),
                    Err(err) => ConnectOutcome::Failed(err, dropped_samples),
                };
            }
            _ = &mut deadline => {
                let (post_dropped, post_closed) = drain_all_pending(audio_rx);
                dropped_samples += post_dropped;
                if post_closed {
                    return ConnectOutcome::CaptureClosed;
                }
                return ConnectOutcome::TimedOut(dropped_samples);
            }
            chunk = audio_rx.recv() => {
                match chunk {
                    Some(pcm) => dropped_samples += pcm.len(),
                    None => return ConnectOutcome::CaptureClosed,
                }
            }
        }
    }
}

async fn send_with_timeout<S>(socket: &mut S, msg: Message, label: &str) -> Result<(), String>
where
    S: Sink<Message, Error = tokio_tungstenite::tungstenite::Error> + Unpin,
{
    tokio::time::timeout(SEND_TIMEOUT, socket.send(msg))
        .await
        .map_err(|_| format!("{label} timed out after {}s", SEND_TIMEOUT.as_secs()))?
        .map_err(|e| format!("{label} failed: {e}"))
}

async fn run_session(
    api_key: &str,
    audio_rx: &mut UnboundedReceiver<Vec<i16>>,
    events: &UnboundedSender<SttEvent>,
) -> Result<(), String> {
    let mut request = listen_url()
        .into_client_request()
        .map_err(|e| format!("bad url: {e}"))?;
    request.headers_mut().insert(
        "Authorization",
        HeaderValue::from_str(&format!("Token {api_key}")).map_err(|e| format!("bad key: {e}"))?,
    );

    let (mut socket, dropped_during_connect) =
        match connect_while_draining(connect_async(request), audio_rx, CONNECT_TIMEOUT).await {
            ConnectOutcome::Connected((sock, _), dropped) => (sock, dropped),
            ConnectOutcome::CaptureClosed => {
                println!("STT_STOPPED capture closed during connect");
                return Ok(());
            }
            ConnectOutcome::TimedOut(dropped) => {
                return Err(format!(
                    "connect timed out after {}s (discarded {} stale samples)",
                    CONNECT_TIMEOUT.as_secs(),
                    dropped
                ));
            }
            ConnectOutcome::Failed(err, dropped) => {
                return Err(format!(
                    "connect failed: {err} (discarded {dropped} stale samples)"
                ));
            }
        };

    if dropped_during_connect > 0 {
        println!(
            "STT_CONNECT_FLUSHED dropped_samples={} dropped_ms={}",
            dropped_during_connect,
            dropped_during_connect * 1000 / 16_000
        );
    }

    println!("STT_CONNECTED model=nova-3 rate=16000");
    let _ = events.send(SttEvent::Connected);

    let mut keepalive = tokio::time::interval(KEEPALIVE_INTERVAL);
    keepalive.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    keepalive.tick().await; // the first tick fires immediately; ignore it
    let mut unacked_pings: u32 = 0;

    loop {
        tokio::select! {
            chunk = audio_rx.recv() => {
                match chunk {
                    Some(first_pcm) => {
                        let (pcm, dropped, disconnected) =
                            clamp_live_batch(first_pcm, audio_rx, MAX_LIVE_QUEUE_SAMPLES);
                        if dropped > 0 {
                            println!(
                                "STT_AUDIO_CLAMPED dropped_samples={} dropped_ms={} kept_samples={}",
                                dropped,
                                dropped * 1000 / 16_000,
                                pcm.len()
                            );
                        }
                        if !pcm.is_empty() {
                            let mut bytes = Vec::with_capacity(pcm.len() * 2);
                            for sample in pcm {
                                bytes.extend_from_slice(&sample.to_le_bytes());
                            }
                            send_with_timeout(&mut socket, Message::Binary(bytes.into()), "send").await?;
                        }
                        if disconnected {
                            let _ = send_with_timeout(
                                &mut socket,
                                Message::Text(r#"{"type":"CloseStream"}"#.into()),
                                "close_stream",
                            )
                            .await;
                            return Ok(());
                        }
                    }
                    None => {
                        let _ = send_with_timeout(
                            &mut socket,
                            Message::Text(r#"{"type":"CloseStream"}"#.into()),
                            "close_stream",
                        )
                        .await;
                        return Ok(());
                    }
                }
            }
            _ = keepalive.tick() => {
                if unacked_pings >= MAX_UNACKED_PINGS {
                    return Err(format!(
                        "socket unresponsive ({unacked_pings} consecutive keepalive pings unacknowledged)"
                    ));
                }
                send_with_timeout(
                    &mut socket,
                    Message::Text(r#"{"type":"KeepAlive"}"#.into()),
                    "keepalive",
                )
                .await?;
                send_with_timeout(&mut socket, Message::Ping(Vec::new().into()), "ping").await?;
                unacked_pings += 1;
            }
            incoming = socket.next() => {
                match incoming {
                    Some(Ok(Message::Text(text))) => {
                        unacked_pings = 0;
                        if let Some(t) = parse(&text) {
                            let _ = events.send(SttEvent::Transcript(t));
                        }
                    }
                    Some(Ok(Message::Binary(bytes))) => {
                        unacked_pings = 0;
                        if let Ok(text) = String::from_utf8(bytes.to_vec()) {
                            if let Some(t) = parse(&text) {
                                let _ = events.send(SttEvent::Transcript(t));
                            }
                        }
                    }
                    Some(Ok(Message::Pong(_))) => {
                        unacked_pings = 0;
                    }
                    Some(Ok(Message::Ping(payload))) => {
                        unacked_pings = 0;
                        send_with_timeout(&mut socket, Message::Pong(payload), "pong").await?;
                    }
                    Some(Ok(Message::Close(frame))) => {
                        return Err(format!("server closed: {frame:?}"));
                    }
                    Some(Err(err)) => return Err(format!("socket error: {err}")),
                    None => return Err("socket ended".to_string()),
                    _ => {}
                }
            }
        }
    }
}

/// Waits out the reconnect delay while discarding audio.
///
/// The audio channel is unbounded, so sleeping straight through it queues every second of a dead
/// socket and then dumps that whole backlog into Deepgram the moment it reconnects: the tracker
/// would receive a burst of stale, duplicated speech and the highlight would jump around. Those
/// words are lost either way, so dropping them is the honest choice.
///
/// Returns `false` when the capture side has closed, which means the session is over and
/// reconnecting would leave a socket nobody is feeding.
pub(crate) async fn wait_out_backoff(
    backoff: Duration,
    audio_rx: &mut UnboundedReceiver<Vec<i16>>,
) -> bool {
    let deadline = tokio::time::Instant::now() + backoff;
    loop {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            let (_, closed) = drain_all_pending(audio_rx);
            return !closed;
        }
        let slice = (deadline - now).min(Duration::from_millis(250));
        match tokio::time::timeout(slice, audio_rx.recv()).await {
            Ok(Some(_)) => continue,
            Ok(None) => return false,
            Err(_) => {}
        }
    }
}

/// Runs until the audio stream ends, reconnecting with exponential backoff.
pub async fn run(
    api_key: String,
    mut audio_rx: UnboundedReceiver<Vec<i16>>,
    events: UnboundedSender<SttEvent>,
) {
    let mut backoff = 1u64;
    loop {
        let started = tokio::time::Instant::now();
        match run_session(&api_key, &mut audio_rx, &events).await {
            Ok(()) => {
                println!("STT_STOPPED clean");
                return;
            }
            Err(err) => {
                // Reset the ladder after a healthy session: a drop ten minutes into a live call
                // is not the same failure as being unable to connect, and waiting out the 8-10s
                // a burst of failures earned would put a hole in the transcript mid-sentence.
                if started.elapsed() >= HEALTHY_SESSION {
                    backoff = 1;
                }
                eprintln!("STT_ERROR {err} (reconnecting in {backoff}s)");
                let _ = events.send(SttEvent::Error(err));
                if !wait_out_backoff(Duration::from_secs(backoff), &mut audio_rx).await {
                    println!("STT_STOPPED capture closed during backoff");
                    return;
                }
                backoff = (backoff * 2).min(MAX_BACKOFF_SECS);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::parse;
    use std::time::Duration;

    #[test]
    fn extracts_a_final_transcript() {
        let raw = r#"{"type":"Results","is_final":true,"speech_final":true,"channel":{"alternatives":[{"transcript":"how fast do your leads get called back"}]}}"#;
        let parsed = parse(raw).expect("should parse");
        assert_eq!(parsed.text, "how fast do your leads get called back");
        assert!(parsed.is_final && parsed.speech_final);
    }

    #[test]
    fn ignores_control_messages() {
        assert!(parse(r#"{"type":"Metadata","request_id":"abc"}"#).is_none());
        assert!(parse(r#"{"type":"UtteranceEnd","last_word_end":1.2}"#).is_none());
    }

    #[test]
    fn ignores_empty_transcripts() {
        let raw =
            r#"{"type":"Results","is_final":false,"channel":{"alternatives":[{"transcript":""}]}}"#;
        assert!(parse(raw).is_none());
    }

    /// Regression: the backlog of a dead socket used to be replayed on reconnect, so the
    /// tracker saw minutes of stale speech arrive at once.
    #[tokio::test]
    async fn backoff_discards_queued_audio() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        for _ in 0..50 {
            tx.send(vec![7i16; 160]).expect("send");
        }
        let started = std::time::Instant::now();
        assert!(
            super::wait_out_backoff(Duration::from_millis(300), &mut rx).await,
            "capture is still alive, so the loop must continue"
        );
        assert!(
            started.elapsed() >= Duration::from_millis(250),
            "the delay must actually be waited out"
        );
        assert!(
            rx.try_recv().is_err(),
            "the backlog must be drained, not replayed into the next socket"
        );
    }

    /// A stopped session must not reconnect. The old code slept through the closed channel and
    /// then tried for a socket with no producer behind it.
    #[tokio::test]
    async fn backoff_ends_when_capture_stops() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Vec<i16>>();
        drop(tx);
        assert!(!super::wait_out_backoff(Duration::from_secs(5), &mut rx).await);
    }

    #[test]
    fn url_requests_nova3_at_16k_mono() {
        let url = super::listen_url();
        assert!(url.contains("model=nova-3"));
        assert!(url.contains("sample_rate=16000"));
        assert!(url.contains("interim_results=true"));
    }

    #[test]
    fn clamp_live_batch_preserves_normal_single_chunk_without_dropping() {
        let (_tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Vec<i16>>();
        let first: Vec<i16> = (0..1600).collect();
        let (out, dropped, closed) =
            super::clamp_live_batch(first.clone(), &mut rx, super::MAX_LIVE_QUEUE_SAMPLES);
        assert_eq!(out, first);
        assert_eq!(dropped, 0);
        assert!(!closed);
    }

    #[test]
    fn clamp_live_batch_drops_oldest_stale_audio_and_keeps_freshest_tail() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Vec<i16>>();
        // 10 chunks of 1,600 samples = 16,000 samples (1.0s at 16 kHz); max = 6,400 (400ms).
        let first: Vec<i16> = (0..1600).collect();
        for c in 1..10 {
            let start = (c * 1600) as i16;
            tx.send((start..start + 1600).collect()).expect("send");
        }

        let (out, dropped, closed) =
            super::clamp_live_batch(first, &mut rx, super::MAX_LIVE_QUEUE_SAMPLES);
        assert_eq!(out.len(), super::MAX_LIVE_QUEUE_SAMPLES);
        assert_eq!(dropped, 16_000 - super::MAX_LIVE_QUEUE_SAMPLES);
        assert!(!closed);
        assert_eq!(out[0], 9600);
        assert_eq!(*out.last().unwrap(), 15999);
        assert!(rx.try_recv().is_err(), "channel must be fully drained");
    }

    #[test]
    fn clamp_live_batch_trims_straddling_and_oversized_chunks_to_exact_tail() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Vec<i16>>();
        // Straddling boundary: 3 chunks of 300 = 900 samples, max = 500 -> exact tail 400..900.
        let first: Vec<i16> = (0..300).collect();
        tx.send((300..600).collect()).expect("send");
        tx.send((600..900).collect()).expect("send");

        let (out, dropped, closed) = super::clamp_live_batch(first, &mut rx, 500);
        assert_eq!(out.len(), 500);
        assert_eq!(dropped, 400);
        assert!(!closed);
        assert_eq!(out[0], 400);
        assert_eq!(*out.last().unwrap(), 899);

        // Single oversized chunk: 1,200 samples, max = 500 -> exact tail 700..1200.
        let huge: Vec<i16> = (0..1200).collect();
        let (out2, dropped2, _) = super::clamp_live_batch(huge, &mut rx, 500);
        assert_eq!(out2.len(), 500);
        assert_eq!(dropped2, 700);
        assert_eq!(out2[0], 700);
        assert_eq!(*out2.last().unwrap(), 1199);
    }

    #[test]
    fn clamp_live_batch_propagates_disconnect_while_preserving_final_tail() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Vec<i16>>();
        let first: Vec<i16> = (0..400).collect();
        tx.send((400..800).collect()).expect("send");
        drop(tx);

        let (out, dropped, closed) = super::clamp_live_batch(first, &mut rx, 500);
        assert!(closed, "disconnect must be observed so CloseStream is sent");
        assert_eq!(dropped, 300);
        assert_eq!(out.len(), 500);
        assert_eq!(out[0], 300);
        assert_eq!(*out.last().unwrap(), 799);
    }

    #[tokio::test]
    async fn connect_while_draining_flushes_pre_and_mid_handshake_audio() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Vec<i16>>();
        // Pre-handshake backlog
        for _ in 0..20 {
            tx.send(vec![1i16; 160]).expect("send");
        }

        // Mid-handshake producer
        let tx_mid = tx.clone();
        let producer = tokio::spawn(async move {
            for _ in 0..15 {
                let _ = tx_mid.send(vec![2i16; 160]);
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        });

        let fake_connect = async {
            tokio::time::sleep(Duration::from_millis(120)).await;
            Ok::<_, String>("connected")
        };

        let outcome =
            super::connect_while_draining(fake_connect, &mut rx, Duration::from_secs(2)).await;
        producer.await.expect("producer done");

        match outcome {
            super::ConnectOutcome::Connected(val, dropped) => {
                assert_eq!(val, "connected");
                assert!(dropped >= 20 * 160, "pre-connect audio must be discarded");
            }
            other => panic!("expected Connected, got {other:?}"),
        }
        // Drain any chunks produced after connect returned to check clean state
        drop(tx);
    }

    #[tokio::test]
    async fn connect_while_draining_aborts_immediately_when_capture_stops() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Vec<i16>>();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            drop(tx);
        });

        let never_connects = std::future::pending::<Result<(), String>>();
        let started = std::time::Instant::now();
        let outcome =
            super::connect_while_draining(never_connects, &mut rx, Duration::from_secs(10)).await;

        assert!(matches!(outcome, super::ConnectOutcome::CaptureClosed));
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "stop must abort handshake immediately instead of waiting 10s"
        );
    }

    #[tokio::test]
    async fn connect_while_draining_times_out_without_starvation_under_audio_flood() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Vec<i16>>();
        let flooder = tokio::spawn(async move {
            for _ in 0..200 {
                if tx.send(vec![9i16; 160]).is_err() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        });

        let never_connects = std::future::pending::<Result<(), String>>();
        let started = std::time::Instant::now();
        let outcome =
            super::connect_while_draining(never_connects, &mut rx, Duration::from_millis(120))
                .await;

        assert!(
            matches!(outcome, super::ConnectOutcome::TimedOut(dropped) if dropped > 0),
            "expected TimedOut with drained samples, got {outcome:?}"
        );
        assert!(
            started.elapsed() < Duration::from_millis(400),
            "audio flood must not starve connect timeout"
        );
        flooder.abort();
    }
}
