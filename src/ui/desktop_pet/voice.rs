//! 接驳语音控制器事件、录音电平刷新及按住说话生命周期。

use gpui::Context;

use super::{DesktopPetView, VOICE_LEVEL_REFRESH_INTERVAL, VOICE_SHORTCUT_RELEASE_TIMEOUT};
use crate::{
    config::{CONFIG, VoiceMode, VoiceSettings},
    voice::{VoiceActivitySnapshot, VoiceEvent, VoicePhase},
};

impl DesktopPetView {
    pub(super) fn apply_voice_settings(
        &mut self,
        settings: &VoiceSettings,
        cx: &mut Context<Self>,
    ) {
        self.release_voice_shortcut();
        self.voice_mode = settings.mode;
        if let Some(voice) = &self.voice {
            let mut runtime_settings = settings.runtime(CONFIG.llm_settings().as_ref());
            if !self.desktop_pet_visible {
                runtime_settings.mode = VoiceMode::Off;
            }
            self.voice_revision = voice.configure(runtime_settings);
            self.voice_activity = VoiceActivitySnapshot::default();
        } else {
            self.voice_revision = 0;
            self.voice_activity = VoiceActivitySnapshot::default();
        }
        self.chat.update(cx, |chat, cx| {
            chat.cancel_pending_voice();
            chat.set_voice_indicator_visible(false, cx);
        });
        if let Some(playback) = &self.speech_playback {
            playback.stop();
        }
        self.sync_voice_level_task(cx);
        cx.notify();
    }

    pub(super) fn handle_voice_event(&mut self, event: VoiceEvent, cx: &mut Context<Self>) -> bool {
        match event {
            VoiceEvent::ActivityChanged { revision } if revision == self.voice_revision => {
                if let Some(voice) = &self.voice {
                    self.voice_activity = voice.activity();
                    let recording = matches!(
                        self.voice_activity.phase,
                        VoicePhase::Recording | VoicePhase::Transcribing
                    );
                    self.chat.update(cx, |chat, cx| {
                        chat.set_voice_indicator_visible(recording, cx);
                    });
                    self.sync_voice_level_task(cx);
                    cx.notify();
                }
            }
            VoiceEvent::SpeechStarted {
                revision,
                utterance_id,
            } if revision == self.voice_revision => {
                if let Some(playback) = &self.speech_playback {
                    playback.stop();
                }
                let language = self.appearance.borrow().language;
                self.chat.update(cx, |chat, cx| {
                    chat.voice_speech_started(utterance_id, language, cx);
                });
            }
            VoiceEvent::UtteranceCancelled {
                revision,
                utterance_id,
            } if revision == self.voice_revision => {
                self.chat.update(cx, |chat, _| {
                    chat.voice_utterance_cancelled(utterance_id);
                });
            }
            VoiceEvent::TranscriptReady {
                revision,
                utterance_id,
                text,
            } if revision == self.voice_revision => {
                self.chat.update(cx, |chat, cx| {
                    chat.send_voice_transcript(utterance_id, text, cx);
                });
            }
            VoiceEvent::TranscriptionRequested {
                revision,
                utterance_id,
                model_id,
                samples,
            } if revision == self.voice_revision => {
                if let Some(voice) = self.voice.clone() {
                    self.chat.update(cx, |chat, cx| {
                        chat.transcribe_voice(revision, utterance_id, model_id, samples, voice, cx);
                    });
                }
            }
            VoiceEvent::Error { revision, message } if revision == self.voice_revision => {
                self.chat.update(cx, |chat, cx| {
                    chat.voice_failed(message, cx);
                });
            }
            _ => {}
        }
        true
    }

    fn sync_voice_level_task(&mut self, cx: &mut Context<Self>) {
        if self.voice_activity.phase != VoicePhase::Recording {
            self.voice_level_task = None;
            return;
        }
        if self.voice_level_task.is_some() {
            return;
        }
        let Some(voice) = self.voice.clone() else {
            return;
        };
        let background = cx.background_executor().clone();
        self.voice_level_task = Some(cx.spawn(async move |this, cx| {
            loop {
                background.timer(VOICE_LEVEL_REFRESH_INTERVAL).await;
                let keep_running = this
                    .update(cx, |this, cx| {
                        this.voice_activity = voice.activity();
                        if this.voice_activity.phase != VoicePhase::Recording {
                            return false;
                        }
                        cx.notify();
                        true
                    })
                    .unwrap_or(false);
                if !keep_running {
                    break;
                }
            }
        }));
    }

    pub(in crate::ui) fn set_push_to_talk(&self, pressed: bool) {
        if self.voice_mode.supports_push_to_talk()
            && let Some(voice) = &self.voice
        {
            voice.set_push_to_talk(pressed);
        }
    }

    pub(super) fn stop_voice_interaction(&mut self, cx: &mut Context<Self>) {
        let utterance_id = self.chat.read(cx).pending_voice_utterance();
        if let (Some(voice), Some(utterance_id)) = (&self.voice, utterance_id) {
            voice.cancel_remote_transcription(self.voice_revision, utterance_id);
        }
        self.chat.update(cx, |chat, cx| {
            chat.stop_voice_interaction(cx);
        });
        if let Some(playback) = &self.speech_playback {
            playback.stop();
        }
        cx.notify();
    }

    pub(super) fn release_voice_shortcut(&mut self) {
        self.voice_shortcut_release_task = None;
        self.set_push_to_talk(false);
    }

    pub(super) fn set_voice_shortcut_pressed(&mut self, pressed: bool, cx: &mut Context<Self>) {
        self.release_voice_shortcut();
        if !pressed || !self.voice_mode.supports_push_to_talk() {
            return;
        }
        self.chat.update(cx, |chat, cx| {
            chat.voice_input_pressed(cx);
        });
        self.set_push_to_talk(true);
        let background = cx.background_executor().clone();
        self.voice_shortcut_release_task = Some(cx.spawn(async move |this, cx| {
            background.timer(VOICE_SHORTCUT_RELEASE_TIMEOUT).await;
            let _ = this.update(cx, |this, _| {
                log::warn!("event=voice_shortcut_release_timeout");
                this.release_voice_shortcut();
            });
        }));
    }
}
