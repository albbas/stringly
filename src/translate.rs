use std::{collections::BTreeMap, path::Path, sync::OnceLock};

use regex::{Captures, Regex};
use serde::Deserialize;
use serde_json::json;

use crate::{PathNode, StringData, StringMap};

const GOOGLE_TRANSLATE_URL: &str = "https://translation.googleapis.com/language/translate/v2";

#[derive(Debug, Clone, Deserialize)]
struct TranslateResponse {
    data: TranslateData,
}

#[derive(Debug, Clone, Deserialize)]
struct TranslateData {
    translations: Vec<TranslateItem>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TranslateItem {
    translated_text: String,
}

pub struct KeyedString {
    pub key: String,
    pub value: String,
}

#[derive(Debug)]
pub struct KeyedTranslation<'a> {
    pub key: &'a str,
    pub source: &'a str,
    pub target: String,
}

async fn translate<'a>(
    api_key: &str,
    segments: &'a [KeyedString],
    source_language: &str,
    target_language: &str,
) -> Result<Vec<KeyedTranslation<'a>>, reqwest::Error> {
    let client = reqwest::Client::builder().build()?;
    let mut translated = vec![];

    for q in segments.chunks(128) {
        let response = client
            .post(GOOGLE_TRANSLATE_URL)
            .query(&[("key", &api_key)])
            .json(&json!({
                "q": q.iter().map(|s| &s.value).collect::<Vec<_>>(),
                "source": source_language,
                "target": target_language,
            }))
            .send()
            .await?
            .error_for_status()?;

        let response: TranslateResponse = response.json().await?;
        for (target, KeyedString { key, value: source }) in
            response.data.translations.into_iter().zip(q)
        {
            translated.push(KeyedTranslation {
                key,
                source,
                target: target.translated_text,
            });
        }
    }

    Ok(translated)
}

static TO_HTML_REGEX: std::sync::OnceLock<Regex> = OnceLock::new();
static FROM_HTML_REGEX: std::sync::OnceLock<Regex> = OnceLock::new();

fn convert_to_html(text: &str) -> String {
    let regex = TO_HTML_REGEX.get_or_init(|| Regex::new(r"\{\s*\$(.+?)\s*\}").unwrap());
    regex
        .replace_all(text, |c: &Captures| {
            format!("<a id=\"{}\">{}</a>", &c[1], &c[1])
        })
        .to_string()
}

fn convert_from_html(text: &str) -> String {
    let regex = FROM_HTML_REGEX.get_or_init(|| Regex::new(r#"<a id="(.+?)">.+?</a>"#).unwrap());
    regex
        .replace_all(text, |c: &Captures| format!("{{ ${} }}", &c[1]))
        .to_string()
}

pub async fn process(
    input_xlsx_path: &Path,
    target_language: &str,
    google_api_key: &str,
) -> anyhow::Result<BTreeMap<String, PathNode>> {
    let input = crate::parse_xlsx(input_xlsx_path)?;
    let mut files = BTreeMap::new();

    for (k, v) in input.into_inner().into_iter() {
        let mut subfiles = BTreeMap::new();
        let source_language = &v.base_language;

        let strings = v
            .base_strings()
            .strings
            .iter()
            .map(|(key, x)| {
                let source = convert_to_html(&x.base);
                std::iter::once(KeyedString {
                    key: key.clone(),
                    value: source,
                })
                .chain(x.meta.iter().map(move |x| {
                    let source = convert_to_html(&x.1);

                    KeyedString {
                        key: format!("{key}__{}", x.0),
                        value: source,
                    }
                }))
            })
            .flatten()
            .collect::<Vec<_>>();

        let strings = translate(google_api_key, &strings, source_language, target_language).await?;

        let mut out = StringMap {
            language: target_language.to_string(),
            strings: BTreeMap::new(),
        };

        for x in strings.into_iter() {
            let mut iter = x.key.split("__");
            let base_id = iter.next().unwrap();
            let meta_id = iter.next();

            if let Some(meta_id) = meta_id {
                let map = out.strings.get_mut(base_id).unwrap();
                map.meta
                    .insert(meta_id.to_string(), convert_from_html(&x.target));
            } else {
                out.strings.insert(
                    base_id.to_string(),
                    StringData {
                        base: convert_from_html(&x.target),
                        meta: Default::default(),
                    },
                );
            }
        }

        let x: fluent_syntax::ast::Resource<String> = (&out).try_into().unwrap();
        subfiles.insert(
            format!("{target_language}.flt"),
            PathNode::File(fluent_syntax::serializer::serialize(&x).into_bytes()),
        );

        files.insert(k, PathNode::Directory(subfiles));
    }

    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_e2e() {
        let test = "This is { $var } and also { $upsetting-var }.";
        let html = convert_to_html(test);
        assert_eq!(
            html,
            "This is <a id=\"var\">var</a> and also <a id=\"upsetting-var\">upsetting-var</a>."
        );
        let text = convert_from_html(&html);
        assert_eq!(text, "This is { $var } and also { $upsetting-var }.")
    }
}
