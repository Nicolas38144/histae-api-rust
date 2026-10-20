use super::Language;
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use uuid::Uuid;

pub type Translations = BTreeMap<Language, String>;
pub type TranslationPatch = BTreeMap<Language, Option<String>>;

#[derive(Clone, Debug, Default)]
pub struct Entry {
    pub base: String,
    pub names: Translations,
    pub descriptions: Translations,
}
impl Entry {
    pub fn name(&self, language: Language) -> &str {
        self.names
            .get(&language)
            .or_else(|| self.names.get(&Language::Fr))
            .map_or(self.base.as_str(), String::as_str)
    }
}

#[derive(Clone, Debug, Default)]
pub struct CatalogSnapshot {
    pub traits: HashMap<Uuid, Entry>,
    pub trait_ids_by_name: HashMap<String, Uuid>,
    pub questions: HashMap<Uuid, Entry>,
    pub plans: HashMap<String, Entry>,
    pub features: HashMap<String, Entry>,
}
impl CatalogSnapshot {
    pub fn trait_name(&self, id: Uuid, name: &mut String, language: Language) {
        if let Some(entry) = self.traits.get(&id) {
            *name = entry.name(language).to_owned();
        }
    }
    pub fn trait_names(&self, names: &mut [String], language: Language) {
        for name in names {
            if let Some(id) = self.trait_ids_by_name.get(name) {
                self.trait_name(*id, name, language);
            }
        }
    }
    pub fn question(&self, id: Uuid, prompt: &mut String, language: Language) {
        if let Some(entry) = self.questions.get(&id) {
            *prompt = entry.name(language).to_owned();
        }
    }
    pub fn answers(&self, answers: &mut [Value], language: Language) {
        for answer in answers {
            let id = answer
                .get("question_id")
                .and_then(Value::as_str)
                .and_then(|id| Uuid::parse_str(id).ok());
            if let Some(entry) = id.and_then(|id| self.questions.get(&id)) {
                // Only the catalogue question is translated. Never alter the user's answer.
                if let Some(question) = answer.get_mut("question") {
                    *question = Value::String(entry.name(language).to_owned());
                }
            }
        }
    }
    pub fn trait_translations(&self, id: Uuid, canonical: &str) -> Translations {
        self.traits.get(&id).map_or_else(
            || BTreeMap::from([(Language::Fr, canonical.to_owned())]),
            |entry| entry.names.clone(),
        )
    }
    pub fn question_translations(&self, id: Uuid, canonical: &str) -> Translations {
        self.questions.get(&id).map_or_else(
            || BTreeMap::from([(Language::Fr, canonical.to_owned())]),
            |entry| entry.names.clone(),
        )
    }
}

#[derive(Serialize)]
pub struct Translated<T> {
    #[serde(flatten)]
    pub value: T,
    pub translations: Translations,
}
