//! Whether a dictation goes through the LLM, and the prompt it gets. The device builds a `Plan` from its settings,
//! history and dictionary; the device (BYOK) and the Cloud server both decide with `Plan::decide`, so the two modes
//! send the same prompt and skip the same dictations.
use crate::actions::{self, Action, Parsed};
use crate::llm::{self, Level};
use crate::rules::Rules;
use crate::store::Fix;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Plan {
    pub level: Level,
    /// Local rules run on the recognized text before the LLM (all off at `Level::Raw`).
    pub rules: Rules,
    /// Shorter texts skip the LLM.
    pub min_chars: usize,
    /// The level's prompt with the active styles.
    pub system: String,
    /// The user message with every placeholder filled but `{{text}}`.
    pub user: String,
    /// Voice-command names and aliases (the commands themselves stay on the device) and the wake word: a command
    /// is never refined.
    #[serde(default)]
    pub commands: Vec<String>,
    #[serde(default)]
    pub wake: String,
    pub timeout_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Skip {
    /// The level runs no LLM.
    Off,
    /// Under `min_chars`.
    Short,
    /// A voice command, not dictation.
    Command,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Decision {
    /// The recognized text after the local rules: what is pasted unless the LLM rewrites it.
    pub text: String,
    /// The user message for the LLM, or why there is none.
    pub llm: Result<String, Skip>,
}

const TEXT: &str = "{{text}}";

impl Plan {
    pub fn decide(&self, asr: &str) -> Decision {
        let triggers: Vec<Action> = self
            .commands
            .iter()
            .map(|t| Action {
                name: t.clone(),
                run: "-".into(),
                ..Action::default()
            })
            .collect();
        if actions::parse(asr, &triggers, &self.wake) != Parsed::Dictation {
            return Decision {
                text: asr.to_string(),
                llm: Err(Skip::Command),
            };
        }
        let text = self.rules.apply(asr);
        let llm = if self.level <= Level::Off {
            Err(Skip::Off)
        } else if crate::records::chars(&text) < self.min_chars {
            Err(Skip::Short)
        } else {
            Ok(self.user_with(&text))
        };
        Decision { text, llm }
    }

    /// The user message for `text`.
    pub fn user_with(&self, text: &str) -> String {
        self.user.replacen(TEXT, text, 1)
    }

    /// The request to a BYOK endpoint for the user message from `decide`.
    pub fn request(
        &self,
        ep: &llm::Endpoint,
        key: Option<&str>,
        user: &str,
        extra: Option<&str>,
    ) -> Result<llm::Request, String> {
        let timeout = std::time::Duration::from_millis(self.timeout_ms);
        llm::request(ep, key, &self.system, user, extra, timeout)
    }
}

/// What the prompt is filled with besides the settings and the text.
pub struct Fill<'a> {
    /// What the model is told the current app is (app — window, or empty).
    pub app: &'a str,
    /// The front app's bare name, for the styles.
    pub front: Option<&'a str>,
    pub history: &'a [String],
    pub hotwords: &'a str,
    pub fixes: &'a [Fix],
}

/// System prompt and user message (without the text) from llmPrompt / llmPromptLight, llmStyles, llmTemplate and
/// llmContext; the 10 newest corrections.
pub fn prompt(
    mut setting: impl FnMut(&str) -> Option<String>,
    light: bool,
    f: &Fill,
) -> (String, String) {
    let system = if light {
        setting("llmPromptLight").unwrap_or_else(|| llm::LIGHT_PROMPT.into())
    } else {
        setting("llmPrompt").unwrap_or_else(|| llm::DEFAULT_PROMPT.into())
    };
    let system = llm::with_styles(&system, &llm::active_styles(&mut setting, f.front));
    let template = setting("llmTemplate").unwrap_or_else(|| llm::DEFAULT_TEMPLATE.into());
    let corrections: Vec<String> = f
        .fixes
        .iter()
        .take(10)
        .map(|x| format!("{} → {}", x.from, x.to))
        .collect();
    let context = setting("llmContext").unwrap_or_default();
    let user = llm::render(
        &template,
        &[
            ("context", &context),
            ("hotwords", f.hotwords),
            ("app", f.app),
            ("history", &f.history.join("\n")),
            ("corrections", &corrections.join("; ")),
        ],
    );
    (system, user)
}

/// A plan for text that is already past the local rules and always goes to the LLM (重新润色, learning).
pub fn direct(
    setting: impl FnMut(&str) -> Option<String>,
    light: bool,
    f: &Fill,
    timeout_ms: u64,
) -> Plan {
    let (system, user) = prompt(setting, light, f);
    Plan {
        level: if light { Level::Light } else { Level::Fluent },
        rules: Rules::default(),
        min_chars: 0,
        system,
        user,
        commands: vec![],
        wake: String::new(),
        timeout_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(level: Level, min_chars: usize) -> Plan {
        Plan {
            level,
            rules: Rules {
                fillers: true,
                ..Rules::default()
            },
            min_chars,
            system: "S".into(),
            user: "U: {{text}}".into(),
            commands: vec!["打开".into()],
            wake: "指令".into(),
            timeout_ms: 4000,
        }
    }

    #[test]
    fn decides_like_the_controller() {
        let d = plan(Level::Fluent, 3).decide("嗯，今天开会");
        assert_eq!(d.text, "今天开会");
        assert_eq!(d.llm, Ok("U: 今天开会".into()));
        assert_eq!(
            plan(Level::Fluent, 10).decide("今天开会").llm,
            Err(Skip::Short)
        );
        // English counts words, as the stats do: 4 words is under 5 though it is 22 characters.
        assert_eq!(
            plan(Level::Fluent, 5).decide("ship the release today").llm,
            Err(Skip::Short)
        );
        assert!(
            plan(Level::Fluent, 4)
                .decide("ship the release today")
                .llm
                .is_ok()
        );
        assert_eq!(plan(Level::Off, 0).decide("今天开会").llm, Err(Skip::Off));
        let c = plan(Level::Fluent, 0).decide("指令，打开设置");
        assert_eq!(c.llm, Err(Skip::Command));
        assert_eq!(c.text, "指令，打开设置");
        // The wake word alone (no matching command) is still not dictation.
        assert_eq!(
            plan(Level::Fluent, 0).decide("指令，关掉").llm,
            Err(Skip::Command)
        );
    }

    #[test]
    fn the_text_fills_only_the_text_placeholder() {
        let mut p = plan(Level::Fluent, 0);
        p.rules = Rules::default();
        assert_eq!(p.decide("a {{text}} b").llm, Ok("U: a {{text}} b".into()));
    }

    #[test]
    fn round_trips_as_json() {
        let p = plan(Level::Light, 20);
        let j = serde_json::to_string(&p).unwrap();
        assert!(j.contains(r#""level":"light""#), "{j}");
        assert_eq!(serde_json::from_str::<Plan>(&j).unwrap(), p);
    }

    #[test]
    fn prompt_appends_styles_for_the_front_app() {
        let json = llm::format_styles(&[llm::Style {
            name: "精炼".into(),
            prompt: "短".into(),
            apps: vec!["Rex".into()],
            ..llm::Style::default()
        }]);
        let set = |k: &str| (k == "llmStyles").then(|| json.clone());
        let system = |front, light| {
            let f = Fill {
                app: "",
                front,
                history: &[],
                hotwords: "",
                fixes: &[],
            };
            prompt(set, light, &f).0
        };
        assert!(
            system(Some("Rex"), false).starts_with(llm::DEFAULT_PROMPT)
                && system(Some("Rex"), false).contains("- 精炼：短")
        );
        assert!(
            system(Some("Rex"), true).starts_with(llm::LIGHT_PROMPT)
                && system(Some("Rex"), true).contains("- 精炼：短")
        );
        assert_eq!(system(Some("Mail"), false), llm::DEFAULT_PROMPT);
        assert_eq!(system(None, false), llm::DEFAULT_PROMPT);
    }

    #[test]
    fn user_message_keeps_the_text_placeholder_last() {
        let f = Fill {
            app: "Mail — Inbox",
            front: Some("Mail"),
            history: &["上一句".into()],
            hotwords: "Plaid",
            fixes: &[],
        };
        let (_, user) = prompt(|_: &str| None, false, &f);
        assert!(user.contains("词表：Plaid") && user.contains("当前应用：Mail — Inbox"));
        assert!(user.contains("用户以往的手动更正（识别结果 → 正确写法）：(none)"));
        assert!(user.ends_with("待优化文本：\n{{text}}"), "{user}");
    }
}
