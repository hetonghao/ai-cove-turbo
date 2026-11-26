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
    let escaped = std::str::from_utf8(&raw)?.replace("data:", r"\u0064\u0061\u0074\u0061\u003a");
    let (b, warm) = processor
        .prepare(Bytes::from(escaped), "a".into(), true)
        .await;
    assert!(a == b, "JSON escaping changed the processed output");
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
    let before = processor.usage();
    for _ in 0..2 {
        let (unchanged, err) = processor.prepare(bad.clone(), "a".into(), true).await;
        assert!(unchanged == bad, "invalid image changed original JSON");
        assert!(err.ok_or("missing failure")?.fallback);
    }
    assert_eq!(processor.usage(), before);
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
#[tokio::test]
async fn lossless_png_shrinks_a_screenshot_without_losing_its_color_or_orientation()
-> Result<(), Box<dyn std::error::Error>> {
    // Display P3, sRGB transfer, RGB matrix, full range (cICP chunk including its CRC).
    const COLOR: &[u8] = b"\0\0\0\x04cICP\x0c\x0d\0\x01\x6e\x03\xe3\xef";
    const EXIF: &[u8] = b"II\x2a\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0\x06\0\0\0\0\0\0\0";
    let pixels = image::RgbaImage::from_fn(128, 64, |x, y| {
        image::Rgba(if (x / 8 + y / 16) % 2 == 0 {
            [224, 81, 23, 255]
        } else {
            [38, 83, 170, 0]
        })
    });
    let mut raw = Vec::new();
    let mut encoder = PngEncoder::new_with_quality(
        &mut raw,
        CompressionType::Uncompressed,
        FilterType::NoFilter,
    );
    encoder.set_exif_metadata(EXIF.to_vec())?;
    encoder.write_image(pixels.as_raw(), 128, 64, image::ExtendedColorType::Rgba8)?;
    raw.splice(33..33, COLOR.iter().copied()); // Immediately after IHDR.
    let url = format!("data:image/png;base64,{}", STANDARD.encode(&raw));
    let body = Bytes::from(serde_json::to_vec(
        &json!({"input":[{"role":"user","content":[
            {"type":"input_image", "image_url":url}
        ]}]}),
    )?);

    let processor = Arc::new(Processor::default());
    for webp in [false, true] {
        let (out, report) = processor.prepare(body.clone(), "scope".into(), webp).await;
        let report = report.ok_or("missing report")?;
        assert_eq!(report.marker().as_deref(), Some("v1/changed/1"));
        assert!(report.after < raw.len());
        let obj: Value = serde_json::from_slice(&out)?;
        let encoded = obj
            .pointer("/input/0/content/0/image_url")
            .and_then(Value::as_str)
            .and_then(|url| url.strip_prefix("data:image/png;base64,"))
            .ok_or("expected the original PNG format")?;
        let png = STANDARD.decode(encoded)?;
        assert_eq!(png.len(), report.after);
        assert_eq!(image::load_from_memory(&png)?.into_rgba8(), pixels);
        let mut decoder = image::codecs::png::PngDecoder::new(std::io::Cursor::new(&png))?;
        assert_eq!(
            image::ImageDecoder::exif_metadata(&mut decoder)?.as_deref(),
            Some(EXIF)
        );
        assert!(png.windows(COLOR.len()).any(|chunk| chunk == COLOR));

        let (cached, warm) = processor.prepare(body.clone(), "scope".into(), webp).await;
        assert_eq!(cached, out);
        assert_eq!(warm.ok_or("missing cached report")?.hits, 1);
    }
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
