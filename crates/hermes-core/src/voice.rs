//! Rust-native voice runtime primitives.
//!
//! The runtime is intentionally frame-based: transports, speech adapters, and
//! the Hermes agent exchange small typed frames so local audio and WebRTC can
//! share the same turn loop.

use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::agent::{AgentEvent, HermesAgent};
use crate::error::Result;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioFrame {
    pub samples: Vec<i16>,
    pub sample_rate_hz: u32,
    pub channels: u16,
}

impl AudioFrame {
    pub fn new(samples: Vec<i16>, sample_rate_hz: u32, channels: u16) -> Self {
        Self {
            samples,
            sample_rate_hz,
            channels,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VoiceFrame {
    InputAudio(AudioFrame),
    UserTranscriptDelta { text: String },
    UserTranscriptFinal { text: String },
    AssistantTextDelta { text: String },
    AssistantTextFinal { text: String },
    OutputAudio(AudioFrame),
    Interruption,
    Error { message: String },
    Shutdown,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VoiceRunSummary {
    pub completed_turns: usize,
    pub interruptions: usize,
}

#[async_trait]
pub trait VoiceInput: Send {
    async fn next_frame(&mut self) -> Result<Option<VoiceFrame>>;
}

#[async_trait]
pub trait VoiceOutput: Send {
    async fn send_frame(&mut self, frame: VoiceFrame) -> Result<()>;
}

#[async_trait]
pub trait SpeechToText: Send {
    async fn transcribe(&mut self, audio: AudioFrame) -> Result<Vec<VoiceFrame>>;
}

#[async_trait]
pub trait TextToSpeech: Send {
    async fn synthesize(&mut self, text: &str) -> Result<Vec<AudioFrame>>;

    async fn finish_turn(&mut self, _final_text: &str) -> Result<Vec<AudioFrame>> {
        Ok(Vec::new())
    }

    async fn interrupt(&mut self) -> Result<()> {
        Ok(())
    }
}

#[async_trait]
pub trait VoiceFrameSink: Send {
    async fn send(&mut self, frame: VoiceFrame) -> Result<()>;
}

#[async_trait]
pub trait VoiceResponder: Send {
    async fn respond(&mut self, user_text: String, sink: &mut dyn VoiceFrameSink) -> Result<()>;
}

pub struct NoopSpeechToText;

#[async_trait]
impl SpeechToText for NoopSpeechToText {
    async fn transcribe(&mut self, _audio: AudioFrame) -> Result<Vec<VoiceFrame>> {
        Ok(Vec::new())
    }
}

pub struct NoopTextToSpeech;

#[async_trait]
impl TextToSpeech for NoopTextToSpeech {
    async fn synthesize(&mut self, _text: &str) -> Result<Vec<AudioFrame>> {
        Ok(Vec::new())
    }
}

pub struct HermesVoiceResponder {
    agent: HermesAgent,
    event_rx: mpsc::Receiver<AgentEvent>,
}

impl HermesVoiceResponder {
    pub fn new(agent: HermesAgent, event_rx: mpsc::Receiver<AgentEvent>) -> Self {
        Self { agent, event_rx }
    }
}

#[async_trait]
impl VoiceResponder for HermesVoiceResponder {
    async fn respond(&mut self, user_text: String, sink: &mut dyn VoiceFrameSink) -> Result<()> {
        let mut streamed_text = String::new();
        let mut events_open = true;
        let run = self.agent.run(user_text);
        tokio::pin!(run);

        loop {
            tokio::select! {
                event = self.event_rx.recv(), if events_open => {
                    match event {
                        Some(AgentEvent::Content { text }) => {
                            streamed_text.push_str(&text);
                            sink.send(VoiceFrame::AssistantTextDelta { text }).await?;
                        }
                        Some(_) => {}
                        None => {
                            events_open = false;
                        }
                    }
                }
                result = &mut run => {
                    let message = result?;
                    if streamed_text.is_empty() && !message.content.is_empty() {
                        sink.send(VoiceFrame::AssistantTextDelta {
                            text: message.content.clone(),
                        })
                        .await?;
                    }
                    sink.send(VoiceFrame::AssistantTextFinal {
                        text: message.content,
                    })
                    .await?;
                    break;
                }
            }
        }

        Ok(())
    }
}

pub struct VoiceRuntime<I, O, S, T, R> {
    input: I,
    output: O,
    speech_to_text: S,
    text_to_speech: T,
    responder: R,
    allow_interruptions: bool,
}

impl<I, O, S, T, R> VoiceRuntime<I, O, S, T, R>
where
    I: VoiceInput,
    O: VoiceOutput,
    S: SpeechToText,
    T: TextToSpeech,
    R: VoiceResponder,
{
    pub fn new(
        input: I,
        output: O,
        speech_to_text: S,
        text_to_speech: T,
        responder: R,
        allow_interruptions: bool,
    ) -> Self {
        Self {
            input,
            output,
            speech_to_text,
            text_to_speech,
            responder,
            allow_interruptions,
        }
    }

    pub async fn run(&mut self) -> Result<VoiceRunSummary> {
        let mut summary = VoiceRunSummary::default();

        while let Some(frame) = self.input.next_frame().await? {
            if self.handle_frame(frame, &mut summary).await? {
                break;
            }
        }

        Ok(summary)
    }

    async fn handle_frame(
        &mut self,
        frame: VoiceFrame,
        summary: &mut VoiceRunSummary,
    ) -> Result<bool> {
        match frame {
            VoiceFrame::InputAudio(audio) => {
                let frames = self.speech_to_text.transcribe(audio).await?;
                for frame in frames {
                    if self.handle_control_frame(frame, summary).await? {
                        return Ok(true);
                    }
                }
                Ok(false)
            }
            frame => self.handle_control_frame(frame, summary).await,
        }
    }

    async fn handle_control_frame(
        &mut self,
        frame: VoiceFrame,
        summary: &mut VoiceRunSummary,
    ) -> Result<bool> {
        match frame {
            VoiceFrame::Shutdown => {
                self.output.send_frame(VoiceFrame::Shutdown).await?;
                Ok(true)
            }
            VoiceFrame::Interruption => {
                if self.allow_interruptions {
                    summary.interruptions += 1;
                    self.text_to_speech.interrupt().await?;
                    self.output.send_frame(VoiceFrame::Interruption).await?;
                }
                Ok(false)
            }
            VoiceFrame::UserTranscriptDelta { text } => {
                self.output
                    .send_frame(VoiceFrame::UserTranscriptDelta { text })
                    .await?;
                Ok(false)
            }
            VoiceFrame::UserTranscriptFinal { text } => {
                let text = text.trim().to_string();
                if text.is_empty() {
                    return Ok(false);
                }

                self.output
                    .send_frame(VoiceFrame::UserTranscriptFinal { text: text.clone() })
                    .await?;

                let mut sink = AssistantFrameSink {
                    output: &mut self.output,
                    text_to_speech: &mut self.text_to_speech,
                };
                self.responder.respond(text, &mut sink).await?;
                summary.completed_turns += 1;
                Ok(false)
            }
            other => {
                self.output.send_frame(other).await?;
                Ok(false)
            }
        }
    }
}

struct AssistantFrameSink<'a, O, T> {
    output: &'a mut O,
    text_to_speech: &'a mut T,
}

#[async_trait]
impl<O, T> VoiceFrameSink for AssistantFrameSink<'_, O, T>
where
    O: VoiceOutput,
    T: TextToSpeech,
{
    async fn send(&mut self, frame: VoiceFrame) -> Result<()> {
        match frame {
            VoiceFrame::AssistantTextDelta { text } => {
                self.output
                    .send_frame(VoiceFrame::AssistantTextDelta { text: text.clone() })
                    .await?;
                for audio in self.text_to_speech.synthesize(&text).await? {
                    self.output
                        .send_frame(VoiceFrame::OutputAudio(audio))
                        .await?;
                }
            }
            VoiceFrame::AssistantTextFinal { text } => {
                self.output
                    .send_frame(VoiceFrame::AssistantTextFinal { text: text.clone() })
                    .await?;
                for audio in self.text_to_speech.finish_turn(&text).await? {
                    self.output
                        .send_frame(VoiceFrame::OutputAudio(audio))
                        .await?;
                }
            }
            other => {
                self.output.send_frame(other).await?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    struct VecInput {
        frames: std::collections::VecDeque<VoiceFrame>,
    }

    impl VecInput {
        fn new(frames: Vec<VoiceFrame>) -> Self {
            Self {
                frames: frames.into(),
            }
        }
    }

    #[async_trait]
    impl VoiceInput for VecInput {
        async fn next_frame(&mut self) -> Result<Option<VoiceFrame>> {
            Ok(self.frames.pop_front())
        }
    }

    #[derive(Clone, Default)]
    struct SharedOutput {
        frames: Arc<Mutex<Vec<VoiceFrame>>>,
    }

    #[async_trait]
    impl VoiceOutput for SharedOutput {
        async fn send_frame(&mut self, frame: VoiceFrame) -> Result<()> {
            self.frames.lock().unwrap().push(frame);
            Ok(())
        }
    }

    struct FinalTextStt;

    #[async_trait]
    impl SpeechToText for FinalTextStt {
        async fn transcribe(&mut self, _audio: AudioFrame) -> Result<Vec<VoiceFrame>> {
            Ok(vec![VoiceFrame::UserTranscriptFinal {
                text: "hello from audio".to_string(),
            }])
        }
    }

    #[derive(Default)]
    struct RecordingTts {
        interrupted: bool,
    }

    #[async_trait]
    impl TextToSpeech for RecordingTts {
        async fn synthesize(&mut self, text: &str) -> Result<Vec<AudioFrame>> {
            Ok(vec![AudioFrame::new(
                text.bytes().map(i16::from).collect(),
                48_000,
                1,
            )])
        }

        async fn interrupt(&mut self) -> Result<()> {
            self.interrupted = true;
            Ok(())
        }
    }

    struct ScriptedResponder;

    #[async_trait]
    impl VoiceResponder for ScriptedResponder {
        async fn respond(
            &mut self,
            user_text: String,
            sink: &mut dyn VoiceFrameSink,
        ) -> Result<()> {
            sink.send(VoiceFrame::AssistantTextDelta {
                text: format!("heard: {}", user_text),
            })
            .await?;
            sink.send(VoiceFrame::AssistantTextFinal {
                text: "done".to_string(),
            })
            .await?;
            Ok(())
        }
    }

    #[tokio::test]
    async fn transcript_turn_runs_responder_and_tts() {
        let input = VecInput::new(vec![
            VoiceFrame::UserTranscriptFinal {
                text: "hello".to_string(),
            },
            VoiceFrame::Shutdown,
        ]);
        let output = SharedOutput::default();
        let captured = output.frames.clone();
        let mut runtime = VoiceRuntime::new(
            input,
            output,
            NoopSpeechToText,
            RecordingTts::default(),
            ScriptedResponder,
            true,
        );

        let summary = runtime.run().await.unwrap();
        let frames = captured.lock().unwrap().clone();

        assert_eq!(summary.completed_turns, 1);
        assert!(frames.contains(&VoiceFrame::AssistantTextDelta {
            text: "heard: hello".to_string()
        }));
        assert!(frames
            .iter()
            .any(|frame| matches!(frame, VoiceFrame::OutputAudio(_))));
        assert_eq!(frames.last(), Some(&VoiceFrame::Shutdown));
    }

    #[tokio::test]
    async fn audio_frames_flow_through_stt() {
        let input = VecInput::new(vec![
            VoiceFrame::InputAudio(AudioFrame::new(vec![1, 2, 3], 16_000, 1)),
            VoiceFrame::Shutdown,
        ]);
        let output = SharedOutput::default();
        let captured = output.frames.clone();
        let mut runtime = VoiceRuntime::new(
            input,
            output,
            FinalTextStt,
            NoopTextToSpeech,
            ScriptedResponder,
            true,
        );

        let summary = runtime.run().await.unwrap();
        let frames = captured.lock().unwrap().clone();

        assert_eq!(summary.completed_turns, 1);
        assert!(frames.contains(&VoiceFrame::UserTranscriptFinal {
            text: "hello from audio".to_string()
        }));
    }

    #[tokio::test]
    async fn interruption_is_forwarded_when_allowed() {
        let input = VecInput::new(vec![VoiceFrame::Interruption, VoiceFrame::Shutdown]);
        let output = SharedOutput::default();
        let captured = output.frames.clone();
        let mut runtime = VoiceRuntime::new(
            input,
            output,
            NoopSpeechToText,
            NoopTextToSpeech,
            ScriptedResponder,
            true,
        );

        let summary = runtime.run().await.unwrap();
        let frames = captured.lock().unwrap().clone();

        assert_eq!(summary.interruptions, 1);
        assert!(frames.contains(&VoiceFrame::Interruption));
    }
}
