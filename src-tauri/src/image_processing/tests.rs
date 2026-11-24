use super::*;
use image::{
    ImageEncoder as _,
    codecs::png::{CompressionType, FilterType, PngEncoder},
};
use serde_json::json;
fn fixture() -> Result<Bytes, Box<dyn std::error::Error>> {
    let mut png = Vec::new();
    let pixels: Vec<u8> = (0..256u32 * 128)
        .flat_map(|i| [u8::try_from((i / 256) % 3 * 92).unwrap_or(0), 77, 133])
        .collect();
    PngEncoder::new_with_quality(&mut png, CompressionType::Fast, FilterType::NoFilter)
        .write_image(&pixels, 256, 128, image::ExtendedColorType::Rgb8)?;
    let url = format!("data:image/png;base64,{}", STANDARD.encode(&png));
    Ok(Bytes::from(serde_json::to_vec(
        &json!({"input":[{"role":"user","content":[{"type":"input_image","image_url":url,"detail":"high"}]},{"type":"function_call_output","call_id":"call1","output":[{"type":"input_image","image_url":url}]}]}),
    )?))
}
#[tokio::test]
async fn client_images_reuse_results_across_turns_and_roll_back_as_one_request()
-> Result<(), Box<dyn std::error::Error>> {
    let processor = Arc::new(Processor::default());
    let raw = fixture()?;
    let (a, first) = processor.prepare(raw.clone(), "a".into(), true).await;
    let first = first.ok_or("missing report")?;
    assert_eq!(first.marker().as_deref(), Some("v1/changed/2"));
    assert_eq!(first.hits, 1);
    assert!(a.len() < raw.len());
    let obj: Value = serde_json::from_slice(&a)?;
    assert_eq!(
        obj.pointer("/input/0/content/0/detail"),
        Some(&json!("high"))
    );
    let output = obj
        .pointer("/input/0/content/0/image_url")
        .and_then(Value::as_str)
        .ok_or("missing image")?;
    let (_, encoded) = output.split_once(',').ok_or("invalid data URL")?;
    let decoded = image::load_from_memory(&STANDARD.decode(encoded)?)?.to_rgb8();
    assert_eq!(decoded.get_pixel(0, 2).0, [184, 77, 133]);
    let (b, warm) = processor.prepare(raw.clone(), "a".into(), true).await;
    assert_eq!(a, b);
    assert_eq!(warm.ok_or("missing warm")?.hits, 2);
    let (_, isolated) = processor.prepare(raw.clone(), "b".into(), true).await;
    assert_eq!(isolated.ok_or("missing isolated")?.hits, 1);
    processor.configure(Settings {
        min_bytes: 20_480_000,
        ..Settings::default()
    });
    let (same, kept) = processor.prepare(raw.clone(), "a".into(), true).await;
    assert_eq!(raw, same);
    assert_eq!(
        kept.ok_or("missing kept")?.marker().as_deref(),
        Some("v1/kept")
    );
    processor.configure(Settings::default());
    let mut broken: Value = serde_json::from_slice(&raw)?;
    broken["input"][1]["output"][0]["image_url"] = json!("data:image/png;base64,??");
    let bad = Bytes::from(serde_json::to_vec(&broken)?);
    let (unchanged, err) = processor.prepare(bad.clone(), "a".into(), true).await;
    assert_eq!(unchanged, bad);
    assert!(err.ok_or("missing failure")?.fallback);
    let lossy = Settings {
        mode: Mode::Lossy,
        format: Format::Png,
        max_side: 32,
        min_bytes: 0,
        ..Settings::default()
    };
    // Input stays below the high threshold and fits the larger max side. Changing only the threshold must invalidate a cached keep.
    processor.configure(Settings {
        mode: Mode::Lossy,
        min_bytes: 20_480_000,
        max_side: 2048,
        ..Settings::default()
    });
    let (_, high) = processor.prepare(raw.clone(), "c".into(), true).await;
    assert_eq!(high.ok_or("missing high")?.changed, 0);
    processor.configure(Settings {
        mode: Mode::Lossy,
        min_bytes: 0,
        max_side: 2048,
        ..Settings::default()
    });
    let (_, low) = processor.prepare(raw.clone(), "c".into(), true).await;
    let low = low.ok_or("missing low")?;
    assert_ne!(
        low.details.first().ok_or("missing detail")?.reason,
        "threshold"
    );
    assert_eq!(low.hits, 1);
    processor.configure(lossy);
    let (_, scaled) = processor.prepare(raw.clone(), "c".into(), true).await;
    assert_eq!(scaled.ok_or("missing resized")?.changed, 2);
    processor.clear();
    assert_eq!(processor.usage(), (0, 0));
    Ok(())
}
#[test]
fn metadata_only_uses_own_decision_and_never_survives_the_http_fallback()
-> Result<(), Box<dyn std::error::Error>> {
    let raw=br#"{"type":"response.create","client_metadata":{"x-ai-cove-image-processing":"v1/changed/200","other":"kept"}}"#.to_vec();
    let clean = ws_metadata(raw, None);
    let obj: Value = serde_json::from_slice(&clean)?;
    assert_eq!(obj["client_metadata"], json!({"other":"kept"}));
    let tagged: Value = serde_json::from_slice(&ws_metadata(clean, Some("v1/changed/2")))?;
    assert_eq!(tagged["client_metadata"][HEADER], "v1/changed/2");
    Ok(())
}
