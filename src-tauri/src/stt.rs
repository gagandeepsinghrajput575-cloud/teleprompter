//! Deepgram Nova-3 streaming transcription.
//!
//! Two things here are load-bearing and easy to get wrong:
//!  * Deepgram closes an idle socket after 10s with `NET-0001`, and a sales call is full
//!    of long silences, so `KeepAlive` must be sent every 4s.
//!  * The socket dies when the laptop sleeps or wifi drops, so reconnection is not an
//!    edge case, it is the normal path.

use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use std::time::Duration;
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
/// A session that lived this long was healthy, so a later drop should reconnect in 1s rather
/// than inheriting the backoff that a failed connect earned.
const HEALTHY_SESSION: Duration = Duration::from_secs(5);

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

    let (mut socket, _) = tokio::time::timeout(CONNECT_TIMEOUT, connect_async(request))
        .await
        .map_err(|_| format!("connect timed out after {}s", CONNECT_TIMEOUT.as_secs()))?
        .map_err(|e| format!("connect failed: {e}"))?;
    println!("STT_CONNECTED model=nova-3 rate=16000");
    let _ = events.send(SttEvent::Connected);

    let mut keepalive = tokio::time::interval(KEEPALIVE_INTERVAL);
    keepalive.tick().await; // the first tick fires immediately; ignore it

    loop {
        tokio::select! {
            chunk = audio_rx.recv() => {
                match chunk {
                    Some(pcm) => {
                        let mut bytes = Vec::with_capacity(pcm.len() * 2);
                        for sample in pcm {
                            bytes.extend_from_slice(&sample.to_le_bytes());
                        }
                        socket
                            .send(Message::Binary(bytes.into()))
                            .await
                            .map_err(|e| format!("send failed: {e}"))?;
                    }
                    None => {
                        let _ = socket.send(Message::Text(r#"{"type":"CloseStream"}"#.into())).await;
                        return Ok(());
                    }
                }
            }
            _ = keepalive.tick() => {
                socket
                    .send(Message::Text(r#"{"type":"KeepAlive"}"#.into()))
                    .await
                    .map_err(|e| format!("keepalive failed: {e}"))?;
            }
            incoming = socket.next() => {
                match incoming {
                    Some(Ok(Message::Text(text))) => {
                        if let Some(t) = parse(&text) {
                            let _ = events.send(SttEvent::Transcript(t));
                        }
                    }
                    Some(Ok(Message::Binary(bytes))) => {
                        if let Ok(text) = String::from_utf8(bytes.to_vec()) {
                            if let Some(t) = parse(&text) {
                                let _ = events.send(SttEvent::Transcript(t));
                            }
                        }
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
async fn wait_out_backoff(
    backoff: Duration,
    audio_rx: &mut UnboundedReceiver<Vec<i16>>,
) -> bool {
    let deadline = tokio::time::Instant::now() + backoff;
    loop {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return true;
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
}
