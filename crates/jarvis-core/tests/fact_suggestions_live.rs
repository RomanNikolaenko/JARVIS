//! Fact suggestions from a real local model, through the whole core: the
//! start-up sweep, the request, the parser and the duplicate filter. Ignored
//! by default, since it needs a model server. Start an OpenAI-compatible
//! server on the LM Studio port (for example
//! `llama-server -m <gguf> --alias <id> --host 127.0.0.1 --port 1234 --jinja`)
//! and run:
//!
//! ```text
//! JARVIS_LIVE_MODEL=<id> JARVIS_LIVE_RUNS=5 cargo test -p jarvis-core \
//!     --test fact_suggestions_live -- --ignored --nocapture
//! ```
//!
//! Each run prints the suggestions for the same conversation (from the live
//! check of 2026-11-03: six lasting facts, one one-off state, and the replies
//! Qwen2.5 3B gave). Judging them is up to the reader.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::print_stdout)]

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use jarvis_core::jarvis_memory::{MessageRole, MessageStatus, NewMessage};
use jarvis_core::{AppPaths, CoreOptions, JarvisCore};
use uuid::Uuid;

/// The default provider at 127.0.0.1:1234/v1.
const PROVIDER: &str = "lm-studio";

const TURNS: [(&str, &str); 4] = [
    (
        "Привіт! Мене звати Остап, я працюю вчителем історії у Львові.",
        "Привет, Остап! Хочешь, чтобы я помог с информацией о Львове или исторических фактах для уроков?",
    ),
    (
        "У мене є кіт Мурчик, і я не їм мʼяса.",
        "Я розумію, що мʼясо не є єдиною робочою для кошіків. Вони можуть потребувати і інші речі, таких \
         як травянике, корм з кошачого пиття, і гриби. Переконайтеся, що Мурчик має достатнього пиття і \
         достатнього здоров’я.",
    ),
    (
        "Я зараз дуже голодний. Порадь, що швидко приготувати на вечерю.",
        "Добре, Остап, тут пару швидких рецепцій для вечернього пиття: зразок з капустою та мясом. Якщо у \
         вас є капуста, бульон і мясце, можна приготувати зразок з капустою і мясом.",
    ),
    (
        "А ще я граю на гітарі.",
        "Це великолепно, Остап! Якщо ви хочете, можемо обговорити можливі способи ідеально приготувати \
         питомому для гри на гітарі.",
    ),
];

async fn open(dir: &Path) -> Arc<JarvisCore> {
    let paths = AppPaths {
        config_dir: dir.join("config"),
        data_dir: dir.join("data"),
        log_dir: dir.join("logs"),
    };
    JarvisCore::initialize(
        CoreOptions::new(paths, "9.9.9").with_fact_suggestion_pause(Duration::from_millis(50)),
    )
    .await
    .unwrap()
}

fn message(role: MessageRole, content: &str, model: &str) -> NewMessage {
    NewMessage {
        id: Uuid::new_v4(),
        role,
        content: content.to_owned(),
        status: MessageStatus::Complete,
        provider_id: (role == MessageRole::Assistant).then(|| PROVIDER.to_owned()),
        model: (role == MessageRole::Assistant).then(|| model.to_owned()),
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a model server on 127.0.0.1:1234 and JARVIS_LIVE_MODEL"]
async fn suggestions_from_a_live_model() {
    let model = std::env::var("JARVIS_LIVE_MODEL").expect("JARVIS_LIVE_MODEL");
    let runs: usize = std::env::var("JARVIS_LIVE_RUNS")
        .ok()
        .and_then(|runs| runs.parse().ok())
        .unwrap_or(3);
    for run in 1..=runs {
        let dir = tempfile::tempdir().unwrap();
        let core = open(dir.path()).await;
        let mut settings = core.settings();
        settings.memory.suggest_facts = true;
        core.update_settings(settings).await.unwrap();
        let conversation = core.create_conversation(None, false).await.unwrap();
        for (user, reply) in TURNS {
            for (role, text) in [(MessageRole::User, user), (MessageRole::Assistant, reply)] {
                core.memory()
                    .append_message(conversation.id, message(role, text, &model))
                    .await
                    .unwrap();
            }
        }
        core.shutdown().await;
        drop(core);

        // The start-up sweep finds the unchecked conversation.
        let core = open(dir.path()).await;
        let started = Instant::now();
        while !core
            .memory()
            .unchecked_messages(conversation.id)
            .await
            .unwrap()
            .is_empty()
        {
            assert!(
                started.elapsed() < Duration::from_secs(300),
                "the check did not finish"
            );
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        let suggestions: Vec<String> = core
            .list_fact_suggestions()
            .await
            .unwrap()
            .into_iter()
            .map(|s| s.content)
            .collect();
        println!(
            "run {run} ({:.0} s): {suggestions:?}",
            started.elapsed().as_secs_f32()
        );
        core.shutdown().await;
    }
}
