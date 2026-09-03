use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};

use crate::canonical_text::is_canonical_text_within;
use crate::{ComponentVersion, DomainError, ProvenanceId};

const MAX_VOCABULARY_CONCEPTS: usize = 256;
const MAX_CONCEPT_ALIASES: usize = 64;
const MAX_CONCEPT_BYTES: usize = 128;
const MAX_ALIAS_BYTES: usize = 512;
const MAX_PROJECTION_TEXT_BYTES: usize = 64 * 1024;

/// Provenance for the authority that supplied one canonical vocabulary.
///
/// Model-derived authorities must identify both the model and prompt
/// revisions. Maintained authorities remain explicit rather than fabricating
/// model provenance for a human- or application-curated vocabulary.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum FactCanonicalVocabularyProvenanceV1 {
    Maintained {
        provenance_id: ProvenanceId,
    },
    Model {
        provenance_id: ProvenanceId,
        model_revision: ComponentVersion,
        prompt_revision: ComponentVersion,
    },
}

/// One canonical concept and the phrases which project source text onto it.
#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq)]
pub struct FactCanonicalConceptV1 {
    canonical: String,
    aliases: Vec<String>,
}

impl FactCanonicalConceptV1 {
    pub fn new(canonical: String, aliases: Vec<String>) -> Result<Self, DomainError> {
        validate_concept(&canonical)?;
        if aliases.is_empty() || aliases.len() > MAX_CONCEPT_ALIASES {
            return Err(DomainError::NonCanonical {
                field: "fact canonical vocabulary aliases",
            });
        }
        let aliases = aliases
            .into_iter()
            .map(|alias| normalize_phrase(&alias))
            .collect::<Result<BTreeSet<_>, _>>()?
            .into_iter()
            .collect::<Vec<_>>();
        if aliases.is_empty() {
            return Err(DomainError::NonCanonical {
                field: "fact canonical vocabulary aliases",
            });
        }
        Ok(Self { canonical, aliases })
    }

    pub fn canonical(&self) -> &str {
        &self.canonical
    }

    pub fn aliases(&self) -> &[String] {
        &self.aliases
    }

    fn match_phrases(&self) -> impl Iterator<Item = String> + '_ {
        self.aliases
            .iter()
            .cloned()
            .chain(std::iter::once(self.canonical.replace('_', " ")))
    }
}

impl<'de> Deserialize<'de> for FactCanonicalConceptV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            canonical: String,
            aliases: Vec<String>,
        }

        let wire = Wire::deserialize(deserializer)?;
        Self::new(wire.canonical, wire.aliases).map_err(serde::de::Error::custom)
    }
}

/// A bounded caller-selected vocabulary used to derive a separate retrieval
/// projection. It never rewrites the authoritative fact payload.
#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq)]
pub struct FactCanonicalVocabularyV1 {
    revision: ComponentVersion,
    provenance: FactCanonicalVocabularyProvenanceV1,
    concepts: Vec<FactCanonicalConceptV1>,
}

impl FactCanonicalVocabularyV1 {
    pub fn new(
        revision: ComponentVersion,
        provenance: FactCanonicalVocabularyProvenanceV1,
        mut concepts: Vec<FactCanonicalConceptV1>,
    ) -> Result<Self, DomainError> {
        if concepts.is_empty() || concepts.len() > MAX_VOCABULARY_CONCEPTS {
            return Err(DomainError::NonCanonical {
                field: "fact canonical vocabulary concepts",
            });
        }
        concepts.sort_by(|left, right| left.canonical().cmp(right.canonical()));
        if concepts
            .windows(2)
            .any(|pair| pair[0].canonical() == pair[1].canonical())
        {
            return Err(DomainError::NonCanonical {
                field: "fact canonical vocabulary concepts",
            });
        }

        let mut aliases = BTreeMap::<String, &str>::new();
        for concept in &concepts {
            for phrase in concept.match_phrases() {
                let phrase = normalize_phrase(&phrase)?;
                if aliases
                    .insert(phrase, concept.canonical())
                    .is_some_and(|existing| existing != concept.canonical())
                {
                    return Err(DomainError::NonCanonical {
                        field: "fact canonical vocabulary aliases",
                    });
                }
            }
        }
        Ok(Self {
            revision,
            provenance,
            concepts,
        })
    }

    pub fn revision(&self) -> &ComponentVersion {
        &self.revision
    }

    pub fn provenance(&self) -> &FactCanonicalVocabularyProvenanceV1 {
        &self.provenance
    }

    pub fn concepts(&self) -> &[FactCanonicalConceptV1] {
        &self.concepts
    }

    pub fn project(&self, text: &str) -> Result<FactVocabularyProjectionV1, DomainError> {
        let text_tokens = projection_tokens(text)?;
        let concepts = self
            .concepts
            .iter()
            .filter(|concept| {
                concept.match_phrases().any(|phrase| {
                    let phrase_tokens = phrase_tokens(&phrase);
                    contains_phrase(&text_tokens, &phrase_tokens)
                })
            })
            .map(|concept| concept.canonical().to_owned())
            .collect();
        FactVocabularyProjectionV1::new(self.revision.clone(), self.provenance.clone(), concepts)
    }

    /// Return every alias belonging to concepts present in `text`. Candidate
    /// discovery uses this to find facts whose original wording differs.
    pub fn expanded_aliases(&self, text: &str) -> Result<Vec<String>, DomainError> {
        let projected = self.project(text)?;
        let projected = projected.concepts().iter().collect::<BTreeSet<_>>();
        Ok(self
            .concepts
            .iter()
            .filter(|concept| projected.contains(&concept.canonical))
            .flat_map(FactCanonicalConceptV1::match_phrases)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect())
    }
}

impl<'de> Deserialize<'de> for FactCanonicalVocabularyV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            revision: ComponentVersion,
            provenance: FactCanonicalVocabularyProvenanceV1,
            concepts: Vec<FactCanonicalConceptV1>,
        }

        let wire = Wire::deserialize(deserializer)?;
        Self::new(wire.revision, wire.provenance, wire.concepts).map_err(serde::de::Error::custom)
    }
}

/// Concepts derived for one text under an exact vocabulary authority.
#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq)]
pub struct FactVocabularyProjectionV1 {
    vocabulary_revision: ComponentVersion,
    provenance: FactCanonicalVocabularyProvenanceV1,
    concepts: Vec<String>,
}

impl FactVocabularyProjectionV1 {
    pub fn new(
        vocabulary_revision: ComponentVersion,
        provenance: FactCanonicalVocabularyProvenanceV1,
        concepts: Vec<String>,
    ) -> Result<Self, DomainError> {
        let concepts = concepts.into_iter().collect::<BTreeSet<_>>();
        for concept in &concepts {
            validate_concept(concept)?;
        }
        Ok(Self {
            vocabulary_revision,
            provenance,
            concepts: concepts.into_iter().collect(),
        })
    }

    pub fn vocabulary_revision(&self) -> &ComponentVersion {
        &self.vocabulary_revision
    }

    pub fn provenance(&self) -> &FactCanonicalVocabularyProvenanceV1 {
        &self.provenance
    }

    pub fn concepts(&self) -> &[String] {
        &self.concepts
    }
}

impl<'de> Deserialize<'de> for FactVocabularyProjectionV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            vocabulary_revision: ComponentVersion,
            provenance: FactCanonicalVocabularyProvenanceV1,
            concepts: Vec<String>,
        }

        let wire = Wire::deserialize(deserializer)?;
        Self::new(wire.vocabulary_revision, wire.provenance, wire.concepts)
            .map_err(serde::de::Error::custom)
    }
}

fn validate_concept(value: &str) -> Result<(), DomainError> {
    let canonical = !value.is_empty()
        && value.len() <= MAX_CONCEPT_BYTES
        && !value.starts_with('_')
        && !value.ends_with('_')
        && !value.contains("__")
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_');
    if !canonical {
        return Err(DomainError::NonCanonical {
            field: "fact canonical vocabulary concept",
        });
    }
    Ok(())
}

fn normalize_phrase(value: &str) -> Result<String, DomainError> {
    let phrase = phrase_tokens(value).join(" ");
    if phrase.is_empty() || phrase.len() > MAX_ALIAS_BYTES {
        return Err(DomainError::NonCanonical {
            field: "fact canonical vocabulary aliases",
        });
    }
    Ok(phrase)
}

fn projection_tokens(value: &str) -> Result<Vec<String>, DomainError> {
    if !is_canonical_text_within(value, MAX_PROJECTION_TEXT_BYTES) {
        return Err(DomainError::NonCanonical {
            field: "fact vocabulary projection text",
        });
    }
    Ok(phrase_tokens(value))
}

fn phrase_tokens(value: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    for character in value.chars() {
        if character.is_ascii_alphanumeric() {
            current.push(character.to_ascii_lowercase());
        } else if !current.is_empty() {
            let token = std::mem::take(&mut current);
            if !is_connector(&token) {
                tokens.push(token);
            }
        }
    }
    if !current.is_empty() && !is_connector(&current) {
        tokens.push(current);
    }
    tokens
}

fn is_connector(token: &str) -> bool {
    matches!(
        token,
        "a" | "an" | "the" | "is" | "was" | "were" | "be" | "because" | "due" | "to"
    )
}

fn contains_phrase(text: &[String], phrase: &[String]) -> bool {
    !phrase.is_empty() && text.windows(phrase.len()).any(|window| window == phrase)
}
