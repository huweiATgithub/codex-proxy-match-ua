mod environment;
mod paths;

use crate::platform::{Platform, Rejection};
use environment::CurrentInput;
use gateway_plugin_sdk::{call::middleware::outbound::Target, client::OutboundRequest};
use serde::{Deserialize, de::IgnoredAny};
use serde_json::value::RawValue;
use std::{collections::BTreeMap, fmt, io::BufReader};

const TURN_METADATA: &str = "x-codex-turn-metadata";

#[derive(Clone, Copy, Debug)]
pub enum TurnSource {
    Header,
    Body(&'static str),
    ClientMetadata(&'static str),
}

impl fmt::Display for TurnSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Header => write!(f, "header.{TURN_METADATA}"),
            Self::Body(key) => write!(f, "body.{key}"),
            Self::ClientMetadata(key) => write!(f, "body.client_metadata.{key}"),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Field {
    Sandbox(TurnSource),
    Workspaces(TurnSource),
    Cwd,
    WorkspaceRoot,
}

impl fmt::Display for Field {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sandbox(source) => write!(f, "{source}.sandbox"),
            Self::Workspaces(source) => write!(f, "{source}.workspaces"),
            Self::Cwd => f.write_str("environment_context.cwd"),
            Self::WorkspaceRoot => f.write_str("environment_context.filesystem.workspace_roots"),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Evidence {
    pub field: Field,
    pub pattern: &'static str,
    pub platform: Platform,
}

fn turn_conflict(json: &[u8], source: TurnSource, effective: Platform) -> Option<Evidence> {
    #[derive(Deserialize)]
    struct Turn<'a> {
        #[serde(borrow)]
        sandbox: Option<&'a RawValue>,
        #[serde(borrow)]
        workspaces: Option<&'a RawValue>,
    }

    let turn: Turn<'_> = serde_json::from_slice(json).ok()?;
    if let Some(backend) = turn
        .sandbox
        .and_then(|s| serde_json::from_str::<String>(s.get()).ok())
    {
        for (pattern, platform) in [
            ("seccomp", Platform::Linux),
            ("seatbelt", Platform::MacOS),
            ("windows_sandbox", Platform::Windows),
            ("windows_elevated", Platform::Windows),
            ("windows_mxc", Platform::Windows),
        ] {
            if backend == pattern && platform != effective {
                return Some(Evidence {
                    field: Field::Sandbox(source),
                    pattern,
                    platform,
                });
            }
        }
    }
    let roots: BTreeMap<String, IgnoredAny> = serde_json::from_str(turn.workspaces?.get()).ok()?;
    roots
        .keys()
        .filter_map(|root| paths::classify(root, Field::Workspaces(source)))
        .find(|e| e.platform != effective)
}

#[derive(Deserialize)]
struct MetadataFields<I> {
    client_metadata: Option<Box<RawValue>>,
    #[serde(rename = "x-codex-turn-metadata")]
    codex: Option<Box<RawValue>>,
    #[serde(rename = "turnMetadata")]
    camel: Option<Box<RawValue>>,
    turn_metadata: Option<Box<RawValue>>,
    input: Option<I>,
}

impl<I> MetadataFields<I> {
    fn conflict(
        &self,
        source: fn(&'static str) -> TurnSource,
        effective: Platform,
    ) -> Option<Evidence> {
        [
            (TURN_METADATA, &self.codex),
            ("turnMetadata", &self.camel),
            ("turn_metadata", &self.turn_metadata),
        ]
        .into_iter()
        .find_map(|(key, value)| {
            let json = value.as_deref()?.get();
            if json.starts_with('"') {
                let decoded: String = serde_json::from_str(json).ok()?;
                turn_conflict(decoded.as_bytes(), source(key), effective)
            } else {
                turn_conflict(json.as_bytes(), source(key), effective)
            }
        })
    }

    fn turn_conflict(&self, effective: Platform) -> Option<Evidence> {
        self.conflict(TurnSource::Body, effective).or_else(|| {
            let client: MetadataFields<IgnoredAny> =
                serde_json::from_str(self.client_metadata.as_deref()?.get()).ok()?;
            client.conflict(TurnSource::ClientMetadata, effective)
        })
    }
}

fn body_fields<I: serde::de::DeserializeOwned>(
    request: &OutboundRequest,
) -> Option<MetadataFields<I>> {
    if matches!(request.head.target, Target::Http { .. })
        && let Some(encoding) = request
            .head
            .headers
            .iter()
            .find(|h| h.name.eq_ignore_ascii_case("content-encoding"))
    {
        let encoding = encoding.value.trim_ascii();
        if encoding.eq_ignore_ascii_case(b"zstd") {
            let decoded = zstd::stream::read::Decoder::new(request.body.as_slice()).ok()?;
            return serde_json::from_reader(BufReader::new(decoded)).ok();
        }
        if !encoding.eq_ignore_ascii_case(b"identity") {
            return None;
        }
    }
    serde_json::from_slice(&request.body).ok()
}

pub fn check(request: &OutboundRequest, effective: Platform) -> Result<(), Rejection> {
    let conflict = request
        .head
        .headers
        .iter()
        .filter(|h| h.name.eq_ignore_ascii_case(TURN_METADATA))
        .find_map(|h| turn_conflict(&h.value, TurnSource::Header, effective))
        .or_else(|| {
            if let Some(body) = body_fields::<CurrentInput>(request) {
                body.turn_conflict(effective)
                    .or_else(|| body.input?.conflict(effective))
            } else {
                // An unfamiliar input item must not hide an independently usable turn declaration.
                body_fields::<IgnoredAny>(request)?.turn_conflict(effective)
            }
        });
    match conflict {
        Some(evidence) => Err(Rejection::Metadata {
            evidence,
            effective,
        }),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests;
