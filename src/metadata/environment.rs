use super::{Evidence, Field, paths};
use crate::platform::Platform;
use roxmltree::Document;
use serde::{
    Deserialize, Deserializer,
    de::{IgnoredAny, MapAccess, SeqAccess, Visitor, value::MapAccessDeserializer},
};
use std::{borrow::Cow, fmt};

#[derive(Default)]
pub(super) struct CurrentInput(Vec<String>);

impl CurrentInput {
    pub(super) fn conflict(&self, effective: Platform) -> Option<Evidence> {
        self.0
            .iter()
            .find_map(|fragment| environment_conflict(fragment, effective))
    }
}

fn environment_conflict(fragment: &str, effective: Platform) -> Option<Evidence> {
    let doc = Document::parse(fragment).ok()?;
    let context = doc.root_element();
    if !context.has_tag_name("environment_context") {
        return None;
    }
    let directories =
        if let Some(list) = context.children().find(|n| n.has_tag_name("environments")) {
            let mut local = None;
            for env in list.children().filter(|n| {
                n.has_tag_name("environment") && n.attribute("status") != Some("unavailable")
            }) {
                // Codex reserves "local" for the local executor. A remote primary also owns the top-level roots.
                match (env.attribute("id"), env.attribute("primary")) {
                    (Some("local"), Some("false")) => return None,
                    (_, Some("false")) => {}
                    (Some("local"), _) => local = Some(env),
                    _ => return None,
                }
            }
            local
        } else {
            Some(context)
        };
    let cwds = directories
        .into_iter()
        .flat_map(|node| node.children())
        .filter(|n| n.has_tag_name("cwd"))
        .map(|n| (n, Field::Cwd));
    // A delta can update just a secondary environment while still declaring the primary workspace roots.
    let roots = context
        .children()
        .filter(|n| n.has_tag_name("filesystem"))
        .flat_map(|fs| fs.children().filter(|n| n.has_tag_name("workspace_roots")))
        .flat_map(|roots| roots.children().filter(|n| n.has_tag_name("root")))
        .map(|n| (n, Field::WorkspaceRoot));
    cwds.chain(roots)
        .filter(|(node, _)| !node.children().any(|child| child.is_element()))
        .filter_map(|(node, field)| paths::classify(node.text()?, field))
        .find(|e| e.platform != effective)
}

#[derive(Deserialize)]
struct Item<'a> {
    #[serde(borrow, rename = "type")]
    kind: Option<Cow<'a, str>>,
    #[serde(borrow)]
    role: Option<Cow<'a, str>>,
    content: Option<Content>,
}

// Project input one item at a time, even when reading a compressed request.
// Only a complete environment fragment can survive the current user turn.
impl<'de> Deserialize<'de> for CurrentInput {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct InputVisitor;
        impl<'de> Visitor<'de> for InputVisitor {
            type Value = CurrentInput;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a Responses input string or list")
            }
            fn visit_str<E>(self, _: &str) -> Result<Self::Value, E> {
                Ok(CurrentInput::default())
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut items: A) -> Result<Self::Value, A::Error> {
                let mut current = Vec::new();
                let mut saw_prompt = false;
                while let Some(item) = items.next_element::<Item<'_>>()? {
                    match (
                        item.kind.as_deref().unwrap_or("message"),
                        item.role.as_deref(),
                    ) {
                        ("message", Some("user")) => {
                            let content = item.content.unwrap_or_default();
                            if !content.environments.is_empty() {
                                // Codex may bundle AGENTS.md and the environment in one context message.
                                current = content.environments;
                                saw_prompt = false;
                            } else if content.prompt {
                                if saw_prompt {
                                    current.clear();
                                }
                                saw_prompt = true;
                            }
                        }
                        ("message", Some("developer" | "system")) => {}
                        _ => {
                            current.clear();
                            saw_prompt = false;
                        }
                    }
                }
                Ok(CurrentInput(current))
            }
        }
        d.deserialize_any(InputVisitor)
    }
}

#[derive(Default)]
struct Content {
    environments: Vec<String>,
    prompt: bool,
}

impl Content {
    fn text(&mut self, text: &str) {
        let text = text.trim();
        if text.starts_with("<environment_context>") && text.ends_with("</environment_context>") {
            self.environments.push(text.to_owned());
        } else if !(text.starts_with("# AGENTS.md instructions")
            && text.ends_with("</INSTRUCTIONS>"))
        {
            self.prompt = true;
        }
    }
}

impl<'de> Deserialize<'de> for Content {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Part<'a> {
            #[serde(borrow, rename = "type")]
            kind: Option<Cow<'a, str>>,
            #[serde(borrow)]
            text: Option<Cow<'a, str>>,
        }
        struct ContentVisitor;
        impl<'de> Visitor<'de> for ContentVisitor {
            type Value = Content;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("message text or content parts")
            }
            fn visit_str<E>(self, text: &str) -> Result<Content, E> {
                let mut content = Content::default();
                content.text(text);
                Ok(content)
            }
            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Content, A::Error> {
                // Other Responses item types can carry structured content of their own.
                IgnoredAny::deserialize(MapAccessDeserializer::new(map))?;
                Ok(Content::default())
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut parts: A) -> Result<Content, A::Error> {
                let mut content = Content::default();
                while let Some(part) = parts.next_element::<Part<'_>>()? {
                    match (part.kind.as_deref(), part.text) {
                        (Some("input_text" | "text"), Some(text)) => content.text(&text),
                        _ => content.prompt = true,
                    }
                }
                Ok(content)
            }
        }
        d.deserialize_any(ContentVisitor)
    }
}
