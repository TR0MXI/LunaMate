use std::{
    future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use futures::{StreamExt as _, channel::mpsc};
use genai::chat::ToolCall;
use rust_i18n::t;
use tokio::time::Instant;

use crate::{
    config::AppLanguage,
    media::prepare_dynamic_image,
    memory::AssistantTrace,
    provider::*,
    session::{ChatContextMessage, ChatRole},
    tools::OutfitOption,
};

use super::outfit;

struct FakeScreenshotCapability {
    authorized: Arc<AtomicBool>,
    captures: Arc<AtomicUsize>,
    revoke_after_capture: bool,
}

impl ScreenshotCapability for FakeScreenshotCapability {
    fn is_authorized(&self) -> bool {
        self.authorized.load(Ordering::Acquire)
    }

    fn wait_for_revocation(&self) -> Pin<Box<dyn future::Future<Output = ()> + Send + 'static>> {
        Box::pin(future::pending())
    }

    fn capture(
        &self,
    ) -> Pin<
        Box<
            dyn future::Future<
                    Output = Result<crate::media::ImageAttachment, crate::media::ImageInputError>,
                > + Send
                + 'static,
        >,
    > {
        let authorized = Arc::clone(&self.authorized);
        let captures = Arc::clone(&self.captures);
        let revoke_after_capture = self.revoke_after_capture;
        Box::pin(async move {
            captures.fetch_add(1, Ordering::AcqRel);
            let image = prepare_dynamic_image(
                image::DynamicImage::new_rgb8(4, 4),
                "screenshot.jpg".to_owned(),
            );
            if revoke_after_capture {
                authorized.store(false, Ordering::Release);
            }
            image
        })
    }
}

#[test]
fn screenshot_tool_is_registered_only_when_permission_is_enabled() {
    let disabled = build_request(String::new(), Vec::new(), false, &[], AppLanguage::English);
    assert!(disabled.tools.is_none());

    let enabled = build_request(String::new(), Vec::new(), true, &[], AppLanguage::English);
    let tools = enabled.tools.expect("开启权限后应当注册截屏工具");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name.as_str(), SCREEN_CAPTURE_TOOL);
}

#[test]
fn screenshot_tool_description_uses_the_explicit_application_language() {
    for language in [
        AppLanguage::SimplifiedChinese,
        AppLanguage::TraditionalChinese,
        AppLanguage::English,
        AppLanguage::Japanese,
    ] {
        let request = build_request(String::new(), Vec::new(), true, &[], language);
        let tools = request.tools.expect("开启权限后应当注册截屏工具");
        assert_eq!(
            tools[0].description.as_deref(),
            Some(
                t!(
                    "chat.tool.screen_capture_description",
                    locale = language.id()
                )
                .as_ref()
            )
        );
    }
}

#[tokio::test]
async fn screenshot_capability_is_checked_before_and_after_capture() {
    let call = || ToolCall {
        call_id: "capture".to_owned(),
        fn_name: SCREEN_CAPTURE_TOOL.to_owned(),
        fn_arguments: serde_json::json!({}),
        thought_signatures: None,
    };

    let authorized = Arc::new(AtomicBool::new(false));
    let captures = Arc::new(AtomicUsize::new(0));
    let capability = Arc::new(FakeScreenshotCapability {
        authorized,
        captures: Arc::clone(&captures),
        revoke_after_capture: false,
    });
    let (result, image) = execute_screenshot_tool_for_test(call(), Some(capability)).await;
    assert_eq!(
        result,
        serde_json::json!({"status": "error", "code": "permission_disabled"})
    );
    assert!(!image);
    assert_eq!(captures.load(Ordering::Acquire), 0);

    let authorized = Arc::new(AtomicBool::new(true));
    let captures = Arc::new(AtomicUsize::new(0));
    let capability = Arc::new(FakeScreenshotCapability {
        authorized: Arc::clone(&authorized),
        captures: Arc::clone(&captures),
        revoke_after_capture: true,
    });
    let (result, image) = execute_screenshot_tool_for_test(call(), Some(capability)).await;
    assert_eq!(
        result,
        serde_json::json!({"status": "error", "code": "permission_revoked"})
    );
    assert!(!image);
    assert!(!authorized.load(Ordering::Acquire));
    assert_eq!(captures.load(Ordering::Acquire), 1);
}

#[test]
fn outfit_tool_is_registered_only_when_the_model_has_an_extra_outfit() {
    let default_only = vec![outfit("default", "Default outfit")];
    let request = build_request(
        String::new(),
        Vec::new(),
        false,
        &default_only,
        AppLanguage::English,
    );
    assert!(request.tools.is_none());

    let outfits = vec![
        outfit("default", "Default outfit"),
        outfit("detective", "Detective"),
    ];
    let request = build_request(
        String::new(),
        Vec::new(),
        false,
        &outfits,
        AppLanguage::English,
    );
    let tools = request.tools.expect("存在额外服装时应当注册换装工具");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name.as_str(), CHANGE_OUTFIT_TOOL);
    assert_eq!(
        tools[0]
            .schema
            .as_ref()
            .expect("换装工具必须提供参数 schema")["properties"]["outfit"]["enum"],
        serde_json::json!(["Default outfit", "Detective"])
    );

    let outfits = vec![
        outfit("default", "Default outfit"),
        outfit("detective", "Detective"),
    ];
    let request = build_request(
        String::new(),
        Vec::new(),
        true,
        &outfits,
        AppLanguage::English,
    );
    assert_eq!(request.tools.expect("两个可用工具都应注册").len(), 2);
}

#[test]
fn outfit_tool_text_uses_the_explicit_application_language() {
    let outfits = vec![
        outfit("default", "default"),
        outfit("alternate", "alternate"),
    ];
    for language in [
        AppLanguage::SimplifiedChinese,
        AppLanguage::TraditionalChinese,
        AppLanguage::English,
        AppLanguage::Japanese,
    ] {
        let request = build_request(String::new(), Vec::new(), false, &outfits, language);
        let tool = request
            .tools
            .expect("存在额外服装时应当注册换装工具")
            .into_iter()
            .find(|tool| tool.name.as_str() == CHANGE_OUTFIT_TOOL)
            .expect("请求中应当包含换装工具");
        assert_eq!(
            tool.description.as_deref(),
            Some(
                t!(
                    "chat.tool.change_outfit_description",
                    locale = language.id()
                )
                .as_ref()
            )
        );
        assert_eq!(
            tool.schema.as_ref().expect("换装工具必须提供参数 schema")["properties"]["outfit"]["description"],
            t!("chat.tool.change_outfit_argument", locale = language.id()).as_ref()
        );
    }
}

#[test]
fn outfit_tool_rejects_unknown_or_malformed_choices() {
    let outfits = vec![
        outfit("default", "Default outfit"),
        outfit("detective-id", "Detective"),
    ];
    assert_eq!(
        outfit_argument(&serde_json::json!({"outfit": "Detective"}), &outfits)
            .map(OutfitOption::id),
        Ok("detective-id")
    );
    assert_eq!(
        outfit_argument(&serde_json::json!({"outfit": "Missing"}), &outfits),
        Err("outfit_unavailable")
    );
    assert_eq!(
        outfit_argument(
            &serde_json::json!({"outfit": "Detective", "extra": true}),
            &outfits
        ),
        Err("invalid_arguments")
    );
}

#[test]
fn failed_capture_handoff_tells_the_model_to_continue_without_the_image() {
    let mut request = build_request(
        String::new(),
        vec![ChatContextMessage {
            source_message_id: None,
            role: ChatRole::User,
            content: "inspect my screen".to_owned(),
            image: None,
        }],
        true,
        &[],
        AppLanguage::English,
    );

    append_stateless_capture_result(&mut request, None, AppLanguage::English);

    assert_eq!(request.messages.len(), 1);
    assert!(request.messages[0].content.binaries().is_empty());
    assert!(request.messages[0].content.texts().iter().any(|text| {
        text.contains(
            t!(
                "chat.tool.screen_capture_unavailable",
                locale = AppLanguage::English.id()
            )
            .as_ref(),
        )
    }));
}

#[test]
fn capture_handoff_prompts_use_the_explicit_application_language() {
    let image = prepare_dynamic_image(
        image::DynamicImage::new_rgb8(10, 6),
        "screenshot.jpg".to_owned(),
    )
    .expect("测试截图应当可以规范化");
    for language in [
        AppLanguage::SimplifiedChinese,
        AppLanguage::TraditionalChinese,
        AppLanguage::English,
        AppLanguage::Japanese,
    ] {
        let mut request = build_request(String::new(), Vec::new(), true, &[], language);
        append_stateless_capture_result(&mut request, None, language);
        let expected = t!(
            "chat.tool.screen_capture_unavailable",
            locale = language.id()
        );

        assert!(
            request.messages[0]
                .content
                .texts()
                .iter()
                .any(|text| text.contains(expected.as_ref()))
        );

        let mut request = build_request(String::new(), Vec::new(), true, &[], language);
        append_stateless_capture_result(&mut request, Some(&image), language);
        let expected = t!("chat.tool.screen_capture_handoff", locale = language.id());
        assert!(
            request.messages[0]
                .content
                .texts()
                .iter()
                .any(|text| text.contains(expected.as_ref()))
        );
    }
}

#[test]
fn capture_handoff_creates_a_user_turn_when_the_request_has_no_messages() {
    let mut request = build_request(String::new(), Vec::new(), true, &[], AppLanguage::English);

    append_stateless_capture_result(&mut request, None, AppLanguage::English);

    assert_eq!(request.messages.len(), 1);
    assert!(!request.messages[0].content.texts().is_empty());
}

#[test]
fn signed_tool_handoff_retries_from_original_user_message() {
    let image = prepare_dynamic_image(
        image::DynamicImage::new_rgb8(10, 6),
        "screenshot.jpg".to_owned(),
    )
    .expect("测试截图应当可以规范化");
    let mut request = build_request(
        String::new(),
        vec![ChatContextMessage {
            source_message_id: None,
            role: ChatRole::User,
            content: "inspect my screen".to_owned(),
            image: None,
        }],
        true,
        &[],
        AppLanguage::English,
    );

    append_stateless_capture_result(&mut request, Some(&image), AppLanguage::English);

    assert_eq!(request.messages.len(), 1);
    assert_eq!(request.messages[0].content.binaries().len(), 1);
    assert!(
        request.messages[0]
            .content
            .texts()
            .iter()
            .any(|text| text.contains("capture_screen"))
    );
}

#[tokio::test]
async fn outfit_tool_waits_for_the_desktop_pet_to_apply_the_change() {
    let (sender, mut receiver) = mpsc::channel(1);
    let task = tokio::spawn(async move {
        let mut sender = sender;
        request_outfit_change(
            "outfit-detective".to_owned(),
            42,
            Instant::now() + Duration::from_secs(1),
            &mut sender,
        )
        .await
    });

    let Some(ChatStreamEvent::ChangeOutfit(request)) = receiver.next().await else {
        panic!("换装工具应当向桌宠视图发送语义请求");
    };
    assert_eq!(request.outfit_id(), "outfit-detective");
    assert_eq!(request.revision(), 42);
    request.complete(true);

    assert_eq!(task.await.expect("换装工具任务不应 panic"), Ok(()));
}

#[tokio::test]
async fn local_tool_trace_keeps_arguments_and_sanitized_result_only() {
    let call = ToolCall {
        call_id: "private-call-id".to_owned(),
        fn_name: SCREEN_CAPTURE_TOOL.to_owned(),
        fn_arguments: serde_json::json!({}),
        thought_signatures: Some(vec!["private-thought-signature".to_owned()]),
    };

    let executions = execute_tool_traces_for_test(&[call]).await;

    assert_eq!(executions.len(), 1);
    assert_eq!(executions[0].name(), SCREEN_CAPTURE_TOOL);
    assert_eq!(executions[0].arguments(), &serde_json::json!({}));
    assert_eq!(
        executions[0].result(),
        &serde_json::json!({"status": "error", "code": "permission_disabled"})
    );
    let encoded =
        serde_json::to_string(&AssistantTrace::new(None, executions)).expect("工具详情应可序列化");
    assert!(!encoded.contains("private-call-id"));
    assert!(!encoded.contains("private-thought-signature"));
}
