//! Read editor reflection metadata without assuming that shipping builds retain it.
use crate::{
    containers::{FName, FScriptMap, FString},
    mem::{Ctx, Ptr},
    objects::FChunkedFixedUObjectArray,
};
use anyhow::{Result, ensure};
use jmap::{ObjectType, Property, PropertyType, ReflectionMetadataInfo};
use std::collections::{BTreeMap, HashMap};

type Map = BTreeMap<String, String>;

fn member(ctx: &Ctx, ty: &str, name: &str) -> Option<usize> {
    ctx.structs
        .get(ty)?
        .members
        .iter()
        .find(|m| m.name == name)
        .map(|m| m.offset as usize)
}

// Limit traversal/allocation when a dump is truncated, a process mutates, or build
// layout flags are wrong. Metadata must never prevent the normal reflection dump.
async fn elements(map: &Ptr<FScriptMap>, stride: usize) -> Result<Vec<Ptr<u8>>> {
    let sparse = map.pairs().elements();
    let count = sparse.get_max_index().await?;
    ensure!(
        count <= 1_000_000 && stride > 0 && stride <= 1024,
        "invalid metadata map dimensions"
    );
    let mut result = Vec::new();
    for i in 0..count {
        if sparse.is_valid_index(i).await? {
            result.push(sparse.get_data(i, stride).await?);
        }
    }
    Ok(result)
}

async fn read_map(map: &Ptr<FScriptMap>) -> Result<Map> {
    let ctx = map.ctx();
    let stride = ctx.get_struct("FMetadataValueElement").size as usize;
    let value_offset = ctx.struct_member("FMetadataValuePair", "Value");
    let mut result = Map::new();
    for pair in elements(map, stride).await? {
        let key = pair.cast::<FName>();
        // Metadata keys should retain their display spelling in case-preserving
        // builds, independently of the pool's comparison spelling.
        let name = if let Some(offset) = member(ctx, "FName", "DisplayIndex") {
            let index = key.byte_offset(offset).cast::<u32>().read().await?;
            crate::containers::resolve_fname(ctx, index, key.number().read().await?).await?
        } else {
            key.read().await?
        };
        let string = pair.byte_offset(value_offset).cast::<FString>();
        ensure!(
            string
                .cast::<crate::containers::TArray<u16>>()
                .len()
                .await?
                <= 1_048_576,
            "metadata string too large"
        );
        result.insert(name, string.read().await?);
    }
    Ok(result)
}

async fn object_maps(ctx: &Ctx, address: u64) -> Result<HashMap<u64, Map>> {
    let map = Ptr::<FScriptMap>::new(
        address + ctx.struct_member("UMetaData", "ObjectMetaDataMap") as u64,
        ctx.clone(),
    )?;
    let stride = ctx.get_struct("FMetadataObjectElement").size as usize;
    let value_offset = ctx.struct_member("FMetadataObjectPair", "Value");
    let array = Ptr::<FChunkedFixedUObjectArray>::new(
        ctx.uobjectarray + ctx.struct_member("FUObjectArray", "ObjObjects") as u64,
        ctx.clone(),
    )?;
    let count = array.num_elements().read().await?;
    let mut result = HashMap::new();
    for pair in elements(&map, stride).await? {
        let index = pair.cast::<i32>().read().await?;
        let serial = pair.byte_offset(4).cast::<i32>().read().await?;
        if index < 0 || index >= count || serial <= 0 {
            continue;
        }
        let item = array.read_item_ptr(index as usize).await?;
        if item
            .byte_offset(ctx.struct_member("FUObjectItem", "SerialNumber"))
            .cast::<i32>()
            .read()
            .await?
            != serial
        {
            continue;
        }
        if let Some(object) = item.object().read().await? {
            result.insert(
                object.address(),
                read_map(&pair.byte_offset(value_offset).cast()).await?,
            );
        }
    }
    Ok(result)
}

async fn property(
    prop: &mut Property,
    ctx: &Ctx,
    maps: &HashMap<u64, Map>,
    stats: &mut ReflectionMetadataInfo,
) {
    if ctx.ue_version() >= (4, 25) {
        if let Some(offset) = member(ctx, "FField", "MetaDataMap") {
            let result = async {
                let ptr = Ptr::<Option<Ptr<FScriptMap>>>::new(
                    prop.address.0 + offset as u64,
                    ctx.clone(),
                )?;
                Ok::<_, anyhow::Error>(match ptr.read().await? {
                    Some(map) => read_map(&map).await?,
                    None => Map::new(),
                })
            }
            .await;
            match result {
                Ok(map) => prop.metadata = map,
                Err(error) => {
                    stats.read_errors += 1;
                    if stats.read_errors <= 10 {
                        crate::warn!(
                            "Metadata unavailable for property {} at {}: {error:#}",
                            prop.name,
                            prop.address
                        );
                    }
                }
            }
        }
    } else if let Some(map) = maps.get(&prop.address.0) {
        prop.metadata = map.clone();
    }
    if !prop.metadata.is_empty() {
        stats.property_maps += 1;
    }
    match &mut prop.r#type {
        PropertyType::Array { inner } | PropertyType::Optional { inner } => {
            Box::pin(property(inner, ctx, maps, stats)).await
        }
        PropertyType::Set { key_prop } => Box::pin(property(key_prop, ctx, maps, stats)).await,
        PropertyType::Enum { container, .. } => {
            Box::pin(property(container, ctx, maps, stats)).await
        }
        PropertyType::Map {
            key_prop,
            value_prop,
        } => {
            Box::pin(property(key_prop, ctx, maps, stats)).await;
            Box::pin(property(value_prop, ctx, maps, stats)).await;
        }
        _ => {}
    }
}

pub async fn capture(
    ctx: &Ctx,
    objects: &mut BTreeMap<String, ObjectType>,
) -> ReflectionMetadataInfo {
    let mut stats = ReflectionMetadataInfo {
        field_metadata_layout: member(ctx, "FField", "MetaDataMap").is_some(),
        // UE 5.6 replaced package UMetaData storage; do not interpret it as the
        // pre-5.6 weak-object map. Older pre-4.20 object arrays also differ.
        package_metadata_layout: ctx.ue_version() >= (4, 20)
            && ctx.ue_version() < (5, 6)
            && member(ctx, "UMetaData", "ObjectMetaDataMap").is_some(),
        ..Default::default()
    };
    if ![
        "FMetadataValueElement",
        "FMetadataValuePair",
        "FMetadataObjectElement",
        "FMetadataObjectPair",
    ]
    .iter()
    .all(|name| ctx.structs.contains_key(*name))
    {
        stats.read_errors = 1;
        crate::warn!(
            "Metadata layouts missing from --struct-info. Regenerate the layout file to capture metadata; normal reflection is still dumped."
        );
        return stats;
    }
    let mut maps = HashMap::new();
    if stats.package_metadata_layout {
        for obj in objects.values().map(ObjectType::get_object) {
            if obj.class != "/Script/CoreUObject.MetaData"
                || obj.object_flags.intersects(
                    jmap::EObjectFlags::RF_ClassDefaultObject
                        | jmap::EObjectFlags::RF_ArchetypeObject,
                )
            {
                continue;
            }
            match object_maps(ctx, obj.address.0).await {
                Ok(values) => maps.extend(values),
                Err(error) => {
                    stats.read_errors += 1;
                    crate::warn!("Metadata unavailable at {}: {error:#}", obj.address);
                }
            }
            if let Some(offset) = member(ctx, "UMetaData", "RootMetaDataMap")
                && let Some(package) = obj.outer.as_ref().and_then(|path| objects.get(path))
            {
                let root = Ptr::<FScriptMap>::new(obj.address.0 + offset as u64, ctx.clone());
                match root {
                    Ok(root) => match read_map(&root).await {
                        Ok(values) => {
                            maps.insert(package.get_object().address.0, values);
                        }
                        Err(error) => {
                            stats.read_errors += 1;
                            crate::warn!(
                                "Package metadata unavailable at {}: {error:#}",
                                obj.address
                            );
                        }
                    },
                    Err(error) => {
                        stats.read_errors += 1;
                        crate::warn!("Package metadata pointer unavailable: {error:#}");
                    }
                }
            }
        }
    }
    for obj in objects.values_mut() {
        let base = match obj {
            ObjectType::Object(o) => o,
            ObjectType::Package(o) => &mut o.object,
            ObjectType::Enum(o) => &mut o.object,
            ObjectType::Class(o) => &mut o.r#struct.object,
            ObjectType::ScriptStruct(o) => &mut o.r#struct.object,
            ObjectType::Function(o) => &mut o.r#struct.object,
        };
        if let Some(map) = maps.get(&base.address.0) {
            base.metadata = map.clone();
        }
        if !base.metadata.is_empty() {
            stats.object_maps += 1;
        }
        if let ObjectType::Enum(e) = obj {
            // UEnum stores entry metadata as "ShortEntryName.Key". Preserve
            // aliases by name, never by numeric value or ordinal position.
            let raw = std::mem::take(&mut e.object.metadata);
            for (key, value) in raw {
                if let Some((name, suffix)) = e.names.iter().find_map(|(name, _)| {
                    // Match UEnum::GetNameStringByIndex: regular enums keep
                    // the name; other forms strip only the first namespace.
                    let entry = if matches!(e.cpp_form, jmap::ECppForm::Regular) {
                        name.as_str()
                    } else {
                        name.split_once("::")
                            .map(|(_, entry)| entry)
                            .unwrap_or(name)
                    };
                    let prefix = format!("{entry}.");
                    key.get(..prefix.len())
                        .filter(|head| head.eq_ignore_ascii_case(&prefix))
                        .map(|_| (name.clone(), key[prefix.len()..].to_owned()))
                }) {
                    e.entry_metadata
                        .entry(name)
                        .or_default()
                        .insert(suffix, value);
                } else {
                    e.object.metadata.insert(key, value);
                }
            }
            stats.enum_entry_maps += e.entry_metadata.len();
        }
        let props = match obj {
            ObjectType::Class(o) => Some(&mut o.r#struct.properties),
            ObjectType::ScriptStruct(o) => Some(&mut o.r#struct.properties),
            ObjectType::Function(o) => Some(&mut o.r#struct.properties),
            _ => None,
        };
        if let Some(props) = props {
            for p in props {
                property(p, ctx, &maps, &mut stats).await;
            }
        }
    }
    crate::info!(
        "Reflection metadata: {} property maps, {} object maps, {} enum entries, {} read errors",
        stats.property_maps,
        stats.object_maps,
        stats.enum_entry_maps,
        stats.read_errors
    );
    if !stats.package_metadata_layout && ctx.ue_version() >= (5, 6) {
        crate::warn!(
            "Package enum/class metadata capture is not yet supported for UE 5.6+ storage; available FField metadata is still captured."
        );
    }
    if stats.property_maps == 0 && stats.object_maps == 0 {
        crate::warn!(
            "No reflection metadata recovered. Shipping/cooked builds may strip editor metadata; use --editor only for an actual editor build. No bitmask associations were inferred."
        );
    }
    stats
}
