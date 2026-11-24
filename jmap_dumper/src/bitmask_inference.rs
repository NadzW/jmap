//! Optional editor metadata heuristics. Never change storage, flags, enum numbers,
//! or defaults. Captured metadata and inferred metadata have separate provenance.
use jmap::{
    BitmaskInferenceInfo, InferredBitmask, Jmap, ObjectType, Property, PropertyType, SkippedBitmask,
};
use std::collections::{BTreeMap, BTreeSet};

type Metadata = BTreeMap<String, String>;

fn meta<'a>(map: &'a Metadata, key: &str) -> Option<&'a str> {
    map.iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(key))
        .map(|(_, v)| v.as_str())
}

fn tokens(name: &str) -> Vec<String> {
    let chars: Vec<char> = name.chars().collect();
    let mut result = Vec::new();
    let mut word = String::new();
    for (i, &c) in chars.iter().enumerate() {
        let boundary = c.is_uppercase()
            && i > 0
            && (chars[i - 1].is_lowercase()
                || (chars[i - 1].is_uppercase()
                    && chars.get(i + 1).is_some_and(|next| next.is_lowercase())));
        if (!c.is_alphanumeric() || boundary) && !word.is_empty() {
            result.push(std::mem::take(&mut word));
        }
        if c.is_alphanumeric() {
            word.extend(c.to_lowercase());
        }
    }
    if !word.is_empty() {
        result.push(word);
    }
    for token in &mut result {
        if token == "flags" {
            *token = "flag".into();
        }
        if token == "types" {
            *token = "type".into();
        }
    }
    result
}

fn short_name(path: &str) -> &str {
    path.rsplit(['.', ':']).next().unwrap_or(path)
}
fn package(path: &str) -> &str {
    path.split('.').next().unwrap_or(path)
}
fn enum_base(name: &str) -> &str {
    if name.starts_with('E')
        && name
            .chars()
            .nth(1)
            .is_some_and(|c| c.is_uppercase() || c == '_')
    {
        &name[1..]
    } else {
        name
    }
}

fn sentinel(name: &str, enum_name: &str) -> bool {
    let leaf = name
        .rsplit("::")
        .next()
        .unwrap_or(name)
        .to_ascii_lowercase();
    leaf.ends_with("_max")
        || matches!(leaf.as_str(), "max" | "count" | "num")
        || leaf == format!("max{}", enum_base(enum_name).to_ascii_lowercase())
        || leaf == format!("num{}", enum_base(enum_name).to_ascii_lowercase())
}

#[derive(Clone, Copy)]
enum Mode {
    Indices,
    Masks,
}
impl Mode {
    fn name(self) -> &'static str {
        match self {
            Self::Indices => "bit_indices",
            Self::Masks => "mask_values",
        }
    }
    fn metadata(self) -> &'static str {
        match self {
            Self::Indices => "false",
            Self::Masks => "true",
        }
    }
}

fn mode(e: &jmap::Enum, name: &str) -> Result<(Mode, String), String> {
    if let Some(value) = meta(&e.object.metadata, "UseEnumValuesAsMaskValuesInEditor") {
        return match value.to_ascii_lowercase().as_str() {
            "true" | "1" => Ok((Mode::Masks, "explicit enum mask-mode metadata".into())),
            "false" | "0" | "" => Ok((Mode::Indices, "explicit enum index-mode metadata".into())),
            _ => Err("enum has unrecognized mask-mode metadata".into()),
        };
    }
    let entries: Vec<_> = e
        .names
        .iter()
        .filter(|(entry, _)| {
            !sentinel(entry, name)
                && !e
                    .entry_metadata
                    .get(entry)
                    .is_some_and(|m| meta(m, "Hidden").is_some())
        })
        .collect();
    let values: BTreeSet<i64> = entries.iter().map(|(_, value)| *value).collect();
    let zero_is_none = entries.iter().any(|(entry, value)| {
        *value == 0
            && matches!(
                entry
                    .rsplit("::")
                    .next()
                    .unwrap_or(entry)
                    .to_ascii_lowercase()
                    .as_str(),
                "none" | "noflags" | "noneflags" | "invalid" | "unset" | "disabled"
            )
    });
    if values.len() >= 3
        && values.len() <= 32
        && !zero_is_none
        && values.iter().copied().eq(0..values.len() as i64)
    {
        return Ok((
            Mode::Indices,
            "dense zero-based values with a meaningful zero entry".into(),
        ));
    }
    let positives: Vec<_> = values.iter().copied().filter(|v| *v != 0).collect();
    let flags_enum = tokens(name).contains(&"flag".to_string())
        || e.enum_flags
            .is_some_and(|f| f.contains(jmap::EEnumFlags::Flags));
    if positives.len() >= 2
        && values.iter().all(|v| *v >= 0 && *v <= i32::MAX as i64)
        && positives.iter().all(|v| (*v as u64).is_power_of_two())
        && (positives.iter().max().is_some_and(|v| *v >= 4)
            || (!values.contains(&0) && flags_enum))
    {
        return Ok((
            Mode::Masks,
            "power-of-two values consistent with mask values".into(),
        ));
    }
    Err(
        "enum values do not establish a sufficiently clear bit-index or mask-value convention"
            .into(),
    )
}

struct Candidate {
    path: String,
    words: Vec<String>,
    flags_name: bool,
    mode: Result<(Mode, String), String>,
}

fn score(owner: &str, property: &str, candidate: &Candidate) -> Option<i32> {
    let owner_words = tokens(short_name(owner));
    let base: Vec<_> = tokens(property)
        .into_iter()
        .filter(|w| w != "flag")
        .collect();
    let mut variants = vec![(base.clone(), 0)];
    if base
        .first()
        .is_some_and(|w| matches!(w.as_str(), "allowed" | "required" | "disabled" | "enabled"))
    {
        variants.push((base[1..].to_vec(), 30));
    }
    for (words, penalty) in variants.clone() {
        if words.last().is_some_and(|w| w == "type") && words.len() > 1 {
            variants.push((words[..words.len() - 1].to_vec(), penalty + 100));
        }
    }
    let mut enums = vec![(candidate.words.clone(), 0)];
    if candidate.words.last().is_some_and(|w| w == "type") {
        enums.push((candidate.words[..candidate.words.len() - 1].to_vec(), 80));
    }
    let same_package = package(owner) == package(&candidate.path);
    let mut best = None;
    for (enum_words, enum_penalty) in &enums {
        for (words, penalty) in &variants {
            let value = if words.is_empty() {
                if !same_package || !candidate.flags_name || *enum_words != owner_words {
                    continue;
                }
                1500 - enum_penalty
            } else {
                if words.len() == 1 && words[0].len() < 5 && !candidate.flags_name {
                    continue;
                }
                if !enum_words.ends_with(words)
                    || (!same_package && enum_words.len() != words.len())
                {
                    continue;
                }
                let prefix = &enum_words[..enum_words.len() - words.len()];
                let context = prefix.iter().filter(|w| owner_words.contains(w)).count() as i32;
                (if same_package { 1000 } else { 0 }) + 400 + words.len() as i32 * 30 + context * 20
                    - penalty
                    - enum_penalty
            };
            best = Some(best.map_or(value, |old: i32| old.max(value)));
        }
    }
    best
}

fn properties_mut<F: FnMut(&mut Property, &str, &str)>(
    owner: &str,
    path: &str,
    props: &mut [Property],
    f: &mut F,
) {
    for prop in props {
        let full = if path.is_empty() {
            format!("{owner}:{}", prop.name)
        } else {
            format!("{path}.{}", prop.name)
        };
        f(prop, owner, &full);
        match &mut prop.r#type {
            PropertyType::Array { inner } | PropertyType::Optional { inner } => {
                properties_mut(owner, &full, std::slice::from_mut(inner.as_mut()), f)
            }
            PropertyType::Set { key_prop } => {
                properties_mut(owner, &full, std::slice::from_mut(key_prop.as_mut()), f)
            }
            PropertyType::Map {
                key_prop,
                value_prop,
            } => {
                properties_mut(owner, &full, std::slice::from_mut(key_prop.as_mut()), f);
                properties_mut(owner, &full, std::slice::from_mut(value_prop.as_mut()), f);
            }
            _ => {}
        }
    }
}

fn each_property<F: FnMut(&mut Property, &str, &str)>(data: &mut Jmap, mut f: F) {
    for (path, obj) in &mut data.objects {
        if !path.starts_with("/Script/") {
            continue;
        }
        let props = match obj {
            ObjectType::Class(c) => &mut c.r#struct.properties,
            ObjectType::ScriptStruct(s) => &mut s.r#struct.properties,
            _ => continue, // Function parameters are not editable instance fields.
        };
        properties_mut(path, "", props, &mut f);
    }
}

pub fn infer_bitmasks(data: &mut Jmap) {
    // Re-running inference removes only annotations previously marked as ours.
    // This keeps the operation idempotent and allows changed enums to be reviewed.
    each_property(data, |p, _, _| {
        if p.metadata.remove("JmapInferredBitmask").is_some() {
            p.metadata.remove("Bitmask");
            p.metadata.remove("BitmaskEnum");
        }
    });
    for obj in data.objects.values_mut() {
        if let ObjectType::Enum(e) = obj {
            if e.object.metadata.remove("JmapInferredMaskMode").is_some() {
                e.object
                    .metadata
                    .remove("UseEnumValuesAsMaskValuesInEditor");
            }
            if e.object.metadata.remove("JmapInferredBitflags").is_some() {
                e.object.metadata.remove("Bitflags");
            }
            for values in e.entry_metadata.values_mut() {
                if values.remove("JmapInferredHidden").is_some() {
                    values.remove("Hidden");
                }
            }
            e.entry_metadata.retain(|_, map| !map.is_empty());
        }
    }
    let enums: Vec<_> = data
        .objects
        .iter()
        .filter_map(|(path, obj)| {
            if !path.starts_with("/Script/") {
                return None;
            }
            let ObjectType::Enum(e) = obj else {
                return None;
            };
            let name = short_name(path);
            let raw = tokens(enum_base(name));
            Some(Candidate {
                path: path.clone(),
                flags_name: raw.contains(&"flag".to_string()),
                words: raw.into_iter().filter(|w| w != "flag").collect(),
                mode: mode(e, name),
            })
        })
        .collect();
    let mut report = BitmaskInferenceInfo {
        method: "heuristic flag-name and enum-value matching; not captured game metadata".into(),
        ..Default::default()
    };
    let mut modes = BTreeMap::new();
    each_property(data, |p, owner, path| {
        if !matches!(p.r#type, PropertyType::Int) || !tokens(&p.name).contains(&"flag".to_string())
        {
            return;
        }
        report.considered += 1;
        let mut skip = |reason: &str, candidates: Vec<String>| {
            report.skipped.push(SkippedBitmask {
                property: path.into(),
                reason: reason.into(),
                candidates,
            })
        };
        if meta(&p.metadata, "Bitmask").is_some() || meta(&p.metadata, "BitmaskEnum").is_some() {
            skip("explicit metadata retained", vec![]);
            return;
        }
        if !p.flags.contains(jmap::EPropertyFlags::CPF_Edit) {
            skip("property is not editor-editable in source reflection", vec![]);
            return;
        }
        let scored: Vec<_> = enums
            .iter()
            .filter_map(|e| score(owner, &p.name, e).map(|s| (s, e)))
            .collect();
        let Some(best) = scored.iter().map(|(s, _)| *s).max() else {
            skip("no sufficiently strong enum-name match", vec![]);
            return;
        };
        let bests: Vec<_> = scored.into_iter().filter(|(s, _)| *s == best).collect();
        if bests.len() != 1 {
            skip(
                "ambiguous enum-name matches",
                bests.iter().map(|(_, e)| e.path.clone()).collect(),
            );
            return;
        }
        let candidate = bests[0].1;
        let (mode, reason) = match &candidate.mode {
            Ok(result) => result,
            Err(reason) => {
                skip(reason, vec![candidate.path.clone()]);
                return;
            }
        };
        p.metadata.insert("Bitmask".into(), "".into());
        p.metadata
            .insert("BitmaskEnum".into(), candidate.path.clone());
        p.metadata
            .insert("JmapInferredBitmask".into(), "true".into());
        modes.insert(candidate.path.clone(), *mode);
        report.applied.push(InferredBitmask {
            property: path.into(),
            r#enum: candidate.path.clone(),
            mode: mode.name().into(),
            score: best,
            reason: reason.clone(),
        });
        crate::info!(
            "Inferred bitmask: {path} -> {} ({})",
            candidate.path,
            mode.name()
        );
    });
    for (path, mode) in modes {
        let Some(ObjectType::Enum(e)) = data.objects.get_mut(&path) else {
            continue;
        };
        if meta(&e.object.metadata, "Bitflags").is_none() {
            e.object.metadata.insert("Bitflags".into(), "".into());
            e.object
                .metadata
                .insert("JmapInferredBitflags".into(), "true".into());
        }
        if meta(&e.object.metadata, "UseEnumValuesAsMaskValuesInEditor").is_none() {
            e.object.metadata.insert(
                "UseEnumValuesAsMaskValuesInEditor".into(),
                mode.metadata().into(),
            );
            e.object
                .metadata
                .insert("JmapInferredMaskMode".into(), "true".into());
        }
        for (entry, _) in &e.names {
            if sentinel(entry, short_name(&path)) {
                let values = e.entry_metadata.entry(entry.clone()).or_default();
                if meta(values, "Hidden").is_none() {
                    values.insert("Hidden".into(), "true".into());
                    values.insert("JmapInferredHidden".into(), "true".into());
                }
            }
        }
    }
    data.bitmask_inference = Some(report);
}
