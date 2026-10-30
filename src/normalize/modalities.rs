use crate::types::{Modalities, Modality, ModelInfo};

/// Raw directional modalities as deserialized from upstream JSON feeds.
#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
pub struct RawModalities {
    #[serde(default)]
    pub input: Vec<String>,
    #[serde(default)]
    pub output: Vec<String>,
}

/// Returns `true` if the model ID or family belongs to the dedicated `gpt-image*` endpoint family.
pub fn is_gpt_image_family(model_id: &str, family: Option<&str>) -> bool {
    let lower_id = model_id.to_ascii_lowercase();
    let lower_id_basename = lower_id.rsplit('/').next().unwrap_or(lower_id.as_str());

    if lower_id_basename.starts_with("gpt-image") {
        return true;
    }

    if let Some(fam) = family {
        if fam.trim().to_ascii_lowercase().starts_with("gpt-image") {
            return true;
        }
    }

    false
}

/// Sanitizes raw directional modalities according to `@cline/llms` rules:
/// 1. Filters `input` and `output` strictly to `{Text, Image, Audio, Video, Pdf}` (deduplicated, order-preserving).
/// 2. If either `input` or `output` is empty on a partial declaration, discards the incomplete
///    declaration (`None`) so chat models aren't broken.
/// 3. Applies the `gpt-image*` family workaround: strips `Modality::Text` from `modalities.output`
///    for dedicated `gpt-image*` models so they aren't misrouted as text chat models.
pub fn sanitize_modalities(
    raw: Option<&RawModalities>,
    model_id: &str,
    family: Option<&str>,
) -> Option<Modalities> {
    let raw = raw?;

    let mut input = Vec::new();
    for item in &raw.input {
        if let Some(m) = Modality::from_raw(item) {
            if !input.contains(&m) {
                input.push(m);
            }
        }
    }

    let mut output = Vec::new();
    for item in &raw.output {
        if let Some(m) = Modality::from_raw(item) {
            if !output.contains(&m) {
                output.push(m);
            }
        }
    }

    // Discard incomplete modality declarations before applying model-specific output filters.
    if input.is_empty() || output.is_empty() {
        return None;
    }

    // Apply gpt-image* workaround: strip Text from output modalities.
    if is_gpt_image_family(model_id, family) {
        output.retain(|&m| m != Modality::Text);
    }

    Some(Modalities { input, output })
}

impl ModelInfo {
    /// Returns `true` if `modalities` is `None` OR both `modalities.input` and `modalities.output`
    /// contain `Modality::Text`.
    pub fn is_chat_model(&self) -> bool {
        match &self.modalities {
            None => true,
            Some(m) => m.input.contains(&Modality::Text) && m.output.contains(&Modality::Text),
        }
    }
}
