//! Microphone capture.
//!
//! Capture happens in Rust rather than in the webview because `getUserMedia` is
//! unreliable inside WKWebView on macOS. `cpal` also gives raw PCM, which is what the
//! pace and inflection coaching will need later.
//!
//! The device is resampled to 16 kHz mono i16 — the exact format Deepgram's streaming
//! endpoint expects and the exact format the TTS fixtures are generated in.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::SizedSample;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc::UnboundedSender;

pub const TARGET_RATE: u32 = 16_000;

#[derive(Clone, Debug, serde::Serialize)]
pub struct CaptureStats {
    pub device: String,
    pub source_rate: u32,
    pub channels: u16,
    pub sample_format: String,
    pub target_rate: u32,
}

pub struct Capture {
    // Dropping the stream stops capture.
    _stream: cpal::Stream,
    pub stats: CaptureStats,
}

/// Names of every input device the host exposes.
pub fn list_input_devices() -> Result<Vec<String>, String> {
    let host = cpal::default_host();
    let devices = host
        .input_devices()
        .map_err(|e| format!("cannot enumerate input devices: {e}"))?;
    Ok(devices
        .filter_map(|d| d.description().ok().map(|desc| desc.name().to_string()))
        .collect())
}

fn pick_device(preferred: Option<&str>) -> Result<cpal::Device, String> {
    let host = cpal::default_host();
    if let Some(wanted) = preferred {
        if let Ok(devices) = host.input_devices() {
            for device in devices {
                let matches = device
                    .description()
                    .map(|desc| desc.name() == wanted)
                    .unwrap_or(false);
                if matches {
                    return Ok(device);
                }
            }
        }
        return Err(format!("input device '{wanted}' not found"));
    }
    host.default_input_device()
        .ok_or_else(|| "no default input device".to_string())
}

/// Linear resampler from the device rate to [`TARGET_RATE`], mono-mixing as it goes.
///
/// Positions are tracked in *source* sample time: `next_time` is the source-time of the
/// next output sample and `index` is the source-time of the current input frame, so the
/// interpolation fraction between `prev` and the current frame is always well defined.
struct Resampler {
    channels: usize,
    /// Source samples per output sample (e.g. 3.0 for 48 kHz -> 16 kHz).
    period: f64,
    next_time: f64,
    index: f64,
    prev: f32,
    primed: bool,
}

impl Resampler {
    fn new(channels: u16, source_rate: u32) -> Self {
        Self {
            channels: channels.max(1) as usize,
            period: source_rate as f64 / TARGET_RATE as f64,
            next_time: 0.0,
            index: 0.0,
            prev: 0.0,
            primed: false,
        }
    }

    fn push(&mut self, interleaved: &[f32], out: &mut Vec<i16>) {
        for frame in interleaved.chunks(self.channels) {
            let mono = frame.iter().sum::<f32>() / self.channels as f32;
            if !self.primed {
                self.prev = mono;
                self.primed = true;
                continue;
            }
            self.index += 1.0;
            while self.next_time <= self.index {
                let frac = (self.next_time - (self.index - 1.0)).clamp(0.0, 1.0) as f32;
                let sample = self.prev + (mono - self.prev) * frac;
                out.push((sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16);
                self.next_time += self.period;
            }
            self.prev = mono;
        }
    }
}

fn rms(interleaved: &[f32]) -> f32 {
    if interleaved.is_empty() {
        return 0.0;
    }
    let sum: f32 = interleaved.iter().map(|s| s * s).sum();
    (sum / interleaved.len() as f32).sqrt()
}

fn build_stream<T, F>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    channels: u16,
    source_rate: u32,
    to_f32: F,
    audio_tx: UnboundedSender<Vec<i16>>,
    level: Arc<AtomicU32>,
) -> Result<cpal::Stream, cpal::BuildStreamError>
where
    T: SizedSample,
    F: Fn(T) -> f32 + Send + 'static,
{
    let mut resampler = Resampler::new(channels, source_rate);
    device.build_input_stream(
        config,
        move |data: &[T], _: &cpal::InputCallbackInfo| {
            let mono: Vec<f32> = data.iter().map(|s| to_f32(*s)).collect();
            level.store(rms(&mono).to_bits(), Ordering::Relaxed);
            let mut pcm = Vec::with_capacity(mono.len() / 2 + 8);
            resampler.push(&mono, &mut pcm);
            if !pcm.is_empty() {
                // Receiver gone means the session ended; drop the audio.
                let _ = audio_tx.send(pcm);
            }
        },
        move |err| eprintln!("AUDIO_ERROR stream: {err}"),
        None,
    )
}

/// Starts capturing, forwarding 16 kHz mono PCM to `audio_tx`.
pub fn start(
    preferred_device: Option<&str>,
    audio_tx: UnboundedSender<Vec<i16>>,
    level: Arc<AtomicU32>,
) -> Result<Capture, String> {
    let device = pick_device(preferred_device)?;
    let device_name = device
        .description()
        .map(|desc| desc.name().to_string())
        .unwrap_or_else(|_| "unknown".to_string());
    let supported = device
        .default_input_config()
        .map_err(|e| format!("no default input config for '{device_name}': {e}"))?;

    let source_rate = supported.sample_rate();
    let channels = supported.channels();
    let sample_format = supported.sample_format();
    let config = supported.config();

    let stream = match sample_format {
        cpal::SampleFormat::F32 => build_stream::<f32, _>(
            &device,
            &config,
            channels,
            source_rate,
            |s| s,
            audio_tx,
            level,
        ),
        cpal::SampleFormat::I16 => build_stream::<i16, _>(
            &device,
            &config,
            channels,
            source_rate,
            |s| s as f32 / 32768.0,
            audio_tx,
            level,
        ),
        cpal::SampleFormat::U16 => build_stream::<u16, _>(
            &device,
            &config,
            channels,
            source_rate,
            |s| (s as f32 - 32768.0) / 32768.0,
            audio_tx,
            level,
        ),
        other => return Err(format!("unsupported sample format {other:?}")),
    }
    .map_err(|e| format!("cannot build input stream: {e}"))?;

    stream
        .play()
        .map_err(|e| format!("cannot start input stream: {e}"))?;

    println!(
        "AUDIO_STARTED device='{}' source_rate={} channels={} format={:?} -> {}Hz mono i16",
        device_name, source_rate, channels, sample_format, TARGET_RATE
    );

    Ok(Capture {
        _stream: stream,
        stats: CaptureStats {
            device: device_name,
            source_rate,
            channels,
            sample_format: format!("{sample_format:?}"),
            target_rate: TARGET_RATE,
        },
    })
}

/// Streams a 16 kHz mono WAV into the same channel the microphone feeds.
///
/// This is the Simulated Call Mode: it exercises the real Deepgram socket and the real
/// tracker without needing a microphone, which makes the pipeline testable and
/// reproducible even when no human is available to speak.
pub fn replay_wav(path: &str, tx: UnboundedSender<Vec<i16>>) -> Result<CaptureStats, String> {
    let reader = hound::WavReader::open(path).map_err(|e| format!("cannot open {path}: {e}"))?;
    let spec = reader.spec();
    if spec.channels != 1 || spec.sample_rate != TARGET_RATE {
        return Err(format!(
            "fixture must be mono {}Hz, got {}ch {}Hz",
            TARGET_RATE, spec.channels, spec.sample_rate
        ));
    }
    let samples: Vec<i16> = reader
        .into_samples::<i16>()
        .filter_map(|s| s.ok())
        .collect();
    if samples.is_empty() {
        return Err(format!("{path} contained no samples"));
    }
    // Owned so the replay thread can be 'static.
    let path = path.to_string();

    let stats = CaptureStats {
        device: format!("fixture:{path}"),
        source_rate: TARGET_RATE,
        channels: 1,
        sample_format: "i16".to_string(),
        target_rate: TARGET_RATE,
    };

    // 100 ms of audio per tick, paced at real time so latency measurements are honest.
    let chunk = (TARGET_RATE / 10) as usize;
    std::thread::Builder::new()
        .name("wav-replay".into())
        .spawn(move || {
            println!("REPLAY_STARTED {path} samples={}", samples.len());
            for piece in samples.chunks(chunk) {
                if tx.send(piece.to_vec()).is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            println!("REPLAY_FINISHED {path}");
        })
        .map_err(|e| format!("cannot spawn replay thread: {e}"))?;

    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::Resampler;

    #[test]
    fn downsamples_48k_to_16k_at_one_third_the_length() {
        let mut r = Resampler::new(1, 48_000);
        let mut out = Vec::new();
        // Constant DC input so interpolation cannot attenuate the signal.
        let input = vec![0.5f32; 48_000];
        r.push(&input, &mut out);
        let expected = 16_000;
        let drift = (out.len() as i64 - expected as i64).abs();
        assert!(
            drift <= 4,
            "got {} samples, expected ~{expected}",
            out.len()
        );
    }

    #[test]
    fn mixes_stereo_to_mono() {
        let mut r = Resampler::new(2, 16_000);
        let mut out = Vec::new();
        r.push(&[1.0, -1.0, 1.0, -1.0], &mut out);
        // Left/right cancel, so the mono signal is silence.
        assert!(out.iter().all(|s| *s == 0));
    }

    #[test]
    fn clamps_instead_of_wrapping() {
        let mut r = Resampler::new(1, 16_000);
        let mut out = Vec::new();
        r.push(&[5.0, 5.0, 5.0, 5.0], &mut out);
        assert!(out.iter().all(|s| *s == i16::MAX));
    }
}
