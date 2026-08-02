//! 接驳语音输入转写、回复语音合成及其取消与迟到结果隔离。

use futures::future::{AbortHandle, Abortable};
use gpui::Context;
use gpui_tokio::Tokio;
use lunamate_agent::{
    AgentSnapshot,
    config::AppLanguage,
    stt::{TranscriptionInput, transcribe},
    tts::synthesize,
};
use rust_i18n::t;

use crate::{config::CONFIG, voice::VoiceController};

use super::{AgentView, AgentViewEvent, RemoteTranscriptionRequest, ThinkingFeedback};

impl AgentView {
    pub fn voice_speech_started(
        &mut self,
        utterance_id: u64,
        language: AppLanguage,
        cx: &mut Context<Self>,
    ) {
        self.cancel_remote_transcription();
        self.cancel_speech();
        cx.emit(AgentViewEvent::StopSpeech);
        let snapshot = CONFIG.agent_config_snapshot();
        if snapshot.generation() != self.agent_config_generation {
            self.refresh_settings(snapshot, cx);
            return;
        }
        self.agent.voice_started(utterance_id, language);
        self.sync_agent_snapshot(cx);
        cx.notify();
    }

    /// 在按键边沿立即停止本地语音，语义层打断仍等待真实语音确认。
    pub fn voice_input_pressed(&mut self, cx: &mut Context<Self>) {
        self.cancel_speech();
        cx.emit(AgentViewEvent::StopSpeech);
    }

    pub fn send_voice_transcript(
        &mut self,
        utterance_id: u64,
        text: String,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(language) = self.agent.take_voice_transcript(utterance_id) else {
            return false;
        };
        self.send_message(text, None, language, Some(ThinkingFeedback::Voice), cx)
    }

    pub fn voice_utterance_cancelled(&mut self, utterance_id: u64) {
        self.cancel_remote_transcription_for(utterance_id);
        self.cancel_speech();
        self.agent.cancel_voice(utterance_id);
    }

    pub fn cancel_pending_voice(&mut self) {
        self.cancel_remote_transcription();
        self.cancel_speech();
        self.agent.cancel_pending_voice();
    }

    pub fn transcribe_voice(
        &mut self,
        revision: u64,
        utterance_id: u64,
        model_id: String,
        samples: Vec<i16>,
        voice: VoiceController,
        cx: &mut Context<Self>,
    ) {
        self.cancel_remote_transcription();
        self.cancel_speech();
        cx.emit(AgentViewEvent::StopSpeech);
        let language = self.snapshot.language();
        let Some(model) = CONFIG.llm_settings().model(&model_id).cloned() else {
            voice.complete_remote_transcription(
                revision,
                utterance_id,
                Err(t!("voice.model_missing", locale = language.id()).to_string()),
            );
            return;
        };
        let (abort_handle, abort_registration) = AbortHandle::new_pair();
        let task = Tokio::spawn(
            cx,
            Abortable::new(
                async move {
                    let input = TranscriptionInput::new(samples)
                        .map_err(|error| error.localized_message(language))?;
                    transcribe(&model, input, language)
                        .await
                        .map_err(|error| error.localized_message(language))
                },
                abort_registration,
            ),
        );
        let request = RemoteTranscriptionRequest {
            revision,
            utterance_id,
            voice: voice.clone(),
        };
        self.voice_transcription_abort = Some(abort_handle);
        self.voice_transcription_request = Some(request.clone());
        self.voice_transcription_task = Some(cx.spawn(async move |this, cx| {
            let result = match task.await {
                Ok(Ok(result)) => result,
                Ok(Err(_)) | Err(_) => {
                    Err(t!("voice.transcription_cancelled", locale = language.id()).to_string())
                }
            };
            let current = this
                .update(cx, |this, _| {
                    this.voice_transcription_request
                        .as_ref()
                        .is_some_and(|request| {
                            request.revision == revision && request.utterance_id == utterance_id
                        })
                })
                .unwrap_or(false);
            if current {
                voice.complete_remote_transcription(revision, utterance_id, result);
            }
            let _ = this.update(cx, |this, _| {
                if this
                    .voice_transcription_request
                    .as_ref()
                    .is_some_and(|request| {
                        request.revision == revision && request.utterance_id == utterance_id
                    })
                {
                    this.voice_transcription_request = None;
                    this.voice_transcription_abort = None;
                    this.voice_transcription_task = None;
                }
            });
        }));
    }

    pub fn voice_failed(&mut self, message: String, cx: &mut Context<Self>) {
        self.cancel_remote_transcription();
        self.cancel_speech();
        cx.emit(AgentViewEvent::StopSpeech);
        self.agent.cancel_pending_voice();
        self.agent.set_status(Some(message));
        self.sync_agent_snapshot(cx);
    }

    pub(in crate::ui) fn stop_voice_interaction(&mut self, cx: &mut Context<Self>) {
        self.thinking_feedback_revision = self.thinking_feedback_revision.wrapping_add(1).max(1);
        self.thinking_feedback = None;
        self.cancel_remote_transcription();
        self.agent.cancel_pending_voice();
        cx.emit(AgentViewEvent::StopSpeech);
        self.stop(cx);
        cx.notify();
    }

    pub(super) fn cancel_remote_transcription(&mut self) {
        if let Some(request) = self.voice_transcription_request.take() {
            request
                .voice
                .cancel_remote_transcription(request.revision, request.utterance_id);
        }
        if let Some(abort) = self.voice_transcription_abort.take() {
            abort.abort();
        }
        self.voice_transcription_task = None;
    }

    fn cancel_remote_transcription_for(&mut self, utterance_id: u64) {
        if self
            .voice_transcription_request
            .as_ref()
            .is_some_and(|request| request.utterance_id == utterance_id)
        {
            self.cancel_remote_transcription();
        }
    }

    pub(super) fn start_speech(&mut self, snapshot: AgentSnapshot, cx: &mut Context<Self>) {
        self.cancel_speech();
        let revision = self.speech_revision;
        let personas = CONFIG.persona_settings();
        let Some(persona) = personas.active() else {
            return;
        };
        if persona.id != snapshot.active_persona() {
            return;
        }
        let Some(model_id) = persona.tts_model.as_deref() else {
            return;
        };
        let Some(message) = snapshot.messages().iter().find(|message| {
            Some(message.id()) == snapshot.reply_message_id()
                && message.role() == lunamate_agent::ChatRole::Assistant
        }) else {
            return;
        };
        if !matches!(message.state(), lunamate_agent::ChatMessageState::Complete) {
            return;
        }
        let Some(model) = CONFIG.llm_settings().model(model_id).cloned() else {
            return;
        };
        let text = message.visible_content().to_owned();
        let (abort_handle, abort_registration) = AbortHandle::new_pair();
        let task = Tokio::spawn(
            cx,
            Abortable::new(
                async move { synthesize(&model, &text).await },
                abort_registration,
            ),
        );
        self.speech_abort = Some(abort_handle);
        self.speech_task = Some(cx.spawn(async move |this, cx| {
            match task.await {
                Ok(Ok(Ok(audio))) => {
                    let _ = this.update(cx, |this, cx| {
                        if this.suspended || this.speech_revision != revision {
                            return;
                        }
                        let sample_rate = audio.sample_rate();
                        cx.emit(AgentViewEvent::SpeechAudio {
                            samples: audio.into_samples(),
                            sample_rate,
                        });
                    });
                }
                Ok(Ok(Err(_))) => log::warn!("event=speech_synthesis_failed stage=request"),
                Ok(Err(_)) => {}
                Err(error) if !error.is_cancelled() => {
                    log::warn!("event=speech_synthesis_failed stage=task_join");
                }
                Err(_) => {}
            }
            let _ = this.update(cx, |this, _| {
                if this.speech_revision == revision {
                    this.speech_abort = None;
                    this.speech_task = None;
                }
            });
        }));
    }

    pub(super) fn cancel_speech(&mut self) {
        self.speech_revision = self.speech_revision.wrapping_add(1).max(1);
        if let Some(abort) = self.speech_abort.take() {
            abort.abort();
        }
        self.speech_task = None;
    }
}
