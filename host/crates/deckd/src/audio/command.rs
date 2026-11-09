//! `[voice.tts] engine = "command"`: a speech program on this machine — what
//! Linux speaks with, since it has no system synthesiser to fall back on.
//!
//! The contract is the one `espeak-ng --stdout` and `piper --output_file -`
//! share: the text arrives on the program's stdin, a WAV (16-bit PCM) leaves
//! on its stdout. `[voice.tts] command` is the program and its arguments,
//! split on whitespace; `{voice}` in an argument is the voice the utterance
//! was given (`[voice.tts] voices`, or `cmn`/`en-us` by whether the text has
//! Chinese in it). The whole utterance is synthesised before it is played;
//! barge-in kills the program.
use std::io::{Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

use crossbeam_channel::{bounded, Receiver};

use super::traits::{
    AudioError, AudioFormat, EngineInfo, TtsChunk, TtsEngine, TtsJob, TtsRequest, TtsStop, VoiceInfo,
};

/// The default program: espeak-ng, which speaks Chinese and English.
pub const DEFAULT_COMMAND: &str = "espeak-ng --stdout -v {voice}";

/// The most WAV one utterance may hand back (about 90 s at 22.05 kHz).
const MAX_WAV: usize = 8 * 1024 * 1024;

pub struct CommandTts {
    command: Vec<String>,
    voices: Vec<String>,
}

impl CommandTts {
    #[must_use]
    pub fn new(command: &str, voices: &[String]) -> Self {
        let command = if command.trim().is_empty() { DEFAULT_COMMAND } else { command };
        CommandTts {
            command: command.split_whitespace().map(str::to_string).collect(),
            voices: voices.to_vec(),
        }
    }

    fn voice(&self, req: &TtsRequest) -> String {
        if let Some(v) = req.voice.as_deref().filter(|v| !v.is_empty()) {
            return v.to_string();
        }
        if let Some(v) = self.voices.first() {
            return v.clone();
        }
        if has_cjk(&req.text) { "cmn" } else { "en-us" }.into()
    }
}

fn has_cjk(text: &str) -> bool {
    text.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c))
}

/// The 16-bit mono PCM and its rate out of a WAV; streaming headers (a data
/// size of 0 or 0xFFFFFFFF, as `espeak-ng --stdout` writes) take the rest.
fn parse_wav(bytes: &[u8]) -> Result<(u32, Vec<f32>), String> {
    if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("the program did not write a WAV".into());
    }
    let (mut rate, mut channels, mut bits) = (0u32, 0u16, 0u16);
    let mut at = 12;
    while at + 8 <= bytes.len() {
        let id = &bytes[at..at + 4];
        let size = u32::from_le_bytes([bytes[at + 4], bytes[at + 5], bytes[at + 6], bytes[at + 7]]) as usize;
        let body = at + 8;
        if id == b"fmt " && body + 16 <= bytes.len() {
            channels = u16::from_le_bytes([bytes[body + 2], bytes[body + 3]]);
            rate = u32::from_le_bytes([bytes[body + 4], bytes[body + 5], bytes[body + 6], bytes[body + 7]]);
            bits = u16::from_le_bytes([bytes[body + 14], bytes[body + 15]]);
        } else if id == b"data" {
            if bits != 16 || channels != 1 || rate == 0 {
                return Err(format!("expected 16-bit mono, got {bits}-bit x{channels}"));
            }
            let end = if size == 0 || size == 0xFFFF_FFFF || body + size > bytes.len() { bytes.len() } else { body + size };
            let pcm = bytes[body..end]
                .chunks_exact(2)
                .map(|p| f32::from(i16::from_le_bytes([p[0], p[1]])) / 32768.0)
                .collect();
            return Ok((rate, pcm));
        }
        at = body + size + (size & 1);
    }
    Err("the WAV has no audio".into())
}

struct Process {
    child: Mutex<Option<Child>>,
}

impl TtsStop for Process {
    fn stop(&self) {
        if let Some(mut child) = self.child.lock().unwrap().take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

struct Job {
    format: AudioFormat,
    chunks: Receiver<TtsChunk>,
    stopper: Arc<Process>,
}

impl TtsJob for Job {
    fn format(&self) -> AudioFormat {
        self.format
    }

    fn chunks(&self) -> Receiver<TtsChunk> {
        self.chunks.clone()
    }

    fn stopper(&self) -> Arc<dyn TtsStop> {
        self.stopper.clone()
    }
}

impl TtsEngine for CommandTts {
    fn info(&self) -> EngineInfo {
        EngineInfo {
            name: self.command.first().cloned().unwrap_or_else(|| "command".into()),
            on_device: true,
            locales: Vec::new(),
        }
    }

    fn voices(&self) -> Vec<VoiceInfo> {
        self.voices
            .iter()
            .map(|v| VoiceInfo { id: v.clone(), name: v.clone(), locale: String::new(), enhanced: false })
            .collect()
    }

    fn synthesize(&self, req: &TtsRequest) -> Result<Box<dyn TtsJob>, AudioError> {
        let voice = self.voice(req);
        let (program, args) = self.command.split_first().ok_or(AudioError::NoEngine("synthesiser"))?;
        let mut child = Command::new(program)
            .args(args.iter().map(|a| a.replace("{voice}", &voice)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| AudioError::Engine(format!("{program}: {e} (install it, or set [voice.tts] command)")))?;
        let mut stdin = child.stdin.take().ok_or_else(|| AudioError::Engine("no stdin".into()))?;
        let mut stdout = child.stdout.take().ok_or_else(|| AudioError::Engine("no stdout".into()))?;
        let text = req.text.clone();
        std::thread::spawn(move || {
            let _ = stdin.write_all(text.as_bytes());
        });
        let process = Arc::new(Process { child: Mutex::new(Some(child)) });
        let mut wav = Vec::new();
        stdout
            .by_ref()
            .take(MAX_WAV as u64)
            .read_to_end(&mut wav)
            .map_err(|e| AudioError::Engine(format!("{program}: {e}")))?;
        process.stop();
        let (rate, pcm) = parse_wav(&wav).map_err(|e| AudioError::Engine(format!("{program}: {e}")))?;
        let (tx, rx) = bounded(2);
        let _ = tx.send(TtsChunk::Pcm(pcm));
        let _ = tx.send(TtsChunk::End);
        Ok(Box::new(Job { format: AudioFormat::new(rate, 1), chunks: rx, stopper: process }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wav(rate: u32, size_field: u32, samples: &[i16]) -> Vec<u8> {
        let mut out = b"RIFF\xff\xff\xff\xffWAVEfmt ".to_vec();
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&[1, 0, 1, 0]);
        out.extend_from_slice(&rate.to_le_bytes());
        out.extend_from_slice(&(rate * 2).to_le_bytes());
        out.extend_from_slice(&[2, 0, 16, 0]);
        out.extend_from_slice(b"data");
        out.extend_from_slice(&size_field.to_le_bytes());
        for s in samples {
            out.extend_from_slice(&s.to_le_bytes());
        }
        out
    }

    #[test]
    fn a_wav_with_a_streaming_size_reads_to_the_end() {
        let (rate, pcm) = parse_wav(&wav(22050, 0xFFFF_FFFF, &[0, 16384, -16384])).unwrap();
        assert_eq!((rate, pcm.len()), (22050, 3));
        assert!((pcm[1] - 0.5).abs() < 1e-4);
    }

    #[test]
    fn something_that_is_not_a_wav_is_refused() {
        assert!(parse_wav(b"not a wav at all").is_err());
    }

    #[test]
    fn the_voice_follows_the_text_unless_one_is_set() {
        let req = |text: &str, voice: Option<&str>| TtsRequest { text: text.into(), voice: voice.map(Into::into), rate: 0.5 };
        let tts = CommandTts::new("", &[]);
        assert_eq!(tts.voice(&req("你好", None)), "cmn");
        assert_eq!(tts.voice(&req("hello", None)), "en-us");
        assert_eq!(tts.voice(&req("hello", Some("de"))), "de");
        assert_eq!(CommandTts::new("", &["zh".into()]).voice(&req("hello", None)), "zh");
    }

    #[cfg(unix)]
    #[test]
    fn a_program_that_writes_a_wav_is_played() {
        let dir = std::env::temp_dir().join(format!("owt-cmd-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.wav");
        std::fs::write(&file, wav(16000, 0, &[1000; 160])).unwrap();
        let tts = CommandTts::new(&format!("cat {}", file.display()), &[]);
        let job = tts.synthesize(&TtsRequest { text: "x".into(), voice: None, rate: 0.5 }).unwrap();
        assert_eq!(job.format(), AudioFormat::new(16000, 1));
        let first = job.chunks().recv().unwrap();
        assert!(matches!(first, TtsChunk::Pcm(p) if p.len() == 160));
        assert_eq!(job.chunks().recv().unwrap(), TtsChunk::End);
        std::fs::remove_dir_all(dir).ok();
    }
}
